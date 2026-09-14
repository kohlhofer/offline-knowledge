//! A minimal ZIM writer for tests and fixtures.
//!
//! It writes the new-namespace layout the way libzim lays files out: header,
//! MIME list, clusters, then dirents and the pointer lists, then the MD5
//! checksum. Content goes into clusters of a configurable size; the
//! `X/listing/titleOrdered/v1` listing gets a final cluster of its own,
//! uncompressed unless asked otherwise. `W/mainPage` redirects to the main
//! article. Not a general-purpose creator: no full-text index, no
//! deduplication, and misuse (duplicate paths, redirects to missing paths)
//! panics.

use std::collections::{BTreeMap, BTreeSet};

use md5::{Digest, Md5};

use crate::header::{HEADER_LEN, MAGIC};
use crate::{NS_CONTENT, NS_INDEX, NS_METADATA, NS_WELL_KNOWN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Zstd,
}

enum Payload {
    Blob { mime: String, data: Vec<u8>, front: bool },
    Redirect { target: (u8, String) },
}

struct Pending {
    namespace: u8,
    path: String,
    title: String,
    payload: Payload,
}

pub struct ZimBuilder {
    entries: Vec<Pending>,
    main_path: Option<String>,
    compression: Compression,
    blobs_per_cluster: usize,
    extended_clusters: bool,
    listing_compressed: bool,
}

impl Default for ZimBuilder {
    fn default() -> Self {
        ZimBuilder {
            entries: Vec::new(),
            main_path: None,
            compression: Compression::Zstd,
            blobs_per_cluster: 3,
            extended_clusters: false,
            listing_compressed: false,
        }
    }
}

