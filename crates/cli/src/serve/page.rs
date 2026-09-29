//! HTML page shell and the fragments each route renders into it.
//!
//! Every ZIM-sourced string (a title, a summary, a suggestion) passes
//! through [`esc`] before it reaches a response, the same rule
//! `ok_core::html` follows for article bodies.

use ok_core::text::sanitize;
use ok_core::{Collection, Collections, Label, SkipKind, Skipped};

use serde::Serialize;

/// Sanitizes control characters, then HTML-escapes. Every piece of text this
/// module writes into a response — ZIM-sourced or a user's query string —
/// goes through this first.
fn esc(s: &str) -> String {
    ok_core::html::escape_html(&sanitize(s))
}

/// The active collection, the set around it and the labels the router
/// already resolved: everything the page chrome needs to name the brand,
/// carry the collection on every link and form, and draw the switcher.
pub struct Active<'a> {
    pub collections: &'a Collections,
    pub labels: &'a [Label],
    pub index: usize,
}

impl<'a> Active<'a> {
    pub fn collection(&self) -> &'a Collection {
        self.collections.at(self.index).expect("an index the router resolved")
    }

    /// The token in the URL, in the hidden form field and in the switcher:
    /// validated ASCII by construction, so it needs no escaping in any of
    /// the three.
    pub fn label(&self) -> &'a str {
        self.labels[self.index].as_str()
    }

    /// The path prefix this collection's article URLs hang off.
    pub fn base(&self) -> String {
        format!("/{}", self.label())
    }

    /// The ZIM's own `Title`: the brand, wherever a human name belongs.
    pub fn brand(&self) -> &'a str {
        self.collection().title()
    }

    /// The other collections holding an article with exactly this title, at
    /// most three and in load order. Unscored: it exists to add "wiktionary
    /// has it" to a miss, not to rank anything. Opens each candidate's title
    /// index and nothing else.
    pub fn elsewhere(&self, query: &str) -> Vec<&'a str> {
        self.collections
            .exact_elsewhere(self.index, query)
            .into_iter()
            .filter_map(|collection| collection.label().ok().map(|label| label.as_str()))
            .take(MAX_HINTS)
            .collect()
    }
}

/// How many other collections a miss names. Three is enough to be useful
/// and short enough to stay one sentence.
const MAX_HINTS: usize = 3;

