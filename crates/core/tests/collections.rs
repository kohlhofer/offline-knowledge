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
    let set = Collections::open(&[a, b], None, &[]).unwrap();

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

    let set = Collections::open(&[a.clone(), b.clone()], Some("wiktionary"), &[]).unwrap();
    assert_eq!(set.default().label().unwrap().as_str(), "wiktionary");
    assert_eq!(set.default_index(), 1);

    let err = Collections::open(&[a, b], Some("archlinux"), &[]).err().unwrap().to_string();
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
    let set = Collections::open(&[a, b], None, &[]).unwrap();

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

    // A fourth, on the path every index in the wild is on: no `name` and no
    // `scraper` in `meta.json`, so the refusal comes from the ZIM when a
    // label is resolved, not from the index at load.
    let legacy = imported(dir.path(), "legacy.zim", "wikivoyage_en_all", "Wikivoyage", &["Ulm"]);
    rewrite_meta(&legacy, |m| {
        m.name = None;
        m.scraper = None;
    });
    build(dir.path(), "legacy.zim", "wikivoyage_en_all", "Wikivoyage", Some("sotoki 1.3"), &["Ulm"]);

    // `resolve_labels`, because two of the four are only found out when a
    // label is resolved: the legacy one's scraper and, in the test below, a
    // label already taken.
    let set = Collections::open(&[raw.clone(), good, scraped.clone(), legacy.clone()], None, &[]).unwrap().resolve_labels().unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(set.default().label().unwrap().as_str(), "wikipedia");

    assert_eq!(set.skipped().len(), 3, "{:?}", set.skipped().iter().map(|s| &s.reason).collect::<Vec<_>>());
    let skipped = |path: &Path| set.skipped().iter().find(|s| s.path == path).unwrap();
    let raw_reason = skipped(&raw).reason.clone();
    assert!(raw_reason.contains("ok --zim") && raw_reason.contains("import"), "the reason names the command that fixes it: {raw_reason}");
    let scraped_reason = skipped(&scraped).reason.clone();
    assert!(scraped_reason.contains("sotoki 1.3"), "{scraped_reason}");
    // The class travels with the reason, whichever of the two paths found it:
    // a browser page shows the class, because the reason names a path.
    assert_eq!(skipped(&raw).kind, ok_core::SkipKind::NotImported);
    assert_eq!(skipped(&scraped).kind, ok_core::SkipKind::Scraper);
    assert_eq!(skipped(&legacy).kind, ok_core::SkipKind::Scraper, "{}", skipped(&legacy).reason);
}


