//! Section redirects.
//!
//! A ZIM redirect cannot carry a `#fragment`, so mwoffliner writes a tiny HTML
//! page with a meta refresh for every redirect that points at a section. The
//! 50,000-article English selection has 173,149 of them. The import turns each
//! into a (stub, article, fragment) row so they behave like redirects: never
//! parsed or indexed as articles, and resolved without reading content.

use std::path::Path;

use crate::document::{Href, Target, classify_href};
use crate::{Error, Result};

/// Refresh stubs are a few hundred bytes; anything larger is a real page.
const MAX_STUB_BYTES: usize = 4096;

/// The `href` a meta-refresh stub page points at, if `html` is one.
pub fn refresh_target(page_path: &str, html: &[u8]) -> Option<(String, Option<String>)> {
    if html.len() > MAX_STUB_BYTES {
        return None;
    }
    let text = std::str::from_utf8(html).ok()?;
    let lower = text.to_ascii_lowercase();
    let meta = lower.find("http-equiv=\"refresh\"").or_else(|| lower.find("http-equiv='refresh'"))?;
    let url_at = lower[meta..].find("url=")? + meta + 4;
    let rest = text[url_at..].trim_start_matches(['\'', '"', ' ']);
    let end = rest.find(['\'', '"', '>'])?;
    match classify_href(page_path, rest[..end].trim())? {
        Href::Internal { path, fragment } => Some((path, fragment)),
        _ => None,
    }
}

#[derive(Debug, Default, Clone)]
pub struct StubTable {
    /// Sorted by stub entry.
    rows: Vec<(u32, u32)>,
    fragments: Vec<Option<String>>,
}

impl StubTable {
    pub fn from_rows(mut rows: Vec<(u32, Target)>) -> StubTable {
        rows.sort_unstable_by_key(|(stub, _)| *stub);
        rows.dedup_by_key(|(stub, _)| *stub);
        StubTable {
            fragments: rows.iter().map(|(_, t)| t.fragment.clone()).collect(),
            rows: rows.into_iter().map(|(stub, t)| (stub, t.entry)).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, stub: u32) -> Option<Target> {
        let i = self.rows.binary_search_by_key(&stub, |(s, _)| *s).ok()?;
        Some(Target { entry: self.rows[i].1, fragment: self.fragments[i].clone() })
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.rows.iter().copied()
    }

    /// Layout: row count (u32), rows of (stub u32, article u32, fragment length
    /// u32 with u32::MAX for none), then the fragment bytes in row order.
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut out = Vec::with_capacity(4 + self.rows.len() * 12);
        out.extend_from_slice(&(self.rows.len() as u32).to_le_bytes());
        for ((stub, article), fragment) in self.rows.iter().zip(&self.fragments) {
            out.extend_from_slice(&stub.to_le_bytes());
            out.extend_from_slice(&article.to_le_bytes());
            let len = fragment.as_ref().map_or(u32::MAX, |f| f.len() as u32);
            out.extend_from_slice(&len.to_le_bytes());
        }
        for fragment in self.fragments.iter().flatten() {
            out.extend_from_slice(fragment.as_bytes());
        }
        std::fs::write(path, out)?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<StubTable> {
        let bytes = std::fs::read(path)?;
        let bad = || Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("corrupt {}", path.display())));
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(bad);
        let count = u32_at(0)? as usize;
        let mut rows = Vec::with_capacity(count.min(bytes.len() / 12));
        let mut lengths = Vec::with_capacity(rows.capacity());
        for i in 0..count {
            let at = 4 + i * 12;
            rows.push((u32_at(at)?, u32_at(at + 4)?));
            lengths.push(u32_at(at + 8)?);
        }
        let mut cursor = 4 + count * 12;
        let mut fragments = Vec::with_capacity(count);
        for len in lengths {
            if len == u32::MAX {
                fragments.push(None);
                continue;
            }
            let end = cursor.checked_add(len as usize).filter(|&e| e <= bytes.len()).ok_or_else(bad)?;
            fragments.push(Some(String::from_utf8_lossy(&bytes[cursor..end]).into_owned()));
            cursor = end;
        }
        if !rows.is_sorted_by_key(|(s, _)| *s) {
            return Err(bad());
        }
        Ok(StubTable { rows, fragments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUB: &str = r#"<html>
  <head>
    <title>"Chunky" Lee Chong</title>
    <meta http-equiv="refresh" content="0;URL='./Grand_Theft_Auto_III#Plot'" />
  </head>
  <body>
    <a href="./Grand_Theft_Auto_III#Plot">"Chunky" Lee Chong</a>
  </body>
</html>"#;

    #[test]
    fn recognises_mwoffliner_refresh_stubs() {
        assert_eq!(
            refresh_target("\"Chunky\"_Lee_Chong", STUB.as_bytes()),
            Some(("Grand_Theft_Auto_III".into(), Some("Plot".into())))
        );
        assert_eq!(
            refresh_target("A/B", br#"<meta http-equiv='Refresh' content="0; url=../C%20D">"#),
            Some(("C D".into(), None))
        );
        assert_eq!(refresh_target("X", b"<html><body><p>Real page</p></body></html>"), None);
        assert_eq!(refresh_target("X", br#"<meta http-equiv="refresh" content="0;URL='https://example.org'">"#), None);
        let mut big = STUB.to_string();
        big.push_str(&" ".repeat(MAX_STUB_BYTES));
        assert_eq!(refresh_target("X", big.as_bytes()), None);
    }

    #[test]
    fn table_round_trips_and_looks_up() {
        let table = StubTable::from_rows(vec![
            (30, Target { entry: 3, fragment: Some("Plot".into()) }),
            (10, Target { entry: 1, fragment: None }),
            (20, Target { entry: 2, fragment: Some("Legal_protection".into()) }),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stubs.bin");
        table.write(&path).unwrap();
        let back = StubTable::read(&path).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back.get(20), Some(Target { entry: 2, fragment: Some("Legal_protection".into()) }));
        assert_eq!(back.get(10), Some(Target { entry: 1, fragment: None }));
        assert_eq!(back.get(15), None);

        std::fs::write(&path, [9, 0, 0, 0, 1]).unwrap();
        assert!(StubTable::read(&path).is_err());
    }
}
