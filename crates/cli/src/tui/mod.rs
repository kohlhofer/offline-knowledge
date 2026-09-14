//! The terminal reader: search as you type, read, follow links, go back.

pub mod layout;
mod render;

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ok_core::document::Link;
use ok_core::{Library, SearchResult, Suggestion, Target};

use layout::{Laid, layout};

const SUGGESTIONS: usize = 12;
const RESULTS: usize = 30;
/// Longest line length for reading; wider terminals get margins.
const MAX_TEXT_WIDTH: u16 = 100;

pub fn run(library: Library) -> Result<()> {
    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let mut app = App::new(library, size.width, size.height);
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|frame| render::draw(frame, &mut app))?;
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                Event::Resize(w, h) => app.resize(w, h),
                _ => {}
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Search,
    Article,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Outline(usize),
    Help,
}

pub struct ArticleView {
    pub entry: u32,
    pub title: String,
    pub laid: Laid,
    pub scroll: usize,
    pub selected_link: Option<usize>,
}

#[derive(Clone, Copy)]
struct Place {
    entry: u32,
    scroll: usize,
    selected_link: Option<usize>,
}

pub struct App {
    pub library: Library,
    pub screen: Screen,
    pub overlay: Overlay,
    pub query: String,
    pub suggestions: Vec<Suggestion>,
    pub results: Vec<SearchResult>,
    /// Index into suggestions followed by results.
    pub selected: usize,
    pub article: Option<ArticleView>,
    back: Vec<Place>,
    forward: Vec<Place>,
    pub status: String,
    pub timing: Option<(&'static str, Duration)>,
    pub width: u16,
    pub height: u16,
    pub quit: bool,
    seed: u64,
}

impl App {
    pub fn new(library: Library, width: u16, height: u16) -> App {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        let status = format!("{} articles · type to search · Ctrl-R random · ? help", library.article_count());
        App {
            library,
            screen: Screen::Search,
            overlay: Overlay::None,
            query: String::new(),
            suggestions: Vec::new(),
            results: Vec::new(),
            selected: 0,
            article: None,
            back: Vec::new(),
            forward: Vec::new(),
            status,
            timing: None,
            width,
            height,
            quit: false,
            seed,
        }
    }

    pub fn text_width(&self) -> u16 {
        self.width.saturating_sub(4).clamp(20, MAX_TEXT_WIDTH)
    }

    /// Lines available for article text (everything but the status bar).
    pub fn page_height(&self) -> usize {
        usize::from(self.height.saturating_sub(2)).max(1)
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        let old = self.text_width();
        self.width = width;
        self.height = height;
        if old != self.text_width() {
            if let Some(view) = &self.article {
                // Re-wrap at the new width, keeping the reader in the same section
                // and on whatever screen they were looking at.
                let place = Place { entry: view.entry, scroll: 0, selected_link: None };
                let section = view.laid.section_at(view.scroll).and_then(|s| s.anchor.clone());
                let (screen, overlay, status, timing) = (self.screen, self.overlay, self.status.clone(), self.timing);
                self.load(place, section);
                (self.screen, self.overlay, self.status, self.timing) = (screen, overlay, status, timing);
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit = true;
            return;
        }
        if ctrl && key.code == KeyCode::Char('r') {
            self.random();
            return;
        }
        match self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return;
            }
            Overlay::Outline(i) => {
                self.outline_key(key, i);
                return;
            }
            Overlay::None => {}
        }
        match self.screen {
            Screen::Search => self.search_key(key),
            Screen::Article => self.article_key(key),
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        let total = self.suggestions.len() + self.results.len();
        match key.code {
            KeyCode::Esc => {
                if self.article.is_some() {
                    self.screen = Screen::Article;
                } else if self.query.is_empty() {
                    self.quit = true;
                } else {
                    self.set_query(String::new());
                }
            }
            KeyCode::Enter => {
                if total == 0 && !self.query.trim().is_empty() {
                    self.full_text();
                } else {
                    self.open_selected();
                }
            }
            KeyCode::Tab => self.full_text(),
            KeyCode::Down => self.selected = (self.selected + 1).min(total.saturating_sub(1)),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(total.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Backspace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            KeyCode::Char('?') if self.query.is_empty() => self.overlay = Overlay::Help,
            KeyCode::Char(c) => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(q);
            }
            _ => {}
        }
    }

