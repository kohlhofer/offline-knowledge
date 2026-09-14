//! A read-only ZIM archive reader.
//!
//! The file is memory-mapped. Directory entries are parsed straight out of the
//! mapping without copying, blobs in uncompressed clusters are handed out as
//! slices of the mapping, and decompressed clusters live in an LRU cache with a
//! byte budget. Every size the file declares is bounds-checked and capped.
//! Format reference: <https://wiki.openzim.org/wiki/ZIM_file_format>.

mod bytes;
mod cluster;
mod dirent;
mod error;
mod header;
pub mod write;

use std::cmp::Ordering;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lru::LruCache;
use md5::{Digest, Md5};
use memmap2::Mmap;

pub use cluster::{Cluster, MAX_DECODED_CLUSTER};
pub use dirent::{DirentKind, DirentRef};
pub use error::{Error, Result};
pub use header::{HEADER_LEN, Header, MAGIC};

use bytes::{cstr_at, slice, u32_at, u64_at};
use cluster::Compression;
use error::corrupt;

/// Content namespace in files using the new namespace scheme.
pub const NS_CONTENT: u8 = b'C';
pub const NS_METADATA: u8 = b'M';
pub const NS_WELL_KNOWN: u8 = b'W';
pub const NS_INDEX: u8 = b'X';
/// Article namespace in old-scheme files.
pub const NS_ARTICLE_LEGACY: u8 = b'A';

/// libzim follows up to 50 redirect hops.
const MAX_REDIRECT_HOPS: usize = 50;
const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;
const LISTING_V1: &[u8] = b"listing/titleOrdered/v1";

pub struct Archive {
    mmap: Arc<Mmap>,
    header: Header,
    mime_types: Vec<String>,
    cache: Mutex<ClusterCache>,
}

struct ClusterCache {
    lru: LruCache<u32, Arc<Cluster>>,
    bytes: usize,
    budget: usize,
}

/// A blob's bytes: either a slice of the mapping or part of a cached cluster.
#[derive(Clone)]
pub struct Blob {
    inner: BlobInner,
}

#[derive(Clone)]
enum BlobInner {
    Mapped { mmap: Arc<Mmap>, start: usize, end: usize },
    Decoded { cluster: Arc<Cluster>, blob: u32 },
}

impl std::ops::Deref for Blob {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.inner {
            BlobInner::Mapped { mmap, start, end } => &mmap[*start..*end],
            BlobInner::Decoded { cluster, blob } => cluster.blob(*blob).expect("blob index validated at construction"),
        }
    }
}

/// One cluster opened for reading every blob in it, as a bulk import does.
pub enum ClusterReader<'a> {
    Decoded(Cluster),
    Mapped { archive: &'a Archive, index: u32, table_pos: u64, width: u64 },
}

impl ClusterReader<'_> {
    pub fn blob(&self, blob: u32) -> Result<&[u8]> {
        match self {
            ClusterReader::Decoded(c) => c.blob(blob).ok_or_else(|| corrupt(format!("blob {blob} out of range"))),
            ClusterReader::Mapped { archive, index, table_pos, width } => {
                let info = cluster::Info { compression: Compression::None, width: *width };
                let (start, end) = cluster::uncompressed_blob_range(&archive.mmap, *index, &info, *table_pos, blob)?;
                Ok(&archive.mmap[start as usize..end as usize])
            }
        }
    }
}

impl Archive {
    /// Opens and validates a ZIM file.
    ///
    /// The file is memory-mapped. It must not be truncated or rewritten in
    /// place while the archive is open: a truncated mapping ends the process
    /// with SIGBUS. Replace a ZIM file by writing a new one and renaming it.
    pub fn open(path: impl AsRef<Path>) -> Result<Archive> {
        let file = File::open(path)?;
        // SAFETY: the mapping is read-only; the contract above covers external writers.
        let mmap = unsafe { Mmap::map(&file)? };
        let header = Header::parse(&mmap)?;
        let mime_types = read_mime_list(&mmap, header.mime_list_pos)?;
        Ok(Archive {
            mmap: Arc::new(mmap),
            header,
            mime_types,
            cache: Mutex::new(ClusterCache { lru: LruCache::unbounded(), bytes: 0, budget: DEFAULT_CACHE_BYTES }),
        })
    }

