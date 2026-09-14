use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ok_core::import::{ImportOptions, import};
use ok_zim::write::ZimBuilder;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<html><body><h1>{title}</h1><div id="mw-content-text"><div class="mw-parser-output">{body}</div></div></body></html>"#
    )
}

fn app() -> (tempfile::TempDir, App) {
    let long: String = (0..60).map(|i| format!("<p>Filler paragraph {i} about history.</p>")).collect();
    let bytes = ZimBuilder::new()
        .article(
            "Albert_Einstein",
            "Albert Einstein",
            &page(
                "Albert Einstein",
                &format!(
                    r##"<p>A <a href="Physicist">physicist</a> known for <a href="Theory_of_relativity">relativity</a> and <a href="Nowhere">missing</a>.</p>
                    {long}
                    <div class="mw-heading mw-heading2"><h2 id="Legacy">Legacy</h2></div><p>See <a href="#Legacy">here</a> and <a href="https://example.org">out</a>.</p>"##
                ),
            ),
        )
        .article("Physicist", "Physicist", &page("Physicist", r#"<p>Studies physics. See <a href="Albert_Einstein">Einstein</a>.</p>"#))
        .article("Theory_of_relativity", "Theory of relativity", &page("Theory of relativity", "<p>Space and time.</p>"))
        .article(
            "Einstein_legacy",
            "Einstein legacy",
            r#"<html><head><meta http-equiv="refresh" content="0;URL='./Albert_Einstein#Legacy'" /></head></html>"#,
        )
        .metadata("Title", "Tiny")
        .build();
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("t.zim");
    std::fs::write(&zim, bytes).unwrap();
    import(&zim, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    let library = Library::open(&zim).unwrap();
    (dir, App::new(library, 80, 24))
}

fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn title(app: &App) -> &str {
    &app.article.as_ref().expect("an article is open").title
}

#[test]
fn type_open_follow_back_forward() {
    let (_d, mut app) = app();
    type_text(&mut app, "alb");
    assert_eq!(app.suggestions[0].title, "Albert Einstein");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Article);
    assert_eq!(title(&app), "Albert Einstein");

    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    assert_eq!(title(&app), "Physicist");

    press(&mut app, KeyCode::Backspace);
    assert_eq!(title(&app), "Albert Einstein");
    assert_eq!(app.article.as_ref().unwrap().selected_link, Some(0), "back restores the selected link");
    press(&mut app, KeyCode::Right);
    assert_eq!(title(&app), "Physicist");
}

#[test]
fn missing_external_and_anchor_links_explain_themselves() {
    let (_d, mut app) = app();
    type_text(&mut app, "albert einstein");
    press(&mut app, KeyCode::Enter);
    for _ in 0..3 {
        press(&mut app, KeyCode::Tab);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(title(&app), "Albert Einstein");
    assert!(app.status.contains("not in this collection"), "{}", app.status);

    press(&mut app, KeyCode::Char('G'));
    let links = app.article.as_ref().unwrap().laid.links.len();
    for _ in 0..links {
        press(&mut app, KeyCode::Tab);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.status.contains("external link"), "{}", app.status);

    // Back one link to the in-page "#Legacy" anchor, from the top of the article.
    press(&mut app, KeyCode::BackTab);
    let here = app.article.as_ref().unwrap().selected_link.unwrap();
    assert_eq!(app.article.as_ref().unwrap().laid.links[here].text, "here");
    app.article.as_mut().unwrap().scroll = 0;
    press(&mut app, KeyCode::Enter);
    let view = app.article.as_ref().unwrap();
    assert_eq!(title(&app), "Albert Einstein");
    assert_eq!(view.scroll, view.laid.section_line("Legacy").unwrap());
}

#[test]
fn resizing_rewraps_without_leaving_the_search_screen() {
    let (_d, mut app) = app();
    type_text(&mut app, "albert");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('/'));
    app.resize(60, 20);
    assert_eq!(app.screen, Screen::Search);
    assert!(app.article.as_ref().unwrap().laid.lines.iter().all(|l| layout::display_width(&l.text()) <= 60));
}

#[test]
fn section_redirects_open_at_the_section_and_outline_jumps() {
    let (_d, mut app) = app();
    type_text(&mut app, "einstein leg");
    assert_eq!(app.suggestions[0].fragment.as_deref(), Some("Legacy"));
    press(&mut app, KeyCode::Enter);
    let view = app.article.as_ref().unwrap();
    let legacy = view.laid.section_line("Legacy").unwrap();
    let max_scroll = view.laid.lines.len().saturating_sub(app.page_height());
    assert_eq!(view.scroll, legacy.min(max_scroll));

    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.overlay, Overlay::Outline(0));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.overlay, Overlay::None);
    assert!(app.article.as_ref().unwrap().scroll > 0);
}

#[test]
fn full_text_search_from_the_search_screen() {
    let (_d, mut app) = app();
    type_text(&mut app, "space time");
    assert!(app.suggestions.is_empty());
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.results[0].title, "Theory of relativity");
    press(&mut app, KeyCode::Enter);
    assert_eq!(title(&app), "Theory of relativity");
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.screen, Screen::Search);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.screen, Screen::Article);
}

#[test]
fn renders_every_screen_without_panicking_at_small_and_large_sizes() {
    let (_d, mut app) = app();
    for (w, h) in [(20u16, 6u16), (80, 24), (200, 60)] {
        app.resize(w, h);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        type_text(&mut app, "a");
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        press(&mut app, KeyCode::Enter);
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        press(&mut app, KeyCode::Char('o'));
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('?'));
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('/'));
    }
    let buffer = {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.resize(80, 24);
        type_text(&mut app, "phys");
        terminal.draw(|f| render::draw(f, &mut app)).unwrap();
        terminal.backend().buffer().clone()
    };
    let screen: String = buffer.content().iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("Physicist"));
}

#[test]
fn ctrl_c_quits_from_anywhere() {
    let (_d, mut app) = app();
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.quit);
}
