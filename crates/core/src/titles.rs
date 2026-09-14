//! Title suggestions from an FST keyed by normalized title.
//!
//! Key: normalized title, a 0 byte, the entry index (big-endian u32), so equal
//! titles stay distinct. Value: inbound-link score in the high 32 bits and the
//! resolved article entry in the low 32 bits.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use memmap2::Mmap;

use crate::Result;
use crate::normalize::{normalize, normalize_prefix};

pub struct TitleEntry<'a> {
    pub title: &'a str,
    pub entry: u32,
    pub target: u32,
    pub score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleHit {
    /// The entry whose title matched: the article itself or a redirect to it.
    pub entry: u32,
    /// The article that entry leads to.
    pub target: u32,
    pub score: u32,
    pub exact: bool,
}

pub fn build<'a>(path: &Path, entries: impl IntoIterator<Item = TitleEntry<'a>>) -> Result<usize> {
    let mut keyed: Vec<(Vec<u8>, u64)> = entries
        .into_iter()
        .filter_map(|e| {
            let norm = normalize(e.title);
            if norm.is_empty() {
                return None;
            }
            let mut key = norm.into_bytes();
            key.push(0);
            key.extend_from_slice(&e.entry.to_be_bytes());
            Some((key, (u64::from(e.score) << 32) | u64::from(e.target)))
        })
        .collect();
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    keyed.dedup_by(|a, b| a.0 == b.0);
    let mut builder = MapBuilder::new(BufWriter::new(File::create(path)?))?;
    for (key, value) in &keyed {
        builder.insert(key, *value)?;
    }
    builder.finish()?;
    Ok(keyed.len())
}

pub struct TitleIndex {
    map: Map<Mmap>,
}

impl TitleIndex {
    pub fn open(path: &Path) -> Result<TitleIndex> {
        let file = File::open(path)?;
        // SAFETY: read-only mapping of an index file this program wrote.
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(TitleIndex { map: Map::new(mmap)? })
    }

    /// Up to `limit` articles whose title, or a redirect's title, starts with
    /// `query`. One hit per article: exact matches first, then by score.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<TitleHit> {
        let prefix = normalize_prefix(query);
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let prefix = prefix.as_bytes();
        let mut best: HashMap<u32, (TitleHit, usize)> = HashMap::new();
        let mut stream = self.map.range().ge(prefix).into_stream();
        while let Some((key, value)) = stream.next() {
            if !key.starts_with(prefix) {
                break;
            }
            let Some(sep) = key.len().checked_sub(5).filter(|&s| key[s] == 0) else { continue };
            let entry = u32::from_be_bytes(key[sep + 1..].try_into().unwrap());
            let hit = TitleHit { entry, target: value as u32, score: (value >> 32) as u32, exact: sep == prefix.len() };
            let title_len = sep;
            let better = |old: &(TitleHit, usize)| {
                let rank = |h: &TitleHit, len: usize| (h.exact, h.entry == h.target, std::cmp::Reverse(len));
                rank(&hit, title_len) > rank(&old.0, old.1)
            };
            match best.get(&hit.target) {
                Some(old) if !better(old) => {}
                _ => {
                    best.insert(hit.target, (hit, title_len));
                }
            }
        }
        let mut hits: Vec<(TitleHit, usize)> = best.into_values().collect();
        let order = |a: &(TitleHit, usize), b: &(TitleHit, usize)| {
            b.0.exact.cmp(&a.0.exact).then(b.0.score.cmp(&a.0.score)).then(a.1.cmp(&b.1)).then(a.0.target.cmp(&b.0.target))
        };
        if hits.len() > limit {
            hits.select_nth_unstable_by(limit - 1, order);
            hits.truncate(limit);
        }
        hits.sort_unstable_by(order);
        hits.into_iter().map(|(h, _)| h).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[(&str, u32, u32, u32)]) -> (tempfile::TempDir, TitleIndex) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("titles.fst");
        build(&path, entries.iter().map(|&(title, entry, target, score)| TitleEntry { title, entry, target, score }))
            .unwrap();
        let idx = TitleIndex::open(&path).unwrap();
        (dir, idx)
    }

    #[test]
    fn ranks_exact_then_score_and_dedupes_redirects() {
        let (_d, idx) = index(&[
            ("Einstein", 1, 2, 900),
            ("Albert Einstein", 2, 2, 900),
            ("Einsteinium", 3, 3, 40),
            ("Einstein ring", 4, 4, 60),
            ("Einstein (disambiguation)", 5, 5, 10),
            ("EINSTEIN", 6, 2, 900),
        ]);
        let hits = idx.suggest("einst", 10);
        let targets: Vec<u32> = hits.iter().map(|h| h.target).collect();
        assert_eq!(targets, [2, 4, 3, 5]);
        assert_eq!(hits[0].entry, 1, "shortest redirect title represents the article on a prefix match");

        let exact = idx.suggest("Einstein", 10);
        assert!(exact[0].exact);
        assert_eq!(exact[0].target, 2);
    }

    #[test]
    fn trailing_space_limits_to_word_and_limit_applies() {
        let (_d, idx) = index(&[("New York", 1, 1, 5), ("Newton", 2, 2, 50), ("New Zealand", 3, 3, 9)]);
        let spaced: Vec<u32> = idx.suggest("new ", 10).iter().map(|h| h.target).collect();
        assert_eq!(spaced, [3, 1]);
        assert_eq!(idx.suggest("new", 1).iter().map(|h| h.target).collect::<Vec<_>>(), [2]);
        assert!(idx.suggest("", 5).is_empty());
        assert!(idx.suggest("zzz", 5).is_empty());
    }
}
