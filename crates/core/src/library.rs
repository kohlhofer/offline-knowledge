use std::path::{Path, PathBuf};
use std::sync::Arc;

use ok_zim::{Archive, DirentKind};

use crate::document::{ArticleContext, Document, Target, parse_article};
use crate::fulltext::FullText;
use crate::import::{INDEX_FORMAT, IndexMeta, index_dir_for, read_u32s, stub_aware};
use crate::stubs::StubTable;
use crate::titles::TitleIndex;
use crate::{Error, Result};

/// An imported ZIM file: the archive plus the indexes built by `import`.
pub struct Library {
    zim_path: PathBuf,
    archive: Archive,
    namespace: u8,
    titles: TitleIndex,
    fulltext: FullText,
    inbound: Arc<[u32]>,
    articles: Vec<u32>,
    stubs: StubTable,
    meta: IndexMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The article to open.
    pub article: u32,
    pub title: String,
    /// The redirect title that matched, when it differs from the article's.
    pub matched: Option<String>,
    /// The section a section redirect points at.
    pub fragment: Option<String>,
    pub inbound: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub article: u32,
    pub title: String,
    pub summary: String,
    pub score: f32,
}

/// The result of [`Library::resolve_title`]: an exact match, or suggestions
/// when there isn't one. Never a silent fallback to the closest guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Found(Target),
    NotFound { suggestions: Vec<Suggestion> },
}

impl Library {
    pub fn open(zim_path: impl AsRef<Path>) -> Result<Library> {
        let zim_path = zim_path.as_ref().to_path_buf();
        let dir = index_dir_for(&zim_path);
        if !dir.join("meta.json").exists() {
            return Err(Error::NotImported(zim_path));
        }
        let meta: IndexMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?)?;
        if meta.format != INDEX_FORMAT {
            return Err(Error::IndexFormat { expected: INDEX_FORMAT, found: meta.format });
        }
        let archive = Archive::open(&zim_path)?;
        let uuid = archive.header().uuid_hex();
        if uuid != meta.zim_uuid || archive.file_len() != meta.zim_bytes {
            return Err(Error::IndexMismatch { index: dir, expected: uuid, found: meta.zim_uuid });
        }
        let inbound: Arc<[u32]> = read_u32s(&dir.join("inbound.u32"))?.into();
        Ok(Library {
            namespace: archive.article_namespace(),
            titles: TitleIndex::open(&dir.join("titles.fst"))?,
            fulltext: FullText::open(&dir.join("fulltext"), Arc::clone(&inbound))?,
            articles: read_u32s(&dir.join("articles.u32"))?,
            stubs: StubTable::read(&dir.join("stubs.bin"))?,
            inbound,
            archive,
            meta,
            zim_path,
        })
    }

    pub fn zim_path(&self) -> &Path {
        &self.zim_path
    }

    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    pub fn archive(&self) -> &Archive {
        &self.archive
    }

    pub fn article_count(&self) -> usize {
        self.articles.len()
    }

    /// Article entry indices, sorted.
    pub fn articles(&self) -> &[u32] {
        &self.articles
    }

    pub fn inbound(&self, article: u32) -> u32 {
        self.inbound.get(article as usize).copied().unwrap_or(0)
    }

    pub fn title(&self, entry: u32) -> Result<String> {
        Ok(self.archive.dirent(entry)?.title_str())
    }

    /// The path (no namespace) of an entry, for building `/wiki/{path}` links.
    pub fn path(&self, entry: u32) -> Result<String> {
        Ok(self.archive.dirent(entry)?.path_str())
    }

    /// Resolves `query` to an article exactly, never falling back to the
    /// closest guess: a path or title/alias typed exactly (case, accents and
    /// underscores folded), or a short list of suggestions when there is no
    /// exact match.
    pub fn resolve_title(&self, query: &str) -> Result<Resolution> {
        if let Some(target) = self.find(&query.replace(' ', "_"))? {
            return Ok(Resolution::Found(target));
        }
        if let Some(hit) = self.titles.suggest(query, 1).into_iter().next().filter(|h| h.exact) {
            let fragment = self.stubs.get(hit.entry).and_then(|t| t.fragment);
            return Ok(Resolution::Found(Target { entry: hit.target, fragment }));
        }
        Ok(Resolution::NotFound { suggestions: self.suggest(query, 5)? })
    }

    pub fn suggest(&self, query: &str, limit: usize) -> Result<Vec<Suggestion>> {
        self.titles
            .suggest(query, limit)
            .into_iter()
            .map(|hit| {
                let title = self.title(hit.target)?;
                let matched = if hit.entry == hit.target { None } else { Some(self.title(hit.entry)?) };
                let fragment = self.stubs.get(hit.entry).and_then(|t| t.fragment);
                Ok(Suggestion { article: hit.target, title, matched, fragment, inbound: hit.score })
            })
            .collect()
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.fulltext
            .search(query, limit)?
            .into_iter()
            .map(|hit| Ok(SearchResult { article: hit.entry, title: self.title(hit.entry)?, summary: hit.summary, score: hit.score }))
            .collect()
    }

    /// Resolves an article path (no namespace) through redirects and section redirects.
    pub fn find(&self, path: &str) -> Result<Option<Target>> {
        match self.archive.find_by_path(self.namespace, path.as_bytes())? {
            Some(entry) => self.resolve(entry).map(Some),
            None => Ok(None),
        }
    }

    /// The article an entry leads to, and the section when it is a section redirect.
    pub fn resolve(&self, entry: u32) -> Result<Target> {
        let resolved = self.archive.resolve(entry)?;
        stub_aware(resolved, &self.stubs).ok_or(Error::NotArticle(entry))
    }

    /// Loads and parses an article, following redirects first.
    pub fn article(&self, entry: u32) -> Result<Document> {
        let entry = self.resolve(entry)?.entry;
        let dirent = self.archive.dirent(entry)?;
        if !matches!(dirent.kind, DirentKind::Content { .. }) || self.archive.mime_type(&dirent) != Some("text/html") {
            return Err(Error::NotArticle(entry));
        }
        let (path, title) = (dirent.path_str(), dirent.title_str());
        let blob = self.archive.content(entry)?;
        let html = String::from_utf8_lossy(&blob);
        let resolver = |p: &str| self.find(p).ok().flatten();
        Ok(parse_article(&html, &ArticleContext { entry, path: &path, title: &title }, &resolver))
    }

    /// A pseudo-random article; pass any changing seed.
    pub fn random_article(&self, seed: u64) -> Option<u32> {
        if self.articles.is_empty() {
            return None;
        }
        // SplitMix64: good enough spread for picking an article.
        let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        Some(self.articles[(z % self.articles.len() as u64) as usize])
    }
}