impl ZimBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    pub fn blobs_per_cluster(mut self, n: usize) -> Self {
        self.blobs_per_cluster = n.max(1);
        self
    }

    /// Writes 64-bit blob offsets inside clusters.
    pub fn extended_clusters(mut self, extended: bool) -> Self {
        self.extended_clusters = extended;
        self
    }

    /// Compresses the final listing cluster with the content compression, so
    /// the last cluster in the file is a compressed one.
    pub fn listing_compressed(mut self, compressed: bool) -> Self {
        self.listing_compressed = compressed;
        self
    }

    /// An HTML article in the content namespace; it appears in the front-article listing.
    pub fn article(self, path: &str, title: &str, html: &str) -> Self {
        self.blob(NS_CONTENT, path, title, "text/html", html.as_bytes(), true)
    }

    /// A non-article resource (stylesheet, image) in the content namespace.
    pub fn resource(self, path: &str, mime: &str, data: &[u8]) -> Self {
        self.blob(NS_CONTENT, path, "", mime, data, false)
    }

    pub fn metadata(self, name: &str, value: &str) -> Self {
        self.blob(NS_METADATA, name, "", "text/plain", value.as_bytes(), false)
    }

    pub fn redirect(mut self, path: &str, title: &str, target_path: &str) -> Self {
        self.entries.push(Pending {
            namespace: NS_CONTENT,
            path: path.into(),
            title: title.into(),
            payload: Payload::Redirect { target: (NS_CONTENT, target_path.into()) },
        });
        self
    }

    pub fn main_page(mut self, path: &str) -> Self {
        self.main_path = Some(path.into());
        self
    }

    fn blob(mut self, namespace: u8, path: &str, title: &str, mime: &str, data: &[u8], front: bool) -> Self {
        self.entries.push(Pending {
            namespace,
            path: path.into(),
            title: title.into(),
            payload: Payload::Blob { mime: mime.into(), data: data.to_vec(), front },
        });
        self
    }

    pub fn build(mut self) -> Vec<u8> {
        if let Some(main) = self.main_path.clone() {
            self.entries.push(Pending {
                namespace: NS_WELL_KNOWN,
                path: "mainPage".into(),
                title: String::new(),
                payload: Payload::Redirect { target: (NS_CONTENT, main) },
            });
        }
        self.entries.push(Pending {
            namespace: NS_INDEX,
            path: "listing/titleOrdered/v1".into(),
            title: String::new(),
            payload: Payload::Blob { mime: "application/octet-stream+zimlisting".into(), data: Vec::new(), front: false },
        });
        self.entries.sort_by(|a, b| (a.namespace, a.path.as_bytes()).cmp(&(b.namespace, b.path.as_bytes())));
        let mut index_of: BTreeMap<(u8, String), u32> = BTreeMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            let previous = index_of.insert((e.namespace, e.path.clone()), i as u32);
            assert!(previous.is_none(), "duplicate entry {}/{}", e.namespace as char, e.path);
        }
        let lookup = |target: &(u8, String)| -> u32 {
            *index_of
                .get(target)
                .unwrap_or_else(|| panic!("redirect to missing entry {}/{}", target.0 as char, target.1))
        };
        let title_of = |e: &Pending| if e.title.is_empty() { e.path.clone() } else { e.title.clone() };

        let mut title_order: Vec<u32> = (0..self.entries.len() as u32).collect();
        title_order.sort_by(|&a, &b| {
            let (ea, eb) = (&self.entries[a as usize], &self.entries[b as usize]);
            (ea.namespace, title_of(ea)).cmp(&(eb.namespace, title_of(eb)))
        });
        let front_articles: BTreeSet<u32> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.payload, Payload::Blob { front: true, .. }))
            .map(|(i, _)| i as u32)
            .collect();
        let listing: Vec<u8> = title_order
            .iter()
            .copied()
            .filter(|&i| {
                let e = &self.entries[i as usize];
                match &e.payload {
                    Payload::Blob { front, .. } => *front,
                    Payload::Redirect { target } => {
                        e.namespace == NS_CONTENT && front_articles.contains(&self.resolve(lookup, target))
                    }
                }
            })
            .flat_map(|i| i.to_le_bytes())
            .collect();
        let listing_index = lookup(&(NS_INDEX, "listing/titleOrdered/v1".into())) as usize;
        if let Payload::Blob { data, .. } = &mut self.entries[listing_index].payload {
            *data = listing;
        }

        let mut mimes: Vec<String> = self
            .entries
            .iter()
            .filter_map(|e| match &e.payload {
                Payload::Blob { mime, .. } => Some(mime.clone()),
                Payload::Redirect { .. } => None,
            })
            .collect();
        mimes.sort();
        mimes.dedup();
        assert!(mimes.len() < 0xfffd, "MIME indices would collide with reserved dirent codes");

        // Content clusters in entry order; the listing last, in its own cluster.
        let mut clusters: Vec<(Vec<Vec<u8>>, bool)> = Vec::new();
        let mut location: Vec<Option<(u32, u32)>> = vec![None; self.entries.len()];
        let compress_content = self.compression == Compression::Zstd;
        for (i, e) in self.entries.iter().enumerate() {
            if i == listing_index {
                continue;
            }
            if let Payload::Blob { data, .. } = &e.payload {
                if clusters.last().is_none_or(|(c, _)| c.len() >= self.blobs_per_cluster) {
                    clusters.push((Vec::new(), compress_content));
                }
                let c = clusters.len() - 1;
                location[i] = Some((c as u32, clusters[c].0.len() as u32));
                clusters[c].0.push(data.clone());
            }
        }
        if let Payload::Blob { data, .. } = &self.entries[listing_index].payload {
            location[listing_index] = Some((clusters.len() as u32, 0));
            clusters.push((vec![data.clone()], self.listing_compressed && compress_content));
        }

        let mut out = vec![0u8; HEADER_LEN as usize];
        let mime_list_pos = out.len() as u64;
        for m in &mimes {
            out.extend_from_slice(m.as_bytes());
            out.push(0);
        }
        out.push(0);

        let mut cluster_offsets = Vec::with_capacity(clusters.len());
        for (blobs, compressed) in &clusters {
            cluster_offsets.push(out.len() as u64);
            out.extend_from_slice(&self.encode_cluster(blobs, *compressed));
        }

        let mut dirent_offsets = Vec::with_capacity(self.entries.len());
        for (i, e) in self.entries.iter().enumerate() {
            dirent_offsets.push(out.len() as u64);
            match &e.payload {
                Payload::Blob { mime, .. } => {
                    let mime_index = mimes.iter().position(|m| m == mime).unwrap() as u16;
                    let (c, b) = location[i].unwrap();
                    out.extend_from_slice(&mime_index.to_le_bytes());
                    out.push(0);
                    out.push(e.namespace);
                    out.extend_from_slice(&0u32.to_le_bytes());
                    out.extend_from_slice(&c.to_le_bytes());
                    out.extend_from_slice(&b.to_le_bytes());
                }
                Payload::Redirect { target } => {
                    out.extend_from_slice(&0xffffu16.to_le_bytes());
                    out.push(0);
                    out.push(e.namespace);
                    out.extend_from_slice(&0u32.to_le_bytes());
                    out.extend_from_slice(&lookup(target).to_le_bytes());
                }
            }
            out.extend_from_slice(e.path.as_bytes());
            out.push(0);
            out.extend_from_slice(e.title.as_bytes());
            out.push(0);
        }

        let path_ptr_pos = out.len() as u64;
        for o in &dirent_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        let title_ptr_pos = out.len() as u64;
        for i in &title_order {
            out.extend_from_slice(&i.to_le_bytes());
        }
        let cluster_ptr_pos = out.len() as u64;
        for o in &cluster_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        let checksum_pos = out.len() as u64;
        let main_page = self
            .main_path
            .as_ref()
            .map(|_| lookup(&(NS_WELL_KNOWN, "mainPage".into())))
            .unwrap_or(u32::MAX);

        let h = &mut out[..HEADER_LEN as usize];
        h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        h[4..6].copy_from_slice(&6u16.to_le_bytes());
        h[6..8].copy_from_slice(&3u16.to_le_bytes());
        h[8..24].copy_from_slice(b"ok-zim-testfile!");
        h[24..28].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        h[28..32].copy_from_slice(&(clusters.len() as u32).to_le_bytes());
        h[32..40].copy_from_slice(&path_ptr_pos.to_le_bytes());
        h[40..48].copy_from_slice(&title_ptr_pos.to_le_bytes());
        h[48..56].copy_from_slice(&cluster_ptr_pos.to_le_bytes());
        h[56..64].copy_from_slice(&mime_list_pos.to_le_bytes());
        h[64..68].copy_from_slice(&main_page.to_le_bytes());
        h[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
        h[72..80].copy_from_slice(&checksum_pos.to_le_bytes());

        let digest = Md5::digest(&out);
        out.extend_from_slice(digest.as_slice());
        out
    }

    fn resolve(&self, lookup: impl Fn(&(u8, String)) -> u32, target: &(u8, String)) -> u32 {
        let mut current = lookup(target);
        for _ in 0..50 {
            match &self.entries[current as usize].payload {
                Payload::Redirect { target } => current = lookup(target),
                Payload::Blob { .. } => return current,
            }
        }
        panic!("redirect loop at {}/{}", target.0 as char, target.1);
    }

    fn encode_cluster(&self, blobs: &[Vec<u8>], compressed: bool) -> Vec<u8> {
        let width = if self.extended_clusters { 8 } else { 4 };
        let mut body = Vec::new();
        let mut offset = ((blobs.len() + 1) * width) as u64;
        let mut offsets = vec![offset];
        for b in blobs {
            offset += b.len() as u64;
            offsets.push(offset);
        }
        for o in offsets {
            if self.extended_clusters {
                body.extend_from_slice(&o.to_le_bytes());
            } else {
                body.extend_from_slice(&u32::try_from(o).expect("cluster over 4 GiB needs extended offsets").to_le_bytes());
            }
        }
        for b in blobs {
            body.extend_from_slice(b);
        }
        let flag = if self.extended_clusters { 0x10 } else { 0 };
        let mut out = Vec::new();
        if compressed {
            out.push(5 | flag);
            out.extend_from_slice(&zstd::stream::encode_all(&body[..], 3).expect("in-memory zstd encode"));
        } else {
            out.push(1 | flag);
            out.extend_from_slice(&body);
        }
        out
    }
}