    /// Sets the byte budget for decompressed clusters kept in memory.
    pub fn set_cache_budget(&self, bytes: usize) {
        let mut cache = self.cache.lock().unwrap();
        cache.budget = bytes;
        cache.evict_to_budget();
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn entry_count(&self) -> u32 {
        self.header.entry_count
    }

    pub fn file_len(&self) -> u64 {
        self.mmap.len() as u64
    }

    pub fn mime_types(&self) -> &[String] {
        &self.mime_types
    }

    /// The MIME type of a content dirent read from this archive.
    pub fn mime_type(&self, dirent: &DirentRef<'_>) -> Option<&str> {
        match dirent.kind {
            DirentKind::Content { mime_index, .. } => self.mime_types.get(mime_index as usize).map(String::as_str),
            _ => None,
        }
    }

    /// The directory entry at `index` in path order.
    pub fn dirent(&self, index: u32) -> Result<DirentRef<'_>> {
        if index >= self.header.entry_count {
            return Err(Error::EntryOutOfRange { index, count: self.header.entry_count });
        }
        let offset = u64_at(&self.mmap, self.header.path_ptr_pos + u64::from(index) * 8, "path pointer")?;
        DirentRef::parse(&self.mmap, offset)
    }

    /// Binary search of the path-ordered directory for `(namespace, path)`.
    pub fn find_by_path(&self, namespace: u8, path: &[u8]) -> Result<Option<u32>> {
        let index = self.lower_bound(namespace, path)?;
        if index < self.header.entry_count {
            let d = self.dirent(index)?;
            if d.namespace == namespace && d.path == path {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// The first index whose `(namespace, path)` is not less than the one given.
    fn lower_bound(&self, namespace: u8, path: &[u8]) -> Result<u32> {
        let (mut lo, mut hi) = (0u32, self.header.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let d = self.dirent(mid)?;
            match (d.namespace, d.path).cmp(&(namespace, path)) {
                Ordering::Less => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Ok(lo)
    }

    /// The index range holding every entry of `namespace`.
    pub fn namespace_range(&self, namespace: u8) -> Result<std::ops::Range<u32>> {
        let start = self.lower_bound(namespace, b"")?;
        let end = match namespace.checked_add(1) {
            Some(next) => self.lower_bound(next, b"")?,
            None => self.header.entry_count,
        };
        Ok(start..end.max(start))
    }

    /// The namespace that holds articles: `C` in new-scheme files, `A` in old ones.
    pub fn article_namespace(&self) -> u8 {
        if self.header.uses_new_namespaces() { NS_CONTENT } else { NS_ARTICLE_LEGACY }
    }

    /// Follows redirects from `index` to the first non-redirect entry.
    pub fn resolve(&self, index: u32) -> Result<u32> {
        let mut current = index;
        for _ in 0..MAX_REDIRECT_HOPS {
            match self.dirent(current)?.kind {
                DirentKind::Redirect { target } => current = target,
                _ => return Ok(current),
            }
        }
        Err(Error::RedirectLoop(index))
    }

    /// The main page, resolved: `W/mainPage` in new files, else the header's index.
    pub fn main_entry(&self) -> Result<Option<u32>> {
        let start = match self.find_by_path(NS_WELL_KNOWN, b"mainPage")? {
            Some(i) => i,
            None => match self.header.main_page {
                Some(i) if i < self.header.entry_count => i,
                _ => return Ok(None),
            },
        };
        self.resolve(start).map(Some)
    }

    /// The content of `index`, following redirects.
    pub fn content(&self, index: u32) -> Result<Blob> {
        let resolved = self.resolve(index)?;
        match self.dirent(resolved)?.kind {
            DirentKind::Content { cluster, blob, .. } => self.blob(cluster, blob),
            _ => Err(Error::NotContent(resolved)),
        }
    }

    pub fn blob(&self, cluster: u32, blob: u32) -> Result<Blob> {
        let (info, table_pos) = self.cluster_start(cluster)?;
        if info.compression == Compression::None {
            let (start, end) = cluster::uncompressed_blob_range(&self.mmap, cluster, &info, table_pos, blob)?;
            return Ok(Blob { inner: BlobInner::Mapped { mmap: Arc::clone(&self.mmap), start: start as usize, end: end as usize } });
        }
        let decoded = self.cached_cluster(cluster, &info, table_pos)?;
        if blob >= decoded.blob_count() {
            return Err(corrupt(format!("blob {blob} out of range in cluster {cluster} ({} blobs)", decoded.blob_count())));
        }
        Ok(Blob { inner: BlobInner::Decoded { cluster: decoded, blob } })
    }

    /// Opens a cluster for reading all of its blobs without touching the cache,
    /// so a bulk reader does not evict an interactive reader's working set.
    pub fn read_cluster(&self, index: u32) -> Result<ClusterReader<'_>> {
        let (info, table_pos) = self.cluster_start(index)?;
        match info.compression {
            Compression::None => Ok(ClusterReader::Mapped { archive: self, index, table_pos, width: info.width }),
            _ => Ok(ClusterReader::Decoded(cluster::decode_compressed(index, &info, &self.mmap[table_pos as usize..])?)),
        }
    }

    fn cached_cluster(&self, index: u32, info: &cluster::Info, table_pos: u64) -> Result<Arc<Cluster>> {
        if let Some(hit) = self.cache.lock().unwrap().lru.get(&index) {
            return Ok(Arc::clone(hit));
        }
        let decoded = Arc::new(cluster::decode_compressed(index, info, &self.mmap[table_pos as usize..])?);
        let mut cache = self.cache.lock().unwrap();
        if let Some(previous) = cache.lru.put(index, Arc::clone(&decoded)) {
            cache.bytes -= previous.memory_size();
        }
        cache.bytes += decoded.memory_size();
        cache.evict_to_budget();
        Ok(decoded)
    }

    /// The cluster's info byte and the file position right after it.
    fn cluster_start(&self, index: u32) -> Result<(cluster::Info, u64)> {
        if index >= self.header.cluster_count {
            return Err(corrupt(format!("cluster {index} out of range ({} clusters)", self.header.cluster_count)));
        }
        let start = u64_at(&self.mmap, self.header.cluster_ptr_pos + u64::from(index) * 8, "cluster pointer")?;
        let byte = *slice(&self.mmap, start, 1, "cluster info byte")?.first().unwrap();
        Ok((cluster::info(index, byte)?, start + 1))
    }

    /// Entry indices of the v0 title pointer list (all entries, by namespace and title).
    pub fn title_index_v0(&self) -> Result<Option<Vec<u32>>> {
        let Some(pos) = self.header.title_ptr_pos else { return Ok(None) };
        let count = self.header.entry_count;
        (0..count)
            .map(|i| {
                let index = u32_at(&self.mmap, pos + u64::from(i) * 4, "title pointer")?;
                if index < count { Ok(index) } else { Err(Error::EntryOutOfRange { index, count }) }
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }

    /// Entry indices of "front articles", sorted by title.
    ///
    /// New files store this list as `X/listing/titleOrdered/v1`: articles and
    /// the redirects the creator marked as front articles. Without it, old-scheme
    /// files return every `A` entry, and new-scheme files every `C` entry that
    /// is an HTML page or a redirect.
    pub fn front_articles(&self) -> Result<Vec<u32>> {
        let count = self.header.entry_count;
        if let Some(index) = self.find_by_path(NS_INDEX, LISTING_V1)? {
            let blob = self.content(index)?;
            if blob.len() % 4 != 0 {
                return Err(corrupt("title listing v1 length is not a multiple of 4"));
            }
            return blob
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .map(|index| if index < count { Ok(index) } else { Err(Error::EntryOutOfRange { index, count }) })
                .collect();
        }
        let all = self.title_index_v0()?.ok_or_else(|| corrupt("file has neither title listing"))?;
        let namespace = self.article_namespace();
        let new_scheme = self.header.uses_new_namespaces();
        let mut out = Vec::new();
        for i in all {
            let d = self.dirent(i)?;
            let keep = d.namespace == namespace
                && (!new_scheme || d.is_redirect() || self.mime_type(&d) == Some("text/html"));
            if keep {
                out.push(i);
            }
        }
        Ok(out)
    }

    /// A metadata value such as `Title`, `Language` or `Date`. Invalid UTF-8 is
    /// replaced with U+FFFD; use [`Archive::content`] for the raw bytes.
    pub fn metadata(&self, name: &str) -> Result<Option<String>> {
        match self.find_by_path(NS_METADATA, name.as_bytes())? {
            Some(i) => Ok(Some(String::from_utf8_lossy(&self.content(i)?).into_owned())),
            None => Ok(None),
        }
    }

    /// Recomputes the MD5 over everything before the checksum and compares.
    /// This detects accidental corruption only: whoever built a hostile file
    /// also computed its checksum. Reads the whole file.
    pub fn verify_checksum(&self) -> Result<bool> {
        let pos = self.header.checksum_pos;
        if pos == 0 {
            return Ok(false);
        }
        let stored = slice(&self.mmap, pos, 16, "checksum")?;
        let body = slice(&self.mmap, 0, pos, "checksummed body")?;
        Ok(Md5::digest(body).as_slice() == stored)
    }
}

impl ClusterCache {
    fn evict_to_budget(&mut self) {
        // Always keep the most recent cluster, even when it alone exceeds the budget.
        while self.bytes > self.budget && self.lru.len() > 1 {
            if let Some((_, evicted)) = self.lru.pop_lru() {
                self.bytes -= evicted.memory_size();
            }
        }
    }
}

fn read_mime_list(data: &[u8], pos: u64) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut at = pos;
    loop {
        let s = cstr_at(data, at, "mime list")?;
        if s.is_empty() {
            return Ok(out);
        }
        if out.len() >= 0xfffd {
            return Err(corrupt("mime list has no terminator"));
        }
        out.push(String::from_utf8_lossy(s).into_owned());
        at += s.len() as u64 + 1;
    }
}
