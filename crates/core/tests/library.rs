use std::io::Write;
use std::sync::Mutex;

use ok_core::document::{Block, Link, Target};
use ok_core::import::{ImportOptions, Progress, import};
use ok_core::{Error, Library, Resolution};
use ok_zim::write::ZimBuilder;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><title>{title}</title></head><body><main>
        <h1 id="firstHeading">{title}</h1>
        <div id="mw-content-text"><div class="mw-parser-output">{body}</div></div>
        </main></body></html>"#
    )
}

fn wiki() -> Vec<u8> {
    ZimBuilder::new()
        .article(
            "Albert_Einstein",
            "Albert Einstein",
            &page(
                "Albert Einstein",
                r#"<p><b>Albert Einstein</b> was a <a href="Physicist">physicist</a> who developed
                <a href="Theory_of_relativity">relativity</a>.</p>
                <div class="mw-heading mw-heading2"><h2 id="Life">Life</h2></div><p>Born in <a href="Ulm">Ulm</a>.</p>"#,
            ),
        )
        .article(
            "Theory_of_relativity",
            "Theory of relativity",
            &page("Theory of relativity", r#"<p>Developed by <a href="Einstein">Einstein</a>, a <a href="Physicist">physicist</a>.</p>"#),
        )
        .article("Physicist", "Physicist", &page("Physicist", "<p>A scientist who studies physics, like Isaac Newton.</p>"))
        .article(
            "Isaac_Newton",
            "Isaac Newton",
            &page("Isaac Newton", r#"<p>A <a href="Physicist">physicist</a> known for gravity.</p>"#),
        )
        .article(
            "Einstein_early_life",
            "Einstein early life",
            r#"<html><head><meta http-equiv="refresh" content="0;URL='./Albert_Einstein#Life'" /></head><body></body></html>"#,
        )
        .redirect("Einstein", "Einstein", "Albert_Einstein")
        .redirect("Relativity", "Relativity", "Theory_of_relativity")
        .resource("_res_/style.css", "text/css", b"p{}")
        .metadata("Title", "Tiny wiki")
        .main_page("Albert_Einstein")
        .build()
}

fn imported() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("tiny.zim");
    std::fs::File::create(&zim).unwrap().write_all(&wiki()).unwrap();
    let events = Mutex::new(Vec::new());
    let report = import(&zim, &ImportOptions { heap_bytes: 30_000_000 }, &|p| events.lock().unwrap().push(p)).unwrap();
    assert_eq!(report.meta.articles, 4);
    assert_eq!(report.meta.redirects, 2);
    assert_eq!(report.meta.section_redirects, 1);
    assert_eq!(report.meta.missing_links, 1, "Ulm is not in the file");
    assert!(events.lock().unwrap().iter().any(|p| matches!(p, Progress::Parsed { done: 4, total: 4 })));
    assert!(!dir.path().join("tiny.okx.partial").exists());
    let library = Library::open(&zim).unwrap();
    (dir, library)
}

#[test]
fn opening_without_import_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("tiny.zim");
    std::fs::write(&zim, wiki()).unwrap();
    let err = Library::open(&zim).err().expect("not imported");
    assert!(matches!(err, Error::NotImported(_)));
    assert!(err.to_string().contains("ok import"));
}

#[test]
fn suggestions_rank_by_inbound_links_and_include_redirects() {
    let (_dir, lib) = imported();
    let physicist = lib.find("Physicist").unwrap().unwrap().entry;
    assert_eq!(lib.inbound(physicist), 3);

    let ein = lib.suggest("ein", 5).unwrap();
    assert_eq!(ein.len(), 1, "the redirect and the section redirect both lead to one article: {ein:?}");
    assert_eq!(ein[0].title, "Albert Einstein");
    assert_eq!(ein[0].matched.as_deref(), Some("Einstein"));

    let early = lib.suggest("einstein ear", 5).unwrap();
    assert_eq!(early[0].title, "Albert Einstein");
    assert_eq!(early[0].fragment.as_deref(), Some("Life"));

    let p = lib.suggest("P", 5).unwrap();
    assert_eq!(p[0].title, "Physicist");
    assert!(lib.suggest("zz", 5).unwrap().is_empty());
}