    fn set_query(&mut self, query: String) {
        self.query = query;
        self.results.clear();
        self.selected = 0;
        let started = Instant::now();
        match self.library.suggest(&self.query, SUGGESTIONS) {
            Ok(s) => self.suggestions = s,
            Err(e) => self.status = format!("suggest failed: {e}"),
        }
        self.timing = Some(("suggest", started.elapsed()));
    }

    fn full_text(&mut self) {
        if self.query.trim().is_empty() {
            return;
        }
        let started = Instant::now();
        match self.library.search(&self.query, RESULTS) {
            Ok(r) => {
                self.status = if r.is_empty() { format!("no articles mention \"{}\"", self.query) } else { String::new() };
                self.results = r;
                self.selected = self.suggestions.len();
            }
            Err(e) => self.status = format!("search failed: {e}"),
        }
        self.timing = Some(("search", started.elapsed()));
    }

    fn open_selected(&mut self) {
        let target = if self.selected < self.suggestions.len() {
            let s = &self.suggestions[self.selected];
            Some(Target { entry: s.article, fragment: s.fragment.clone() })
        } else {
            self.results.get(self.selected - self.suggestions.len()).map(|r| Target { entry: r.article, fragment: None })
        };
        if let Some(target) = target {
            self.navigate(target);
        }
    }