/// The hidden field that carries the active collection out of a
/// server-rendered form. Without it a reader on a non-default collection
/// submits the header search and lands in the default one.
fn collection_field(label: &str) -> String {
    format!(r#"<input type="hidden" name="c" value="{label}">"#)
}

/// One link per collection, showing the **label** — the same token the URL
/// carries, so what you click and where you land read alike, while the
/// brand stays the header's own text. Nothing at all with one collection:
/// there is nowhere to switch to.
fn switcher(active: &Active, chrome: Chrome) -> String {
    if active.collections.len() < 2 {
        return String::new();
    }
    let links: String = active
        .labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let failed = active.collections.at(i).is_some_and(|c| c.failure().is_some());
            // `/` is in none of them, so it marks none of them current.
            let current = chrome != Chrome::Root && i == active.index;
            match (current, failed) {
                (true, false) => format!(r#"<a class="collection" href="/{label}" aria-current="page">{label}</a>"#),
                // Current and failed: `/` marks it failed, and a switcher
                // showing the one you are in as healthy while every link out
                // of it is a 503 disagrees with the page around it.
                (true, true) => format!(r#"<span class="collection failed" aria-current="page">{label} · failed</span>"#),
                // Not a link: its library could not be opened, so every
                // page under it would be a 503.
                (false, true) => format!(r#"<span class="collection failed">{label} · failed</span>"#),
                (false, false) => format!(r#"<a class="collection" href="/{label}">{label}</a>"#),
            }
        })
        .collect();
    format!(r#"<nav class="switcher" aria-label="Collections">{links}</nav>"#)
}

/// The full page: header chrome (persistent search, breadcrumb, live
/// region), `body`, and the help dialog. `page_title` becomes the `<title>`
/// tag; the active collection's brand is the search box's placeholder,
/// constant across every page of that collection. `q` prefills the search
/// box. `active` is `None` only on an error page, which belongs to no
/// collection.
pub fn shell(page_title: &str, active: Option<&Active>, q: Option<&str>, body: &str) -> String {
    shell_with(page_title, active, q, body, Chrome::Inside)
}

/// Which collection the chrome around a body belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// A page inside one collection: its brand, its home link, and its
    /// hidden `c` on the header form.
    Inside,
    /// A page whose own collection could not be opened. The switcher still
    /// shows (it is the way out, and marks that collection failed) while the
    /// home link and the search form point at `/` instead: every URL under
    /// this collection is another 503.
    Unavailable,
    /// `/` with more than one collection loaded: the list belongs to the
    /// process, not to whichever collection sorts first, so it is branded for
    /// the process and the switcher marks nothing current. The search box
    /// still names, and still searches, the default collection.
    Root,
}

/// [`shell`] with the chrome named. Kept separate so the ten pages that
/// belong to their collection say nothing about it.
pub fn shell_with(page_title: &str, active: Option<&Active>, q: Option<&str>, body: &str, chrome: Chrome) -> String {
    let inside = active.filter(|_| chrome == Chrome::Inside);
    // The collection the search box names and searches, which is the active
    // one on every page including `/`; `home_text` is what the page itself is
    // called, which on `/` is the process rather than that collection.
    let brand = active.map_or("Error", |a| a.brand());
    let home = inside.map_or_else(|| "/".to_string(), |a| a.base());
    let home_text = if chrome == Chrome::Root { "Collections" } else { brand };
    let label = active.filter(|_| chrome != Chrome::Unavailable).map_or("", |a| a.label());

    // One attribute is where app.js reads the active collection, so the
    // three requests it makes carry it too.
    let collection_attr = if label.is_empty() { String::new() } else { format!(r#" data-collection="{label}""#) };
    let collection_input = if label.is_empty() { String::new() } else { collection_field(label) };
    format!(
        r#"<!DOCTYPE html>
<html lang="en" class="no-js"{collection_attr}>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{page_title}</title>
<link rel="stylesheet" href="/static/app.css">
</head>
<body>
<header class="chrome">
<form action="/search" method="get" class="searchbar" role="search">
<a class="home-link" href="{home}">{home_text}</a>
{collection_input}<input type="text" name="q" id="search-input" value="{q}" placeholder="Search {collection_title}" aria-label="Search {collection_title}" autocomplete="off"
 aria-autocomplete="list" aria-expanded="false" role="combobox" aria-controls="suggestions">
<button type="submit">Search all text</button>
<ul id="suggestions" role="listbox" hidden></ul>
</form>
{switcher}<div id="breadcrumb" class="breadcrumb"></div>
<div id="live" aria-live="polite" class="sr-only"></div>
</header>
<main id="main">{body}</main>
<dialog id="outline" aria-label="Outline"></dialog>
<dialog id="help" aria-labelledby="help-heading">
<h2 id="help-heading">Keys</h2>
<dl>
<dt>/</dt><dd>focus search</dd>
<dt>type</dt><dd>live suggestions; Enter searches all text</dd>
<dt>o</dt><dd>outline, type to filter</dd>
<dt>n / N</dt><dd>next / previous link</dd>
<dt>r</dt><dd>random article</dd>
<dt>?</dt><dd>toggle this help</dd>
</dl>
<form method="dialog"><button>Close</button></form>
</dialog>
<script src="/static/app.js" defer></script>
</body>
</html>"#,
        page_title = esc(page_title),
        home_text = esc(home_text),
        collection_title = esc(brand),
        q = esc(q.unwrap_or_default()),
        switcher = active.map(|a| switcher(a, chrome)).unwrap_or_default(),
        body = body,
    )
}

/// The article count comes from the index's own `meta.json`, so a home page
/// opens no [`ok_core::Library`] at all.
pub fn home_body(collection: &Collection) -> String {
    format!(
        r#"<section class="home">
<p class="count">{count} articles in <strong>{title}</strong>.</p>
<p class="hint">Start typing above for titles, or press Enter to search the full text. <span class="js-only">Press <kbd>?</kbd> for keys, <kbd>r</kbd> for a random article.</span></p>
</section>"#,
        count = with_thousands(collection.article_count()),
        title = esc(collection.title()),
    )
}

/// `/` with more than one collection loaded: a row per collection, and the
/// files that could not be loaded named underneath. A ZIM dropped into the
/// directory that goes nowhere must not go nowhere silently.
pub fn collections_body(active: &Active) -> String {
    let rows: String = active
        .labels
        .iter()
        .enumerate()
        .filter_map(|(i, label)| {
            let collection = active.collections.at(i)?;
            // A failed collection is named, not linked: every page under it
            // is a 503, this one included.
            let (name, state) = match collection.failure() {
                Some(_) => (format!(r#"<span class="failed">{label}</span>"#), "failed".to_string()),

                None => (format!(r#"<a href="/{label}">{label}</a>"#), format!("{} articles", with_thousands(collection.article_count()))),
            };
            Some(format!(
                r#"<li>{name} <span class="brand">{brand}</span> <span class="count">{state}</span></li>"#,
                brand = esc(collection.title()),
            ))
        })

        .collect();
    let skipped: String = active.collections.skipped().iter().map(skipped_row).collect();
    let not_loaded = if skipped.is_empty() {
        String::new()
    } else {
        format!(r#"<h2>Not loaded</h2><ul class="skipped">{skipped}</ul>"#)
    };
    // The same teaching line a collection's own home page carries, which this
    // page dropped: the search box above works here too, on the collection it
    // names, and the keys work on every page.
    let hint = format!(
        r#"<p class="hint">Pick a collection, or search <strong>{brand}</strong> from the box above. <span class="js-only">Press <kbd>?</kbd> for keys, <kbd>r</kbd> for a random article.</span></p>"#,
        brand = esc(active.brand()),
    );
    format!(r#"<section class="collections"><h1>Collections</h1><ul class="collection-list">{rows}</ul>{not_loaded}{hint}</section>"#)
}

/// One "not loaded" row: the file's own name and the class of problem. Not
/// the reason itself — every reason names the path `--zim` was given, and a
/// response body is no place for the server's filesystem layout, while
/// "run `ok collections` for the reason" points a browser reader at a shell
/// they may not have. The startup log and `ok collections` carry the detail.
fn skipped_row(skipped: &Skipped) -> String {
    let reason = match &skipped.kind {
        SkipKind::NotImported => "no index yet; run <code>ok import</code> for it".to_string(),
        SkipKind::Scraper => "another scraper wrote it; <code>ok</code> reads mwoffliner's ZIMs".to_string(),
        SkipKind::Duplicate { label, winner } => {
            format!("its label <code>{}</code> is already {}'s", esc(label), esc(winner))
        }
        SkipKind::Unusable => "not usable; run <code>ok collections</code> for the reason".to_string(),
    };
    format!(
        r#"<li><span class="file">{file}</span> <span class="reason">{reason}</span></li>"#,
        file = esc(&skipped.path.file_name().unwrap_or(skipped.path.as_os_str()).to_string_lossy()),
    )
}

/// `50001` -> `"50,001"`. `library.article_count()` is in the tens of
/// thousands for a real collection; unbroken, it reads as noise.
pub(super) fn with_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub struct SearchRow {
    pub title: String,
    pub path: String,
    pub summary: String,
}

/// The body for `/search` with an empty or missing `q`: says nothing was
/// typed, rather than running the query and blaming the collection for not
/// mentioning an empty string.
pub fn search_prompt_body() -> String {
    r#"<section class="search-page">
<h1>Search</h1>
<p class="hint">Type a query above, then press Enter to search the full text.</p>
</section>"#
        .to_string()
}

/// `rows.len()` is this page's row count, not a corpus total — it changes
/// with `&limit=` alone, so "N results for X" read as a claim the search
/// wasn't making. Zero is the one count that's true regardless of limit
/// (no result at 30 means none at any higher limit either); above zero,
/// says what's actually true: this many are shown.
pub fn search_body(base: &str, query: &str, rows: &[SearchRow]) -> String {
    let count = rows.len();
    let heading = if count == 0 {
        format!(r#"<h1>0 results for "{}"</h1>"#, esc(query))
    } else {
        format!(r#"<h1>{count} shown for "{}"</h1>"#, esc(query))
    };
    if rows.is_empty() {
        return format!(
            r#"<section class="search-page">{heading}<p class="empty">No articles mention "{q}". Try different words, or fewer of them.</p></section>"#,
            q = esc(query)
        );
    }
    let items: String = rows
        .iter()
        .map(|r| {
            format!(
                r#"<li><a class="result" href="{href}"><span class="title">{title}</span><span class="summary">{summary}</span></a></li>"#,
                href = esc(&ok_core::html::article_href(base, &r.path, None)),
                title = esc(&r.title),
                summary = esc(&r.summary),
            )
        })
        .collect();
    format!(r#"<section class="search-page">{heading}<ul class="results">{items}</ul></section>"#)
}

#[derive(Serialize)]
pub struct SuggestDto {
    pub title: String,
    pub path: String,
    /// The URL to open, built here rather than in `app.js`: a
    /// collection-scoped, percent-encoded article href exists in exactly one
    /// place this way, the Rust side that already writes every other one.
    pub href: String,
    pub matched: Option<String>,
    pub fragment: Option<String>,
    pub inbound: u32,
}

/// `redirected_from`, when present, names the path originally requested —
/// a redirect (title or section) landed the reader here instead, and
/// without this note there's no indication of how (N13). Shown as its own
/// line right above the article, same placement convention as Wikipedia's
/// own "(Redirected from X)".
pub fn article_body(html: &str, redirected_from: Option<&str>) -> String {
    let note = match redirected_from {
        Some(from) => format!(r#"<p class="redirect-note">Redirected from "{}"</p>"#, esc(&from.replace('_', " "))),
        None => String::new(),
    };
    format!(r#"<article id="article">{note}{html}</article>"#)
}

pub struct SuggestionRow {
    pub title: String,
    pub path: String,
}

/// The not-found page: honest about the miss, with up to 5 title suggestions
/// and a way to fall back to full-text search, both real links/forms that
/// work with no JS. `fallback_prefix`, when present, names the shortened
/// prefix `suggestions` actually matched — said explicitly, so a fallback
/// batch doesn't read as if it answered `requested_path` as typed.
/// `elsewhere` names the collections that do have an article with exactly
/// this title. The title index stores normalized keys, so there is no
/// display title to recover: the link is to the reader's own path, which
/// that collection's `resolve_title` canonicalises on the way in, exactly as
/// a `/{collection}/{title}` link always has.
pub fn not_found_body(
    active: &Active,
    requested_path: &str,
    suggestions: &[SuggestionRow],
    fallback_prefix: Option<&str>,
    elsewhere: &[&str],
) -> String {
    let display = requested_path.replace('_', " ");
    let suggestion_list = if suggestions.is_empty() {
        String::new()
    } else {
        let items: String = suggestions
            .iter()
            .map(|s| format!(r#"<li><a href="{href}">{title}</a></li>"#, href = esc(&ok_core::html::article_href(&active.base(), &s.path, None)), title = esc(&s.title)))
            .collect();
        let lead = match fallback_prefix {
            Some(prefix) => format!("Titles starting with \"{}\":", esc(&prefix.replace('_', " "))),
            None => "Maybe one of these:".to_string(),
        };
        format!(r#"<p>{lead}</p><ul class="suggestions">{items}</ul>"#)
    };
    let hint = if elsewhere.is_empty() {
        String::new()
    } else {
        let links: Vec<String> = elsewhere
            .iter()
            .map(|label| {
                format!(r#"<a href="{href}">{label}</a>"#, href = esc(&ok_core::html::article_href(&format!("/{label}"), requested_path, None)))
            })
            .collect();
        let verb = if links.len() == 1 { "has" } else { "have" };
        format!(r#"<p class="elsewhere">{} {verb} a page with this title.</p>"#, links.join(", "))
    };
    format!(
        r#"<section class="not-found">
<h1>"{display}" is not in this collection</h1>
{hint}
{suggestion_list}
<form action="/search" method="get"><input type="hidden" name="q" value="{display}">{collection_input}<button type="submit">Search all text for "{display}"</button></form>
</section>"#,
        display = esc(&display),
        collection_input = collection_field(active.label()),
        hint = hint,
        suggestion_list = suggestion_list,
    )
}

/// A 400 or 500 HTML body. No internal error detail is ever interpolated in.
pub fn error_body(status_label: &str, message: &str) -> String {
    format!(r#"<section class="error"><h1>{status_label}</h1><p>{message}</p></section>"#, message = esc(message))
}