/// Skipping is per file, but an empty set is not a working process — and the
/// error is the last place those reasons can still be said. `ok --zim
/// fake.zim suggest x` is the commonest first-run mistake there is, so it
/// names the file and a command that works, not just that nothing loaded.
#[test]
fn nothing_loadable_is_an_error_that_still_names_the_file_and_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let raw = build(dir.path(), "raw.zim", "wikipedia_en_top", "Best of Wikipedia", Some("mwoffliner 1.17.5"), &["Albert Einstein"]);
    let err = Collections::open(std::slice::from_ref(&raw), None, &[]).err().unwrap();
    assert!(matches!(err, ok_core::Error::NoCollections { .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("raw.zim"), "the file that went nowhere is named: {message}");
    assert!(message.contains("ok --zim") && message.contains("import"), "and the command that fixes it: {message}");

    // The same for a `--collection` nobody can satisfy: the label it could
    // not find, what is loaded, and what was skipped on the way.
    let good = imported(dir.path(), "good.zim", "wiktionary_en-simple_all", "Wiktionary", &["Mercury"]);
    let err = Collections::open(&[raw, good], Some("wikipedia"), &[]).err().unwrap().to_string();
    assert!(err.contains("wikipedia") && err.contains("wiktionary"), "{err}");
    assert!(err.contains("raw.zim"), "a skipped file is named here too: {err}");
}

/// Every line that reports a skip names the file itself, so the reason does
/// not open with the same path again: `NotImported`'s message otherwise
/// repeats a 110-character absolute path three times over in one line.
#[test]
fn a_skip_reason_does_not_repeat_the_path_the_line_already_names() {
    let dir = tempfile::tempdir().unwrap();
    let raw = build(dir.path(), "raw.zim", "wikipedia_en_top", "Best of Wikipedia", Some("mwoffliner 1.17.5"), &["Albert Einstein"]);
    let good = imported(dir.path(), "good.zim", "wiktionary_en-simple_all", "Wiktionary", &["Mercury"]);
    let set = Collections::open(&[raw.clone(), good], None, &[]).unwrap();

    let skipped = &set.skipped()[0];
    let shown = raw.display().to_string();
    assert!(!skipped.reason.starts_with(&shown), "the reason does not open with the path: {}", skipped.reason);
    assert!(skipped.reason.contains("ok --zim"), "the command that fixes it still carries it: {}", skipped.reason);
    assert_eq!(skipped.kind, ok_core::SkipKind::NotImported);
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
    let set = Collections::open(&[en, de.clone()], None, &[]).unwrap().resolve_labels().unwrap();

    assert_eq!(set.len(), 1);
    assert_eq!(set.skipped().len(), 1);
    assert_eq!(set.skipped()[0].path, de);
    let reason = &set.skipped()[0].reason;
    assert!(reason.contains("en.zim"), "the reason names the file that took the label: {reason}");
    assert!(reason.contains("wikipedia"), "{reason}");
    // A browser page cannot show the reason (it carries a path), so the
    // class carries the label and the winner's filename on their own.
    assert_eq!(
        set.skipped()[0].kind,
        ok_core::SkipKind::Duplicate { label: "wikipedia".to_string(), winner: "en.zim".to_string() }
    );
}


/// A one-shot pays for no label it does not use. `Collections::open` forced
/// every label whenever more than one path was given, purely to check
/// uniqueness: +3.1 ms on `--zim data/ suggest pac` against the same query
/// on one file, 15x what a suggestion itself costs, on every invocation
/// forever, and the appliance ships `OK_ZIM=/data`. A ZIM whose file is gone
/// is the executable form of "no ZIM is opened": its `meta.json` still
/// loads, and it is dropped only once a label is actually wanted.
#[test]
fn open_resolves_no_label_and_resolve_labels_is_where_an_unusable_one_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let a = imported(dir.path(), "a.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let gone = imported(dir.path(), "gone.zim", "wiktionary_en-simple_all", "Wiktionary", &["Mercury"]);
    rewrite_meta(&a, |m| {
        m.name = None;
        m.scraper = None;
    });
    rewrite_meta(&gone, |m| {
        m.name = None;
        m.scraper = None;
    });
    std::fs::remove_file(&gone).unwrap();

    let set = Collections::open(&[a.clone(), gone.clone()], None, &[]).unwrap();
    assert_eq!(set.len(), 2, "both loaded: `open` reads meta.json and never the ZIM");
    assert!(set.skipped().is_empty());
    assert_eq!(set.default().zim_path(), a, "and the default is the first path given");

    let set = Collections::open(&[a.clone(), gone.clone()], None, &[]).unwrap().resolve_labels().unwrap();
    assert_eq!(set.len(), 1, "a label is wanted now, so the one that cannot give one is dropped");
    assert_eq!(set.skipped()[0].path, gone);
    assert_eq!(set.default().label().unwrap().as_str(), "wikipedia");

    // Naming a default has to find it, so that path resolves every label
    // itself — and the set it hands back needs no second pass.
    let set = Collections::open(&[a, gone.clone()], Some("wikipedia"), &[]).unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(set.skipped()[0].path, gone);
    assert_eq!(set.resolve_labels().unwrap().len(), 1, "idempotent");
}

/// `default()` has to name the same collection whichever frontend asks.
/// `open` left a label nothing could route in place while `resolve_labels`
/// dropped it, so `ok suggest`, `search` and `show` answered from a file
/// `ok collections`, `serve`, `mcp` and `tui` all skipped — and the label a
/// URL would have used belonged to someone else entirely.
#[test]
fn the_default_is_the_same_collection_whether_or_not_every_label_was_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let squatter = imported(dir.path(), "a.zim", "wiki_en_all", "Squatter", &["Query"]);
    let good = imported(dir.path(), "m.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    let gone = imported(dir.path(), "z.zim", "wiktionary_en-simple_all", "Wiktionary", &["Mercury"]);

    // A label `meta.json` already records costs nothing to judge, so `open`
    // judges it rather than leaving it to the pass a one-shot never runs.
    let cheap = Collections::open(&[squatter.clone(), good.clone(), gone.clone()], None, &["wiki"]).unwrap();
    assert_eq!(cheap.len(), 2);
    assert_eq!(cheap.skipped()[0].path, squatter);
    assert_eq!(cheap.default().zim_path(), good, "the one-shot lands where a routed frontend does");
    let resolved = Collections::open(&[squatter.clone(), good.clone(), gone.clone()], None, &["wiki"]).unwrap();
    assert_eq!(resolved.resolve_labels().unwrap().default().zim_path(), good);

    // And on the path every index in the wild is on, where `meta.json`
    // records no label and reading one costs a ZIM open: the default
    // resolves labels forward and stops at the first it could route to.
    for zim in [&squatter, &good, &gone] {
        rewrite_meta(zim, |m| {
            m.name = None;
            m.scraper = None;
        });
    }
    let bytes = std::fs::read(&gone).unwrap();
    std::fs::remove_file(&gone).unwrap();
    let legacy = Collections::open(&[squatter, good.clone(), gone.clone()], None, &["wiki"]).unwrap();
    assert_eq!(legacy.len(), 3, "nothing is dropped here: judging those labels costs a ZIM open each");
    assert_eq!(legacy.default().zim_path(), good, "but the default is still one a URL can name");
    assert_eq!(legacy.default_index(), 1);

    std::fs::write(&gone, &bytes).unwrap();
    assert!(legacy.at(2).unwrap().label().is_ok(), "the label past the default was never read: one label, not N");
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

    let set = Collections::open(&[a], None, &[]).unwrap();
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

    let set = Collections::open(std::slice::from_ref(&a), None, &[]).unwrap();
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
    let set = Collections::open(&[a, b], None, &[]).unwrap();

    let hits: Vec<&str> = set.exact_elsewhere(0, "Mercury").iter().map(|c| c.label().unwrap().as_str()).collect();
    assert_eq!(hits, ["wiktionary"]);
    assert!(set.exact_elsewhere(1, "Mercury").is_empty(), "a collection is never its own hint");
    assert!(set.exact_elsewhere(0, "Merc").is_empty(), "a prefix-only match must not fire the hint");
    assert!(set.exact_elsewhere(0, "Nothing at all").is_empty());
    // A control character is not part of any title. Left in, a `\0` became
    // part of the lookup bound, where it collided with the separator the
    // title index puts between a key and its entry: `Mercury\0` matched
    // `Mercury` and fired a hint whose link was the caller's own unusable
    // path (`/wiktionary/Mercury%00`).
    assert!(set.exact_elsewhere(0, "Mercury\0").is_empty(), "a null in the query is not an exact title");
    assert!(set.exact_elsewhere(0, "Mercury\u{1b}[2J").is_empty());
    assert!(set.iter().all(|c| !c.is_open()), "the probe opens titles.fst, never a Library");
}

/// The gate that keeps one parser, one ranking and one renderer honest has to
/// hold on the path a reader actually takes. `load` can only check the scraper
/// when the index recorded one, and resolving a label only happens when
/// something shows a label, so a single collection on an index that predates
/// `IndexMeta.scraper` — every index built before this version — was read as
/// mwoffliner's by `ok suggest`, `search`, `show` and `tui`, while `ok serve`
/// refused to start on it.
#[test]
fn a_single_legacy_collection_still_refuses_a_zim_another_scraper_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let sotoki = imported(dir.path(), "s.zim", "stack_en_all", "Stack Exchange", &["Question"]);
    rewrite_meta(&sotoki, |m| {
        m.name = None;
        m.scraper = None;
    });
    // The same ZIM with only its `Scraper` changed, and changed to a string
    // the same length, so the file's size and uuid still match its index:
    // `Library::open`'s own check must not be what refuses this.
    build(dir.path(), "s.zim", "stack_en_all", "Stack Exchange", Some("sotoki-zim 1.17.5"), &["Question"]);

    // Alone, so nothing resolves its label: `open` and `resolve_labels` both
    // keep it, exactly as they keep a ZIM carrying no `Name` at all.
    let set = Collections::open(std::slice::from_ref(&sotoki), None, &[]).unwrap().resolve_labels().unwrap();
    assert_eq!(set.len(), 1);

    let err = set.default().library().err().unwrap().to_string();
    assert!(err.contains("sotoki-zim 1.17.5"), "the refusal names the scraper it found: {err}");
    assert!(!set.default().is_open());
    assert_eq!(set.default().failure(), Some(err.as_str()), "and it is remembered, like any other failed open");

    // A ZIM mwoffliner did write still opens, on the same legacy path.
    let good = imported(dir.path(), "g.zim", "wikipedia_en_top", "Best of Wikipedia", &["Albert Einstein"]);
    rewrite_meta(&good, |m| {
        m.name = None;
        m.scraper = None;
    });
    let set = Collections::open(std::slice::from_ref(&good), None, &[]).unwrap();
    assert_eq!(set.default().library().unwrap().article_count(), 1);
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
