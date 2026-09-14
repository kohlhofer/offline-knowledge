//! One pass over a ZIM file that builds everything the reader needs to be fast:
//! the title FST, section-redirect table, inbound-link counts, the article list
//! and the full-text index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ok_zim::{Archive, DirentKind};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::document::{ArticleContext, Link, Target, parse_article};
use crate::fulltext::FullTextWriter;
use crate::stubs::{StubTable, refresh_target};
use crate::titles::{self, TitleEntry};
use crate::{Error, Result};

pub const INDEX_FORMAT: u32 = 1;
const SUMMARY_CHARS: usize = 280;
const MAX_STUB_HOPS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub format: u32,
    pub zim_uuid: String,
    pub zim_bytes: u64,
    pub title: String,
    pub articles: usize,
    pub redirects: usize,
    pub section_redirects: usize,
    pub title_keys: usize,
    pub internal_links: u64,
    pub missing_links: u64,
    pub created_unix: u64,
    pub tool_version: String,
}

#[derive(Debug, Clone)]
pub enum Progress {
    Scanned { candidates: usize, redirects: usize },
    Classified { articles: usize, section_redirects: usize },
    Parsed { done: usize, total: usize },
    Writing(&'static str),
}

#[derive(Debug, Clone)]
pub struct ImportReport {
    pub meta: IndexMeta,
    pub index_dir: PathBuf,
    pub scan: Duration,
    pub parse_and_index: Duration,
    pub finish: Duration,
    pub index_bytes: u64,
}

pub struct ImportOptions {
    /// Memory budget for the full-text writer, split across its threads.
    pub heap_bytes: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions { heap_bytes: 512 * 1024 * 1024 }
    }
}

/// `data/foo.zim` indexes into `data/foo.okx/`.
pub fn index_dir_for(zim: &Path) -> PathBuf {
    zim.with_extension("okx")
}

/// Location of an HTML entry: (cluster, blob, entry), sortable into cluster order.
type Located = (u32, u32, u32);