#[test]
fn full_text_finds_body_words_and_redirect_titles() {
    let (_dir, lib) = imported();
    let gravity = lib.search("gravity", 5).unwrap();
    assert_eq!(gravity[0].title, "Isaac Newton");
    assert!(gravity[0].summary.contains("known for gravity"));
    let relativity = lib.search("relativity", 5).unwrap();
    assert_eq!(relativity[0].title, "Theory of relativity");
    assert_eq!(lib.search("early life", 5).unwrap()[0].title, "Albert Einstein", "section redirect titles are searchable");
}

#[test]
fn articles_parse_with_links_resolved_through_redirects() {
    let (_dir, lib) = imported();
    let relativity = lib.find("Relativity").unwrap().unwrap().entry;
    let doc = lib.article(relativity).unwrap();
    assert_eq!(doc.title, "Theory of relativity");
    let einstein = lib.find("Albert_Einstein").unwrap().unwrap().entry;
    let stub = lib.find("Einstein_early_life").unwrap().unwrap();
    assert_eq!(stub.entry, einstein);
    assert_eq!(stub.fragment.as_deref(), Some("Life"));
    assert_eq!(lib.article(lib.archive().find_by_path(b'C', b"Einstein_early_life").unwrap().unwrap()).unwrap().title, "Albert Einstein");
    let Block::Paragraph { content } = &doc.sections[0].blocks[0] else { panic!() };
    let link = content.iter().find(|i| i.text == "Einstein").unwrap();
    assert_eq!(link.link, Some(Link::Article { entry: einstein, fragment: None }));

    let einstein_doc = lib.article(einstein).unwrap();
    assert_eq!(einstein_doc.sections[1].heading, "Life");
    assert!(einstein_doc.links().any(|l| matches!(l, Link::Missing { path } if path == "Ulm")));

    let css = lib.archive().find_by_path(b'C', b"_res_/style.css").unwrap().unwrap();
    assert!(matches!(lib.article(css), Err(Error::NotArticle(_))));
}

#[test]
fn resolve_title_is_exact_and_never_falls_back_silently() {
    let (_dir, lib) = imported();
    let einstein = lib.find("Albert_Einstein").unwrap().unwrap().entry;

    assert_eq!(lib.resolve_title("Albert_Einstein").unwrap(), Resolution::Found(Target { entry: einstein, fragment: None }), "exact path");

    // Case-folding exact match through the redirect's title, not the path.
    assert_eq!(lib.resolve_title("einstein").unwrap(), Resolution::Found(Target { entry: einstein, fragment: None }));

    // A section redirect's title resolves exactly, fragment included.
    assert_eq!(
        lib.resolve_title("Einstein early life").unwrap(),
        Resolution::Found(Target { entry: einstein, fragment: Some("Life".into()) })
    );

    match lib.resolve_title("zzzzz not a title").unwrap() {
        Resolution::NotFound { suggestions } => assert!(suggestions.is_empty(), "{suggestions:?}"),
        other => panic!("expected NotFound, got {other:?}"),
    }
    // A prefix match ("Einst...") is not an exact match: no silent fallback.
    match lib.resolve_title("Einst").unwrap() {
        Resolution::NotFound { suggestions } => assert!(!suggestions.is_empty()),
        other => panic!("expected NotFound with suggestions, got {other:?}"),
    }
}

#[test]
fn path_round_trips_a_known_entry() {
    let (_dir, lib) = imported();
    let einstein = lib.find("Albert_Einstein").unwrap().unwrap().entry;
    assert_eq!(lib.path(einstein).unwrap(), "Albert_Einstein");
}

#[test]
fn random_articles_are_articles() {
    let (_dir, lib) = imported();
    for seed in 0..50 {
        let entry = lib.random_article(seed).unwrap();
        assert!(lib.articles().contains(&entry));
    }
}
