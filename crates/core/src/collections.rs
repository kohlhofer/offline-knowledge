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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The failure carries its own class as well as its message: an `Error`
    /// is not `Clone`, and the class is what a browser page shows in place
    /// of a reason that names a filesystem path.
    label: OnceLock<std::result::Result<Label, (Arc<str>, SkipKind)>>,
    /// Lazy, and the failure is cached with it: [`Library::open`] reads
    /// `inbound.u32` and `stubs.bin`, mmaps the title FST and opens
    /// Tantivy, and a collection whose ZIM no longer matches its index must
    /// not retry that on every request. A race can open it twice and drop
    /// one result, which is harmless and keeps the hot path lock-free.
    library: OnceLock<std::result::Result<Arc<Library>, Arc<str>>>,
    /// Whether [`check_scraper`] has already passed here, so
    /// [`Self::open_library`] does not decompress the metadata cluster a
    /// second time for a `Scraper` [`Self::load`] read from `meta.json` for
    /// nothing or [`Self::read_label`] has just read from the ZIM: 0.99 ms a
    /// collection a process, and every frontend that shows a label paid it
    /// twice. A hint rather than a fact, so `Relaxed` is enough: losing the
    /// race costs one more read of an answer that does not change.
    scraper_checked: AtomicBool,
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
        // A new-format index records what the ZIM said, so both checks are
        // free here and neither is paid again; a legacy one records neither
        // and defers them to `read_label` and `open_library`.
        let recorded = meta.name.is_some() || meta.scraper.is_some();
        if recorded {
            check_scraper(zim_path, meta.scraper.as_deref())?;
        }
        if let Some(name) = &meta.name {
            let _ = label.set(Ok(label_from_name(name)?));
        }
        Ok(Collection {
            index_dir: index_dir_for(zim_path),
            zim_path: zim_path.to_path_buf(),
            title: meta.title,
            article_count: meta.articles,
            label,
            library: OnceLock::new(),
            scraper_checked: AtomicBool::new(recorded),
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
        match self.resolved_label() {
            Ok(label) => Ok(label),
            Err((reason, _)) => Err(Error::CollectionFailed { reason: reason.to_string() }),
        }
    }

    /// The cached label resolution, failure class included: what
    /// [`keep_unique_labels`] needs to report a skip the way every other one
    /// is reported.
    fn resolved_label(&self) -> &std::result::Result<Label, (Arc<str>, SkipKind)> {
        self.label.get_or_init(|| self.read_label().map_err(|e| (Arc::from(e.to_string()), SkipKind::of(&e))))
    }

    /// The same, for whoever has already resolved it, resolving nothing:
    /// `meta.json` records a label for free, and an index that predates the
    /// field costs a ZIM open to get one, so the pass in
    /// [`Collections::open`] judges the first kind and leaves the second.
    fn known_label(&self) -> Option<&std::result::Result<Label, (Arc<str>, SkipKind)>> {
        self.label.get()
    }

    fn read_label(&self) -> Result<Label> {
        let archive = Archive::open(&self.zim_path)?;
        check_scraper(&self.zim_path, archive.metadata("Scraper")?.as_deref())?;
        self.scraper_checked.store(true, Ordering::Relaxed);
        label_from_name(archive.metadata("Name")?.as_deref().unwrap_or_default())
    }

    /// The library, opened on first use and cached. The uuid-and-size check
    /// [`Library::open`] makes happens here rather than at startup, so a
    /// collection can fail while the process is already serving the others.
    pub fn library(&self) -> Result<Arc<Library>> {
        let opened = self
            .library
            .get_or_init(|| self.open_library().map(Arc::new).map_err(|e| Arc::from(e.to_string())));
        match opened {
            Ok(library) => Ok(Arc::clone(library)),
            Err(reason) => Err(Error::CollectionFailed { reason: reason.to_string() }),
        }
    }

    /// [`Library::open`] with the scraper gate on top, for the collections
    /// nothing has applied it to yet. [`Self::load`] can only apply it when
    /// the index recorded a scraper and [`Self::label`] only when it resolves
    /// a label from the ZIM, so for a single collection on an index that
    /// predates `IndexMeta.scraper` — which is every index built before this
    /// version — neither ran: `ok suggest`, `search`, `show` and `tui` read a
    /// sotoki or devdocs ZIM as mwoffliner's while `ok serve` refused to start
    /// on it. The archive is open here either way, so this is where the gate
    /// belongs for them; for the rest the answer is already in hand, and
    /// reading `Scraper` again decompresses the metadata cluster twice.
    fn open_library(&self) -> Result<Library> {
        let library = Library::open(&self.zim_path)?;
        if !self.scraper_checked.load(Ordering::Relaxed) {
            check_scraper(&self.zim_path, library.archive().metadata("Scraper")?.as_deref())?;
        }
        Ok(library)
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

/// What class of problem a skipped path has. [`Skipped::reason`] names the
/// file's own path, which a response body must not carry, so a browser page
/// renders this instead of the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipKind {
    /// Nothing has imported the file yet: the commonest one by far, since
    /// `/data` gains files.
    NotImported,
    /// A ZIM mwoffliner did not write.
    Scraper,
    /// The label this file derives is already another file's, named here by
    /// its filename alone.
    Duplicate { label: String, winner: String },
    /// An unusable `Name`, an index this version cannot read, an index that
    /// no longer describes its ZIM.
    Unusable,
}

impl SkipKind {
    fn of(error: &Error) -> SkipKind {
        match error {
            Error::NotImported(_) => SkipKind::NotImported,
            Error::UnsupportedScraper { .. } => SkipKind::Scraper,
            _ => SkipKind::Unusable,
        }
    }
}

/// A path [`Collections::open`] could not load, and why.
pub struct Skipped {
    pub path: PathBuf,
    /// Already sanitized and length-capped: safe to print as one line.
    pub reason: String,
    pub kind: SkipKind,
}

impl Skipped {
    fn new(path: &Path, error: &Error) -> Skipped {
        Skipped { path: path.to_path_buf(), reason: reason_line(path, &error.to_string()), kind: SkipKind::of(error) }
    }

    /// A label that could not be resolved. Its `Error` became a message and
    /// a class when [`Collection::label`] cached it, an `Error` being neither
    /// `Clone` nor something a cached failure can hand back twice.
    fn from_label(path: &Path, reason: &str, kind: SkipKind) -> Skipped {
        Skipped { path: path.to_path_buf(), reason: reason_line(path, reason), kind }
    }
}

/// A failure as one line, with the path stripped off its front: every line
/// that reports a skip names the file itself, and `NotImported`'s message
/// otherwise repeats a 110-character absolute path three times over.
fn reason_line(path: &Path, message: &str) -> String {
    let reason = text::sanitize_line(message);
    let named = format!("{} ", show_path(path));
    reason.strip_prefix(&named).unwrap_or(&reason).to_string()
}


/// A path on its way into a reason or an error message.
fn show_path(path: &Path) -> String {
    text::sanitize_line(&path.display().to_string())
}

/// The skipped paths on their way into an error that carries no set to
/// report them on. `ok --zim fake.zim suggest x` is the commonest first-run
/// mistake there is: without this it says only that nothing loaded.
fn skipped_note(skipped: &[Skipped]) -> String {
    match skipped {
        [] => String::new(),
        [one] => format!(" — skipped {}: {}", show_path(&one.path), one.reason),
        many => many.iter().map(|s| format!("\n  skipped {}: {}", show_path(&s.path), s.reason)).collect(),
    }
}


/// The loaded set. One collection is the default; a frontend makes exactly
/// one of them active.
pub struct Collections {
    collections: Vec<Collection>,
    skipped: Vec<Skipped>,
    default_index: usize,
    /// First path segments a frontend's own routes own, which no label may
    /// take. Which ones those are, and how often they change, belongs to
    /// that router, so they are passed in; holding them here makes
    /// [`Self::open`]'s filter and [`Self::resolve_labels`]' the same one,
    /// which is what keeps `default_index` valid across both.
    reserved: Vec<Box<str>>,
}

impl Collections {
    /// Loads every path, reading each one's `meta.json` and no more. A path
    /// that cannot be loaded — no index, an index this version cannot read,
    /// a ZIM mwoffliner did not write — is skipped and recorded rather than
    /// fatal: the appliance ships `OK_ZIM=/data`, so a directory gaining a
    /// file is the normal case, and one new file must not stop the rest from
    /// serving. Only an empty result is an error.
    ///
    /// No label is read here unless `default_label` names one. On an index
    /// that predates `IndexMeta.name` — which is every index built before
    /// this version, including the 2.1 GB one that cannot be rebuilt while a
    /// server reads it — a label costs an `Archive::open` and a
    /// metadata-cluster decompression, 1.1 to 1.7 ms per file, and
    /// `ok suggest` shows no label at all. [`Self::resolve_labels`] is where
    /// a frontend that does shows asks for them.
    ///
    /// The labels `meta.json` already carries are judged here, though, since
    /// judging them reads nothing: a collection no frontend that routes
    /// labels would keep must not be one `ok suggest` silently reads from.
    pub fn open(paths: &[PathBuf], default_label: Option<&str>, reserved: &[&str]) -> Result<Collections> {
        let reserved: Vec<Box<str>> = reserved.iter().map(|&segment| Box::from(segment)).collect();
        let mut collections = Vec::new();
        let mut skipped = Vec::new();
        for path in paths {
            match Collection::load(path) {
                Ok(collection) => collections.push(collection),
                Err(e) => skipped.push(Skipped::new(path, &e)),
            }
        }
        collections = keep_usable_labels(collections, &mut skipped, &reserved, false);
        non_empty(&collections, &skipped)?;
        let default_index = match default_label {
            // Finding a named label forces every label anyway, so the
            // uniqueness check is free here and the set arrives resolved.
            Some(wanted) => {
                collections = keep_usable_labels(collections, &mut skipped, &reserved, true);
                non_empty(&collections, &skipped)?;
                index_of_label(&collections, wanted, &skipped)?
            }
            None => 0,
        };
        Ok(Collections { collections, skipped, default_index, reserved })
    }

    /// Every label resolved, and every collection whose label is unusable or
    /// already another file's dropped and recorded: what a frontend that
    /// shows or routes labels needs, and what a one-shot that shows none
    /// must not pay for (`--zim data/ suggest pac` paid +3.1 ms for three
    /// labels it never used, 15x the 0.20 ms a suggestion itself costs).
    /// Free the second time: labels are cached, and a set that has been
    /// through this has nothing left to drop.
    pub fn resolve_labels(mut self) -> Result<Collections> {
        self.collections = keep_usable_labels(std::mem::take(&mut self.collections), &mut self.skipped, &self.reserved, true);

        non_empty(&self.collections, &self.skipped)?;
        // `default_index` is either 0, or an index into a set this already
        // ran over. Dropping the first collection therefore moves the
        // default to the next one in load order, which is what `default`
        // promises when nothing named one.
        Ok(self)
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

    /// The collection a request that names none lands in: the one
    /// `--collection` named, or the first in load order carrying a label a
    /// frontend could route to.
    pub fn default(&self) -> &Collection {
        &self.collections[self.default_index()]
    }

    /// Resolving forward and stopping at the first usable label is what
    /// makes this the same answer before and after [`Self::resolve_labels`]
    /// without paying for every label. That pass drops an unusable or
    /// reserved one, so a set that has not been through it would otherwise
    /// default to a collection `serve`, `mcp`, `tui` and `ok collections`
    /// all skip, and `ok suggest` would answer from it. At most one label is
    /// read, and on a set already resolved none is.
    pub fn default_index(&self) -> usize {
        let from = self.default_index;
        self.collections[from..]
            .iter()
            .position(|collection| collection.resolved_label().as_ref().is_ok_and(|label| !is_reserved(&self.reserved, label)))
            // Nothing carries a label a frontend could route to, so the
            // collection named keeps the place: one alone is kept whatever
            // its `Name` says, which is what `ok tui` on a ZIM carrying none
            // has always been.
            .map_or(from, |offset| from + offset)
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

/// An empty set is not a working process, whichever pass emptied it.
fn non_empty(collections: &[Collection], skipped: &[Skipped]) -> Result<()> {
    if collections.is_empty() {
        return Err(Error::NoCollections { skipped: skipped_note(skipped) });
    }
    Ok(())
}

/// Whether a label is a first path segment a frontend's own routes own.
fn is_reserved(reserved: &[Box<str>], label: &Label) -> bool {
    reserved.iter().any(|segment| **segment == *label.as_str())
}

/// Drops every collection whose label is unusable, already taken, or one a
/// frontend's own routes own, naming it in `skipped`. The first file loaded
/// keeps a contested label.
///
/// With `resolve` off only the labels an index already recorded are judged
/// and nothing is read: that is [`Collections::open`]'s pass, where paying
/// 1.1 to 1.7 ms a file for a label a one-shot never shows is exactly the
/// cost [`Collections::resolve_labels`] exists to defer.
fn keep_usable_labels(loaded: Vec<Collection>, skipped: &mut Vec<Skipped>, reserved: &[Box<str>], resolve: bool) -> Vec<Collection> {
    // One collection whose label cannot be resolved at all is kept: nothing
    // can collide with it, no frontend shows a single collection's label, and
    // `ok tui` and `ok mcp` have always worked on a ZIM carrying no `Name`
    // metadata. `ok serve`, which needs a label for its own URLs, still
    // refuses that set when it builds its router.
    let alone = loaded.len() == 1;
    let mut kept: Vec<Collection> = Vec::new();
    for collection in loaded {
        let known = if resolve { Some(collection.resolved_label()) } else { collection.known_label() };
        let label = match known {
            // Nothing is known about this one yet, and reading it is what
            // this pass was told not to do.
            None => {
                kept.push(collection);
                continue;
            }
            Some(Ok(label)) => label.clone(),
            Some(Err(_)) if alone => {
                kept.push(collection);
                continue;
            }
            Some(Err((reason, kind))) => {
                skipped.push(Skipped::from_label(&collection.zim_path, reason, kind.clone()));
                continue;
            }
        };
        // Skipped like any other unusable label rather than fatal: the
        // trigger is a `Name` inside a ZIM, so one hostile or unlucky file in
        // a directory of good ones would otherwise take every collection
        // down with it. A set left with nothing to serve is still an error,
        // from `non_empty` above.
        if is_reserved(reserved, &label) {
            let reason = format!("the label \"{label}\" is a reserved route segment");
            skipped.push(Skipped { path: collection.zim_path.clone(), reason, kind: SkipKind::Unusable });
            continue;
        }
        // `known_label`, not `label`: every collection kept above either has
        // its label cached already or is one this pass was told not to read.
        match kept.iter().find(|k| k.known_label().is_some_and(|l| l.as_ref().is_ok_and(|kept| *kept == label))) {
            Some(first) => {
                let reason = format!("the label \"{label}\" is already taken by {}", show_path(&first.zim_path));
                let winner = first.zim_path.file_name().unwrap_or(first.zim_path.as_os_str()).to_string_lossy().to_string();
                skipped.push(Skipped {
                    path: collection.zim_path.clone(),
                    reason: text::sanitize_line(&reason),
                    kind: SkipKind::Duplicate { label: label.to_string(), winner: text::sanitize_line(&winner) },
                });
            }
            None => kept.push(collection),
        }
    }
    kept
}

/// Resolving a wanted label forces every label, which is the point: a
/// single collection's is otherwise never read.
fn index_of_label(collections: &[Collection], wanted: &str, skipped: &[Skipped]) -> Result<usize> {
    for (i, collection) in collections.iter().enumerate() {
        if collection.label()?.as_str() == wanted {
            return Ok(i);
        }
    }
    let loaded = collections.iter().filter_map(|c| c.label().ok()).map(Label::to_string).collect::<Vec<_>>().join(", ");
    Err(Error::UnknownCollection { label: echo(wanted), loaded, skipped: skipped_note(skipped) })
}

