//! Several imported ZIM files held by one process, exactly one of them
//! active at a time.
//!
//! [`Library`] keeps meaning "one imported ZIM": every ranked list comes
//! from one of them, and a frontend can only reach a `Library` through a
//! [`Collection`], so a query scoped to the wrong file is a type error
//! rather than something a review has to catch. The one cross-collection
//! operation is [`Collections::exact_elsewhere`], an unscored existence
//! probe used to add a hint to a miss.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ok_zim::Archive;

use crate::import::{IndexMeta, index_dir_for};
use crate::titles::TitleIndex;
use crate::{Error, Library, Result, text};

/// The longest a label may be, its first character included.
const MAX_LABEL_LEN: usize = 24;

/// How much of an offending, ZIM-supplied string an error message repeats.
const MAX_ECHO_CHARS: usize = 40;

/// A collection's short name: the segment in a URL path, the prefix in an
/// MCP identifier, the token in the TUI's status bar.
///
/// The only constructor validates, so "a label is safe in a URL path
/// segment, an HTML attribute, a terminal line and MCP text without further
/// escaping" is a fact about the type rather than a convention. Labels are
/// ASCII: no percent-encoding, no homoglyphs, no bidi.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(Box<str>);

impl Label {
    /// `^[a-z][a-z0-9-]{0,23}$`, with no trailing `-`. Syntax, length and
    /// (through [`Collections::open`]) uniqueness are all `ok-core` checks.
    /// Reserved names belong to whichever frontend has routes, and change
    /// at that router's rate, so they live there.
    pub fn new(label: &str) -> Result<Label> {
        let valid = (1..=MAX_LABEL_LEN).contains(&label.len())
            && label.starts_with(|c: char| c.is_ascii_lowercase())
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.ends_with('-');
        if !valid {
            return Err(Error::Label(echo(label)));
        }
        Ok(Label(label.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The label a ZIM's `Name` metadata implies: the text before the first
/// `_`, lowercased. `wikipedia_en_top` becomes `wikipedia`, which stays put
/// across the editions and languages of one project, where the whole `Name`
/// would bake the edition into every bookmark.
pub fn label_from_name(name: &str) -> Result<Label> {
    let candidate = name.split('_').next().unwrap_or_default().to_ascii_lowercase();
    Label::new(&candidate).map_err(|_| Error::Label(echo(name)))
}

/// Refuses a ZIM mwoffliner did not write. One parser, one ranking and one
/// renderer serve every supported collection; a sotoki or devdocs file
/// would parse into nonsense rather than fail outright, so it is turned
/// away instead.
pub fn check_scraper(zim: &Path, scraper: Option<&str>) -> Result<()> {
    if scraper.is_some_and(|s| s.to_ascii_lowercase().starts_with("mwoffliner")) {
        return Ok(());
    }
    let scraper = scraper.map_or_else(|| "no Scraper metadata".to_string(), echo);
    Err(Error::UnsupportedScraper { zim: zim.to_path_buf(), scraper })
}

/// ZIM-supplied text on its way into an error message: control characters
/// and bidi overrides stripped, newlines collapsed, and cut to
/// [`MAX_ECHO_CHARS`] so a crafted `Name` cannot flood a terminal line.
fn echo(s: &str) -> String {
    let clean = text::sanitize_line(s);
    match clean.char_indices().nth(MAX_ECHO_CHARS) {
        Some((cut, _)) => format!("{}…", &clean[..cut]),
        None => clean,
    }
}

/// One imported ZIM within a [`Collections`] set.
pub struct Collection {
    zim_path: PathBuf,
    index_dir: PathBuf,
    title: String,
    article_count: usize,
    /// Filled when the index records `IndexMeta.name`. An index built
    /// before that field existed leaves it empty and [`Collection::label`]
    /// reads the ZIM's own `Name` instead: measured 0.88 ms (26 MB file),
    /// 1.61 ms (2.1 GB) and 1.76 ms (34 MB) on an M4 Pro with a warm cache,
    /// because the metadata cluster is compressed and decompressing it is
    /// nearly all of the cost. Against a 0.20 ms suggestion that is not
    /// something a one-shot `ok suggest` should pay, so a single-collection
    /// invocation resolves no label at all.
    label: OnceLock<std::result::Result<Label, Arc<str>>>,
    /// Lazy, and the failure is cached with it: [`Library::open`] reads
    /// `inbound.u32` and `stubs.bin`, mmaps the title FST and opens
    /// Tantivy, and a collection whose ZIM no longer matches its index must
    /// not retry that on every request. A race can open it twice and drop
    /// one result, which is harmless and keeps the hot path lock-free.
    library: OnceLock<std::result::Result<Arc<Library>, Arc<str>>>,
    /// Separate from `library`, and far cheaper: the miss hint opens only
    /// `titles.fst`, a `File::open` and an mmap. `None` when it cannot be
    /// opened, which costs that collection its hints and nothing else.
    titles: OnceLock<Option<TitleIndex>>,
}

impl Collection {
    /// Reads `meta.json` and nothing else: for an index this version wrote,
    /// no ZIM is opened and no [`Library`] is built.
    fn load(zim_path: &Path) -> Result<Collection> {
        let meta = IndexMeta::read(zim_path)?;
        let label = OnceLock::new();
        if let Some(name) = &meta.name {
            // A new-format index records what the ZIM said, so both checks
            // are free here; a legacy one defers them to `label`.
            check_scraper(zim_path, meta.scraper.as_deref())?;
            let _ = label.set(Ok(label_from_name(name)?));
        }
        Ok(Collection {
            index_dir: index_dir_for(zim_path),
            zim_path: zim_path.to_path_buf(),
            title: meta.title,
            article_count: meta.articles,
            label,
            library: OnceLock::new(),
            titles: OnceLock::new(),
        })
    }

    pub fn zim_path(&self) -> &Path {
        &self.zim_path
    }

    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// The ZIM's own `Title`. The label is the token a reader clicks and
    /// types; this is the brand a reader reads.
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn article_count(&self) -> usize {
        self.article_count
    }

    /// The label, read from the ZIM's `Name` metadata when the index
    /// predates [`IndexMeta`] recording it. Cached either way, the failure
    /// included.
    pub fn label(&self) -> Result<&Label> {
        match self.label.get_or_init(|| self.read_label().map_err(|e| Arc::from(e.to_string()))) {
            Ok(label) => Ok(label),
            Err(reason) => Err(Error::CollectionFailed { reason: reason.to_string() }),
        }
    }

    fn read_label(&self) -> Result<Label> {
        let archive = Archive::open(&self.zim_path)?;
        check_scraper(&self.zim_path, archive.metadata("Scraper")?.as_deref())?;
        label_from_name(archive.metadata("Name")?.as_deref().unwrap_or_default())
    }

    /// The library, opened on first use and cached. The uuid-and-size check
    /// [`Library::open`] makes happens here rather than at startup, so a
    /// collection can fail while the process is already serving the others.
    pub fn library(&self) -> Result<Arc<Library>> {
        let opened = self
            .library
            .get_or_init(|| Library::open(&self.zim_path).map(Arc::new).map_err(|e| Arc::from(e.to_string())));
        match opened {
            Ok(library) => Ok(Arc::clone(library)),
            Err(reason) => Err(Error::CollectionFailed { reason: reason.to_string() }),
        }
    }

    /// Whether [`Self::library`] has already opened this collection.
    pub fn is_open(&self) -> bool {
        self.library.get().is_some_and(|opened| opened.is_ok())
    }

    /// Why this collection failed, once [`Self::library`] has tried and
    /// failed. `None` before the first attempt: a collection nothing has
    /// opened is not known to be good either.
    pub fn failure(&self) -> Option<&str> {
        match self.library.get() {
            Some(Err(reason)) => Some(reason),
            _ => None,
        }
    }

    /// Whether an article with exactly this title exists here. Opens
    /// `titles.fst` alone, never a [`Library`].
    fn has_exact(&self, query: &str) -> bool {
        self.titles
            .get_or_init(|| TitleIndex::open(&self.index_dir.join("titles.fst")).ok())
            .as_ref()
            .is_some_and(|titles| titles.has_exact(query))
    }
}

/// A path [`Collections::open`] could not load, and why.
pub struct Skipped {
    pub path: PathBuf,
    /// Already sanitized and length-capped: safe to print as one line.
    pub reason: String,
}

impl Skipped {
    fn new(path: &Path, error: &Error) -> Skipped {
        Skipped { path: path.to_path_buf(), reason: text::sanitize_line(&error.to_string()) }
    }
}

/// The loaded set. One collection is the default; a frontend makes exactly
/// one of them active.
pub struct Collections {
    collections: Vec<Collection>,
    skipped: Vec<Skipped>,
    default_index: usize,
}

impl Collections {
    /// Loads every path, reading each one's `meta.json` and no more. A path
    /// that cannot be loaded — no index, an index this version cannot read,
    /// a ZIM mwoffliner did not write, an unusable or already-taken label —
    /// is skipped and recorded rather than fatal: the appliance ships
    /// `OK_ZIM=/data`, so a directory gaining a file is the normal case,
    /// and one new file must not stop the rest from serving. Only an empty
    /// result is an error.
    pub fn open(paths: &[PathBuf], default_label: Option<&str>) -> Result<Collections> {
        let mut loaded = Vec::new();
        let mut skipped = Vec::new();
        for path in paths {
            match Collection::load(path) {
                Ok(collection) => loaded.push(collection),
                Err(e) => skipped.push(Skipped::new(path, &e)),
            }
        }
        // Uniqueness cannot be checked without resolving every label, so a
        // set of more than one pays for its labels here. Exactly one needs
        // no label until a frontend asks for one, and then only when the
        // index predates `IndexMeta.name`.
        let collections = if loaded.len() > 1 { keep_unique_labels(loaded, &mut skipped) } else { loaded };
        if collections.is_empty() {
            return Err(Error::NoCollections);
        }
        let default_index = match default_label {
            Some(wanted) => index_of_label(&collections, wanted)?,
            None => 0,
        };
        Ok(Collections { collections, skipped, default_index })
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Collection> {
        self.collections.iter()
    }

    pub fn len(&self) -> usize {
        self.collections.len()
    }

    /// Always false: [`Self::open`] refuses an empty set.
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty()
    }

    pub fn get(&self, label: &str) -> Option<&Collection> {
        self.index_of(label).map(|i| &self.collections[i])
    }

    /// The collection at a position in load order. Every frontend holds its
    /// active collection as an index — the same index
    /// [`Self::exact_elsewhere`] takes — so each of them needs this.
    pub fn at(&self, index: usize) -> Option<&Collection> {
        self.collections.get(index)
    }

    pub fn index_of(&self, label: &str) -> Option<usize> {
        self.collections.iter().position(|c| c.label().is_ok_and(|l| l.as_str() == label))
    }

    /// The collection a request that names none lands in: the first loaded,
    /// or the one `--collection` named.
    pub fn default(&self) -> &Collection {
        &self.collections[self.default_index]
    }

    pub fn default_index(&self) -> usize {
        self.default_index
    }

    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// The other collections holding an article with exactly this title.
    /// Unscored and in load order: it exists to add "wiktionary has it" to
    /// a miss, not to rank anything, and each frontend caps the list
    /// itself. Opens only each candidate's `titles.fst`.
    pub fn exact_elsewhere(&self, active: usize, query: &str) -> Vec<&Collection> {
        self.collections
            .iter()
            .enumerate()
            .filter(|&(i, collection)| i != active && collection.has_exact(query))
            .map(|(_, collection)| collection)
            .collect()
    }
}

/// Drops every collection whose label is unusable or already taken, naming
/// it in `skipped`. The first file loaded keeps a contested label.
fn keep_unique_labels(loaded: Vec<Collection>, skipped: &mut Vec<Skipped>) -> Vec<Collection> {
    let mut kept: Vec<Collection> = Vec::new();
    for collection in loaded {
        let label = match collection.label() {
            Ok(label) => label.clone(),
            Err(e) => {
                skipped.push(Skipped::new(&collection.zim_path, &e));
                continue;
            }
        };
        match kept.iter().find(|k| k.label().is_ok_and(|kept| *kept == label)) {
            Some(first) => {
                let reason = format!("the label \"{label}\" is already taken by {}", first.zim_path.display());
                skipped.push(Skipped { path: collection.zim_path.clone(), reason: text::sanitize_line(&reason) });
            }
            None => kept.push(collection),
        }
    }
    kept
}

/// Resolving a wanted label forces every label, which is the point: a
/// single collection's is otherwise never read.
fn index_of_label(collections: &[Collection], wanted: &str) -> Result<usize> {
    for (i, collection) in collections.iter().enumerate() {
        if collection.label()?.as_str() == wanted {
            return Ok(i);
        }
    }
    let loaded = collections.iter().filter_map(|c| c.label().ok()).map(Label::to_string).collect::<Vec<_>>().join(", ");
    Err(Error::UnknownCollection { label: echo(wanted), loaded })
}