pub fn import(zim_path: &Path, options: &ImportOptions, progress: &(dyn Fn(Progress) + Sync)) -> Result<ImportReport> {
    let started = Instant::now();
    let archive = Archive::open(zim_path)?;
    let namespace = archive.article_namespace();

    // Every path in the article namespace, resolved through ZIM redirects, so
    // link resolution in the parallel passes is a hash lookup.
    let range = archive.namespace_range(namespace)?;
    let mut path_to_entry: HashMap<Box<str>, u32> = HashMap::with_capacity(range.len());
    for i in range {
        let dirent = archive.dirent(i)?;
        if let Ok(target) = archive.resolve(i) {
            path_to_entry.insert(String::from_utf8_lossy(dirent.path).into(), target);
        }
    }

    let mut candidates: Vec<Located> = Vec::new();
    let mut redirects: Vec<(u32, u32)> = Vec::new(); // (redirect entry, resolved entry)
    for entry in archive.front_articles()? {
        let dirent = archive.dirent(entry)?;
        match dirent.kind {
            DirentKind::Content { cluster, blob, .. } if archive.mime_type(&dirent) == Some("text/html") => {
                candidates.push((cluster, blob, entry));
            }
            DirentKind::Redirect { .. } => {
                let Ok(target) = archive.resolve(entry) else { continue };
                if archive.mime_type(&archive.dirent(target)?) == Some("text/html") {
                    redirects.push((entry, target));
                }
            }
            _ => {}
        }
    }
    candidates.sort_unstable();
    progress(Progress::Scanned { candidates: candidates.len(), redirects: redirects.len() });

    // Pass one: separate real articles from section-redirect stubs.
    let classified: Vec<(Vec<Located>, Vec<(u32, String, Option<String>)>)> = candidates
        .chunk_by(|a, b| a.0 == b.0)
        .collect::<Vec<_>>()
        .par_iter()
        .map(|group| -> Result<_> {
            let cluster = archive.read_cluster(group[0].0)?;
            let (mut real, mut stubs) = (Vec::new(), Vec::new());
            for &(c, blob, entry) in group.iter() {
                let path = archive.dirent(entry)?.path_str();
                match refresh_target(&path, cluster.blob(blob)?) {
                    Some((target_path, fragment)) => stubs.push((entry, target_path, fragment)),
                    None => real.push((c, blob, entry)),
                }
            }
            Ok((real, stubs))
        })
        .collect::<Result<_>>()?;
    let mut articles: Vec<Located> = Vec::new();
    let mut raw_stubs = Vec::new();
    for (real, stubs) in classified {
        articles.extend(real);
        raw_stubs.extend(stubs);
    }
    articles.sort_unstable();
    let stubs = resolve_stubs(raw_stubs, &path_to_entry);
    progress(Progress::Classified { articles: articles.len(), section_redirects: stubs.len() });

    let resolver = |path: &str| -> Option<Target> { stub_aware(path_to_entry.get(path).copied()?, &stubs) };

    // Alternative titles for full-text search: every redirect and stub title,
    // attached to the article it finally lands on.
    let mut title_rows: Vec<(u32, u32)> = articles.iter().map(|&(_, _, e)| (e, e)).collect();
    for &(entry, target) in &redirects {
        if let Some(t) = stub_aware(target, &stubs) {
            title_rows.push((entry, t.entry));
        }
    }
    title_rows.extend(stubs.iter().map(|(stub, article)| (stub, article)));
    let mut alt_titles: HashMap<u32, String> = HashMap::new();
    for &(entry, target) in &title_rows {
        if entry != target {
            let s = alt_titles.entry(target).or_default();
            s.push_str(&archive.dirent(entry)?.title_str());
            s.push('\n');
        }
    }
    let scan = started.elapsed();

    let final_dir = index_dir_for(zim_path);
    let work_dir = final_dir.with_extension("okx.partial");
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir)?;
    }
    std::fs::create_dir_all(&work_dir)?;

    // Pass two: parse and index the real articles, counting inbound links.
    let parse_started = Instant::now();
    let writer = FullTextWriter::create(&work_dir.join("fulltext"), options.heap_bytes)?;
    let inbound: Vec<AtomicU32> = (0..archive.entry_count()).map(|_| AtomicU32::new(0)).collect();
    let done = AtomicUsize::new(0);
    let internal_links = AtomicUsize::new(0);
    let missing_links = AtomicUsize::new(0);
    let total = articles.len();

    articles.chunk_by(|a, b| a.0 == b.0).collect::<Vec<_>>().par_iter().try_for_each(|group| -> Result<()> {
        let cluster = archive.read_cluster(group[0].0)?;
        for &(_, blob, entry) in group.iter() {
            let dirent = archive.dirent(entry)?;
            let (path, title) = (dirent.path_str(), dirent.title_str());
            let html = String::from_utf8_lossy(cluster.blob(blob)?);
            let doc = parse_article(&html, &ArticleContext { entry, path: &path, title: &title }, &resolver);

            let mut targets = Vec::new();
            let mut missing = 0;
            for link in doc.links() {
                match link {
                    Link::Article { entry: t, .. } if *t != entry => targets.push(*t),
                    Link::Missing { .. } => missing += 1,
                    _ => {}
                }
            }
            internal_links.fetch_add(targets.len(), Ordering::Relaxed);
            missing_links.fetch_add(missing, Ordering::Relaxed);
            targets.sort_unstable();
            targets.dedup();
            for t in targets {
                if let Some(counter) = inbound.get(t as usize) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }

            let alt = alt_titles.get(&entry).map(String::as_str).unwrap_or("");
            writer.add(entry, &doc.title, alt, &doc.plain_text(), &doc.summary(SUMMARY_CHARS))?;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 1000 == 0 || n == total {
                progress(Progress::Parsed { done: n, total });
            }
        }
        Ok(())
    })?;
    let parse_and_index = parse_started.elapsed();

    let finish_started = Instant::now();
    let inbound: Vec<u32> = inbound.into_iter().map(AtomicU32::into_inner).collect();

    progress(Progress::Writing("title index"));
    let titled: Vec<(String, u32, u32)> = title_rows
        .iter()
        .map(|&(entry, target)| Ok((archive.dirent(entry)?.title_str(), entry, target)))
        .collect::<Result<_>>()?;
    let title_keys = titles::build(
        &work_dir.join("titles.fst"),
        titled.iter().map(|(title, entry, target)| TitleEntry {
            title,
            entry: *entry,
            target: *target,
            score: inbound[*target as usize],
        }),
    )?;

    progress(Progress::Writing("link counts, article list, section redirects"));
    write_u32s(&work_dir.join("inbound.u32"), &inbound)?;
    let mut article_entries: Vec<u32> = articles.iter().map(|&(_, _, e)| e).collect();
    article_entries.sort_unstable();
    write_u32s(&work_dir.join("articles.u32"), &article_entries)?;
    stubs.write(&work_dir.join("stubs.bin"))?;

    progress(Progress::Writing("full-text index"));
    writer.finish()?;

    let meta = IndexMeta {
        format: INDEX_FORMAT,
        zim_uuid: archive.header().uuid_hex(),
        zim_bytes: archive.file_len(),
        title: archive.metadata("Title")?.unwrap_or_default(),
        articles: articles.len(),
        redirects: redirects.len(),
        section_redirects: stubs.len(),
        title_keys,
        internal_links: internal_links.into_inner() as u64,
        missing_links: missing_links.into_inner() as u64,
        created_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    std::fs::write(work_dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;

    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&work_dir, &final_dir)?;
    let index_bytes = dir_size(&final_dir)?;
    Ok(ImportReport { meta, index_dir: final_dir, scan, parse_and_index, finish: finish_started.elapsed(), index_bytes })
}

/// The article an entry finally lands on, following section redirects.
pub(crate) fn stub_aware(entry: u32, stubs: &StubTable) -> Option<Target> {
    let mut target = Target { entry, fragment: None };
    for _ in 0..MAX_STUB_HOPS {
        match stubs.get(target.entry) {
            Some(next) => target = Target { entry: next.entry, fragment: target.fragment.or(next.fragment) },
            None => return Some(target),
        }
    }
    None
}

fn resolve_stubs(raw: Vec<(u32, String, Option<String>)>, path_to_entry: &HashMap<Box<str>, u32>) -> StubTable {
    let first_hop: Vec<(u32, Target)> = raw
        .into_iter()
        .filter_map(|(stub, path, fragment)| {
            let entry = *path_to_entry.get(path.as_str())?;
            (entry != stub).then_some((stub, Target { entry, fragment }))
        })
        .collect();
    // Collapse stub-to-stub chains so lookups take one step.
    let table = StubTable::from_rows(first_hop.clone());
    let rows = first_hop
        .into_iter()
        .filter_map(|(stub, target)| {
            let end = stub_aware(target.entry, &table)?;
            Some((stub, Target { entry: end.entry, fragment: target.fragment.or(end.fragment) }))
        })
        .collect();
    StubTable::from_rows(rows)
}

pub(crate) fn write_u32s(path: &Path, values: &[u32]) -> Result<()> {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(path, bytes)?;
    Ok(())
}

pub(crate) fn read_u32s(path: &Path) -> Result<Vec<u32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() % 4 != 0 {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a whole number of u32 values", path.display()),
        )));
    }
    Ok(bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() { dir_size(&entry.path())? } else { meta.len() };
    }
    Ok(total)
}