    fn article_key(&mut self, key: KeyEvent) {
        let page = self.page_height();
        let Some(view) = &mut self.article else {
            self.screen = Screen::Search;
            return;
        };
        let max_scroll = view.laid.lines.len().saturating_sub(page);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('/') | KeyCode::Char('s') => {
                self.screen = Screen::Search;
                self.set_query(String::new());
            }
            KeyCode::Esc => self.screen = Screen::Search,
            KeyCode::Char('j') | KeyCode::Down => view.scroll = (view.scroll + 1).min(max_scroll),
            KeyCode::Char('k') | KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown | KeyCode::Char('d') => {
                view.scroll = (view.scroll + page.saturating_sub(2).max(1)).min(max_scroll)
            }
            KeyCode::PageUp | KeyCode::Char('u') => view.scroll = view.scroll.saturating_sub(page.saturating_sub(2).max(1)),
            KeyCode::Char('g') | KeyCode::Home => view.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => view.scroll = max_scroll,
            KeyCode::Tab | KeyCode::Char('n') => select_link(view, page, true),
            KeyCode::BackTab | KeyCode::Char('N') => select_link(view, page, false),
            KeyCode::Enter => self.follow_selected(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('b') => self.go_back(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('f') => self.go_forward(),
            KeyCode::Char('o') => {
                let current = view.laid.sections.iter().rposition(|s| s.line <= view.scroll).unwrap_or(0);
                self.overlay = Overlay::Outline(current);
            }
            KeyCode::Char('r') => self.random(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }

    fn outline_key(&mut self, key: KeyEvent, selected: usize) {
        let Some(view) = &mut self.article else {
            self.overlay = Overlay::None;
            return;
        };
        let count = view.laid.sections.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.overlay = Overlay::Outline((selected + 1).min(count.saturating_sub(1))),
            KeyCode::Up | KeyCode::Char('k') => self.overlay = Overlay::Outline(selected.saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(section) = view.laid.sections.get(selected) {
                    view.scroll = section.line;
                }
                self.overlay = Overlay::None;
            }
            _ => self.overlay = Overlay::None,
        }
    }

    fn follow_selected(&mut self) {
        let Some(view) = &mut self.article else { return };
        let Some(spot) = view.selected_link.and_then(|i| view.laid.links.get(i)) else {
            self.status = "Tab selects a link, Enter follows it".into();
            return;
        };
        match spot.link.clone() {
            Link::Article { entry, fragment } => self.navigate(Target { entry, fragment }),
            Link::Anchor { fragment } => match view.laid.section_line(&fragment) {
                Some(line) => view.scroll = line,
                None => self.status = format!("no section \"{fragment}\" here"),
            },
            Link::Missing { path } => self.status = format!("\"{}\" is not in this collection", path.replace('_', " ")),
            Link::External { url } => self.status = format!("external link (offline): {}", layout::sanitize(&url)),
        }
    }

    /// Opens a target, remembering where we were.
    pub fn navigate(&mut self, target: Target) {
        if let Some(view) = &self.article {
            self.back.push(Place { entry: view.entry, scroll: view.scroll, selected_link: view.selected_link });
            self.forward.clear();
        }
        self.load(Place { entry: target.entry, scroll: 0, selected_link: None }, target.fragment);
    }

    fn go_back(&mut self) {
        let Some(place) = self.back.pop() else {
            self.status = "nothing to go back to".into();
            return;
        };
        if let Some(view) = &self.article {
            self.forward.push(Place { entry: view.entry, scroll: view.scroll, selected_link: view.selected_link });
        }
        self.load(place, None);
    }

    fn go_forward(&mut self) {
        let Some(place) = self.forward.pop() else { return };
        if let Some(view) = &self.article {
            self.back.push(Place { entry: view.entry, scroll: view.scroll, selected_link: view.selected_link });
        }
        self.load(place, None);
    }

    fn random(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        if let Some(entry) = self.library.random_article(self.seed) {
            self.navigate(Target { entry, fragment: None });
        }
    }

    fn load(&mut self, place: Place, fragment: Option<String>) {
        let started = Instant::now();
        let doc = match self.library.article(place.entry) {
            Ok(doc) => doc,
            Err(e) => {
                self.status = format!("could not open article: {e}");
                return;
            }
        };
        let laid = layout(&doc, self.text_width());
        let mut scroll = place.scroll;
        if let Some(fragment) = &fragment {
            match laid.section_line(fragment) {
                Some(line) => scroll = line,
                None => self.status = format!("section \"{}\" not found", fragment.replace('_', " ")),
            }
        }
        let max_scroll = laid.lines.len().saturating_sub(self.page_height());
        self.article = Some(ArticleView {
            entry: doc.entry,
            title: layout::sanitize(&doc.title),
            laid,
            scroll: scroll.min(max_scroll),
            selected_link: place.selected_link,
        });
        self.timing = Some(("open", started.elapsed()));
        self.screen = Screen::Article;
        self.overlay = Overlay::None;
        if fragment.is_none() {
            self.status.clear();
        }
    }
}

/// Moves the link selection forward or back, starting from the first visible
/// link when nothing visible is selected, and scrolls it into view.
fn select_link(view: &mut ArticleView, page: usize, forward: bool) {
    let links = &view.laid.links;
    if links.is_empty() {
        return;
    }
    let visible = |i: usize| (view.scroll..view.scroll + page).contains(&links[i].line);
    let next = match view.selected_link.filter(|&i| visible(i)) {
        Some(i) if forward => (i + 1).min(links.len() - 1),
        Some(i) => i.saturating_sub(1),
        None if forward => links.iter().position(|l| l.line >= view.scroll).unwrap_or(links.len() - 1),
        None => links.iter().rposition(|l| l.line < view.scroll + page).unwrap_or(0),
    };
    view.selected_link = Some(next);
    let line = links[next].line;
    if line < view.scroll {
        view.scroll = line;
    } else if line >= view.scroll + page {
        view.scroll = line + 1 - page;
    }
}

#[cfg(test)]
mod tests;
