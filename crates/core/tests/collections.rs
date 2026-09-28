//! Several imported ZIMs in one set: labels, skip-and-continue, lazy
//! opening and the one cross-collection probe.

use std::path::{Path, PathBuf};

use ok_core::collections::{Collections, label_from_name};
use ok_core::import::{ImportOptions, IndexMeta, import, index_dir_for};
use ok_zim::write::ZimBuilder;

fn page(title: &str, body: &str) -> String {
    format!(r#"<html><body><h1>{title}</h1><div id="mw-content-text"><div class="mw-parser-output">{body}</div></div></body></html>"#)
}

/// A ZIM carrying the three metadata fields a collection reads: `Name` (the
/// label's source), `Title` (the brand) and `Scraper` (the refusal).
fn build(dir: &Path, file: &str, name: &str, title: &str, scraper: Option<&str>, articles: &[&str]) -> PathBuf {
    let mut builder = ZimBuilder::new();
    for article in articles {
        builder = builder.article(&article.replace(' ', "_"), article, &page(article, "<p>Some prose about the subject.</p>"));
    }
    builder = builder.metadata("Title", title).metadata("Name", name);
    if let Some(scraper) = scraper {
        builder = builder.metadata("Scraper", scraper);
    }
    let path = dir.join(file);
    std::fs::write(&path, builder.build()).unwrap();
    path
}

fn imported(dir: &Path, file: &str, name: &str, title: &str, articles: &[&str]) -> PathBuf {
    let path = build(dir, file, name, title, Some("mwoffliner 1.17.5"), articles);
    import(&path, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    path
}

/// Edits an index's `meta.json` in place: the only way to produce an index
/// today's `import` would refuse to build.
fn rewrite_meta(zim: &Path, edit: impl FnOnce(&mut IndexMeta)) {
    let path = index_dir_for(zim).join("meta.json");
    let mut meta: IndexMeta = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut meta);
    std::fs::write(&path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
}

/// The label is `Name`'s first token, lowercased, and the first file loaded
/// is the default. The ZIM's own `Title` stays the brand.
#[test]
fn labels_come_from_name_and_the_first_loaded_is_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let b = imported(dir.path(), "b.zim", "wiktionary_en-simple_all", "Wiktionary in Simple English", &["Mercury"]);
    let set = Collections::open(&[a, b], None).unwrap();

    let labels: Vec<String> = set.iter().map(|c| c.label().unwrap().to_string()).collect();
    assert_eq!(labels, ["wikipedia", "wiktionary"], "the text before the first underscore, lowercased");
    assert_eq!(set.len(), 2);
    assert!(set.skipped().is_empty());
    assert_eq!(set.default().label().unwrap().as_str(), "wikipedia");
    assert_eq!(set.get("wiktionary").unwrap().title(), "Wiktionary in Simple English");
    assert_eq!(set.get("wiktionary").unwrap().article_count(), 1);
    assert_eq!(set.index_of("wiktionary"), Some(1));
    assert!(set.get("archlinux").is_none());
}

/// `--collection` picks the default, and an unknown one names what is
/// loaded rather than falling back to something the caller did not ask for.
#[test]
fn a_named_default_is_resolved_and_an_unknown_one_lists_the_loaded_labels() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let b = imported(dir.path(), "b.zim", "wiktionary_en-simple_all", "Wiktionary in Simple English", &["Mercury"]);

    let set = Collections::open(&[a.clone(), b.clone()], Some("wiktionary")).unwrap();
    assert_eq!(set.default().label().unwrap().as_str(), "wiktionary");
    assert_eq!(set.default_index(), 1);

    let err = Collections::open(&[a, b], Some("archlinux")).err().unwrap().to_string();
    assert!(err.contains("archlinux") && err.contains("wikipedia") && err.contains("wiktionary"), "{err}");
}

/// The executable form of "startup gains no work": `Collections::open`
/// reads `meta.json` and nothing else, and opening one collection leaves
/// the rest closed.
#[test]
fn open_opens_no_library_and_one_open_leaves_the_others_closed() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let b = imported(dir.path(), "b.zim", "wiktionary_en-simple_all", "Wiktionary in Simple English", &["Mercury"]);
    let set = Collections::open(&[a, b], None).unwrap();

    assert!(set.iter().all(|c| !c.is_open()), "startup must open no library");
    let library = set.get("wikipedia").unwrap().library().unwrap();
    assert_eq!(library.article_count(), 1);
    assert!(set.get("wikipedia").unwrap().is_open());
    assert!(!set.get("wiktionary").unwrap().is_open(), "opening one must not open the rest");
}

/// One un-imported file, one index recording a scraper `ok` does not
/// support, one good: the good one serves, and the other two are named with
/// a reason. `/data` gains files; one new file must not stop the appliance.
#[test]
fn an_unusable_file_is_skipped_with_a_reason_and_the_rest_load() {
    let dir = tempfile::tempdir().unwrap();
    let raw = build(dir.path(), "raw.zim", "wiktionary_en_all", "Wiktionary", Some("mwoffliner 1.17.5"), &["Mercury"]);
    let good = imported(dir.path(), "good.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let scraped = imported(dir.path(), "scraped.zim", "stack_en_all", "Stack Exchange", &["Question"]);
    // An index built by an `ok` that recorded the scraper but did not yet
    // refuse it: today's `import` turns the file away outright, so an
    // older build is the only thing that can leave this behind.
    rewrite_meta(&scraped, |m| m.scraper = Some("sotoki 1.3".into()));

    let set = Collections::open(&[raw.clone(), good, scraped.clone()], None).unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(set.default().label().unwrap().as_str(), "wikipedia");

    assert_eq!(set.skipped().len(), 2, "{:?}", set.skipped().iter().map(|s| &s.reason).collect::<Vec<_>>());
    let reason = |path: &Path| set.skipped().iter().find(|s| s.path == path).unwrap().reason.clone();
    let raw_reason = reason(&raw);
    assert!(raw_reason.contains("ok --zim") && raw_reason.contains("import"), "the reason names the command that fixes it: {raw_reason}");
    let scraped_reason = reason(&scraped);
    assert!(scraped_reason.contains("sotoki 1.3"), "{scraped_reason}");
}

/// Skipping is per file, but an empty set is not a working process.
#[test]
fn nothing_loadable_is_an_error_not_an_empty_set() {
    let dir = tempfile::tempdir().unwrap();
    let raw = build(dir.path(), "raw.zim", "wikipedia_en_top", "Best of Wikipedia", Some("mwoffliner 1.17.5"), &["Albert Einstein"]);
    let err = Collections::open(&[raw], None).err().unwrap();
    assert!(matches!(err, ok_core::Error::NoCollections), "{err}");
}

/// A label is the token in a URL path, an MCP identifier and a terminal
/// line, so the only constructor validates syntax and length. The offending
/// `Name` is echoed back sanitized and cut.
#[test]
fn a_name_that_cannot_make_a_label_is_refused_with_a_sanitized_echo() {
    assert_eq!(label_from_name("wikipedia_en_top").unwrap().as_str(), "wikipedia");
    assert_eq!(label_from_name("wiktionary_en-simple_all").unwrap().as_str(), "wiktionary");
    assert_eq!(label_from_name("ArchLinux_en_all").unwrap().as_str(), "archlinux", "the first token is lowercased");
    assert_eq!(label_from_name("ok-docs_en").unwrap().as_str(), "ok-docs", "an inner dash is fine");

    for name in ["Wiki Pedia", "9lives_en", "../etc_passwd", "", "-dash_en", "dash-_en", "wiki.pedia_en"] {
        assert!(label_from_name(name).is_err(), "{name:?} must not make a label");
    }
    assert!(label_from_name(&"a".repeat(60)).is_err(), "a 60-character first token is too long");

    let hostile = label_from_name("wiki\u{1b}[2Jpedia\u{202e}_en").unwrap_err().to_string();
    assert!(!hostile.contains('\u{1b}'), "an ESC in Name must not reach a terminal line: {hostile:?}");
    assert!(!hostile.contains('\u{202e}'), "a bidi override in Name must not reach a terminal line: {hostile:?}");
    let flood = label_from_name(&"x y".repeat(200)).unwrap_err().to_string();
    assert!(flood.chars().count() < 200, "the echoed Name is cut, not printed whole: {flood:?}");
}

/// Two editions of one project derive the same label. The first loaded
/// keeps it and the second is skipped, naming both files.
#[test]
fn a_label_collision_skips_the_second_file_and_names_both() {
    let dir = tempfile::tempdir().unwrap();
    let en = imported(dir.path(), "en.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let de = imported(dir.path(), "de.zim", "wikipedia_de_all", "Wikipedia", &["Ulm"]);
    let set = Collections::open(&[en, de.clone()], None).unwrap();

    assert_eq!(set.len(), 1);
    assert_eq!(set.skipped().len(), 1);
    assert_eq!(set.skipped()[0].path, de);
    let reason = &set.skipped()[0].reason;
    assert!(reason.contains("en.zim"), "the reason names the file that took the label: {reason}");
    assert!(reason.contains("wikipedia"), "{reason}");
}

/// An index written before `IndexMeta` carried `name` and `scraper` reads
/// them from the ZIM instead, once, when something asks for a label.
/// Bumping the index format would have forced a re-import of a 2.1 GB file,
/// so both fields are optional in either direction.
#[test]
fn an_index_without_name_or_scraper_still_loads_and_labels_itself() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    rewrite_meta(&a, |m| {
        m.name = None;
        m.scraper = None;
    });

    let set = Collections::open(&[a], None).unwrap();
    assert_eq!(set.default().label().unwrap().as_str(), "wikipedia");
    assert!(!set.default().is_open(), "reading a label must not cost a Library");
}

/// Opening is lazy, so a ZIM replaced under a running process fails at
/// first use rather than at startup — and the outcome is cached, reason
/// and all, so a doomed open is not retried on every request.
#[test]
fn a_collection_whose_zim_no_longer_matches_fails_at_first_use_and_stays_failed() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let original = std::fs::read(&a).unwrap();
    // A different ZIM at the same path, so the index no longer describes it.
    build(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", Some("mwoffliner 1.17.5"), &["Marie Curie", "Ulm", "Bern"]);
    assert_ne!(std::fs::read(&a).unwrap().len(), original.len(), "the fixture must actually differ from the imported file");

    let set = Collections::open(std::slice::from_ref(&a), None).unwrap();
    assert_eq!(set.default().failure(), None, "nothing is known to be broken before the first use");

    let first = set.default().library().err().unwrap().to_string();
    assert_eq!(set.default().failure(), Some(first.as_str()), "the failure is remembered");
    assert!(!set.default().is_open());

    std::fs::write(&a, &original).unwrap();
    let second = set.default().library().err().unwrap().to_string();
    assert_eq!(first, second, "the cached reason is handed back, not recomputed");
}

/// The only cross-collection operation: an unscored existence probe over
/// `titles.fst` alone, never the active collection and never a `Library`.
#[test]
fn exact_elsewhere_names_the_other_collections_without_opening_a_library() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let b = imported(dir.path(), "b.zim", "wiktionary_en-simple_all", "Wiktionary in Simple English", &["Mercury"]);
    let set = Collections::open(&[a, b], None).unwrap();

    let hits: Vec<&str> = set.exact_elsewhere(0, "Mercury").iter().map(|c| c.label().unwrap().as_str()).collect();
    assert_eq!(hits, ["wiktionary"]);
    assert!(set.exact_elsewhere(1, "Mercury").is_empty(), "a collection is never its own hint");
    assert!(set.exact_elsewhere(0, "Merc").is_empty(), "a prefix-only match must not fire the hint");
    assert!(set.exact_elsewhere(0, "Nothing at all").is_empty());
    assert!(set.iter().all(|c| !c.is_open()), "the probe opens titles.fst, never a Library");
}

/// The refusal runs before any work: a 90-second import must not end in
/// "wrong scraper", and one parser, one ranking and one renderer are what
/// restricting `ok` to mwoffliner's HTML buys.
#[test]
fn import_refuses_a_zim_another_scraper_wrote_before_doing_any_work() {
    let dir = tempfile::tempdir().unwrap();
    let sotoki = build(dir.path(), "s.zim", "stack_en_all", "Stack Exchange", Some("sotoki 1.3"), &["Question"]);
    let err = import(&sotoki, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap_err().to_string();
    assert!(err.contains("sotoki 1.3"), "{err}");
    assert!(!index_dir_for(&sotoki).exists(), "refused before any work: no index directory");

    let bare = build(dir.path(), "b.zim", "wikipedia_en_top", "Best of Wikipedia", None, &["Albert Einstein"]);
    let err = import(&bare, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap_err().to_string();
    assert!(err.contains("no Scraper metadata"), "{err}");
}
