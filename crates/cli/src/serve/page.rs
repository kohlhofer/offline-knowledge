//! HTML page shell and the fragments each route renders into it.
//!
//! Every ZIM-sourced string (a title, a summary, a suggestion) passes
//! through [`esc`] before it reaches a response, the same rule
//! `ok_core::html` follows for article bodies.

use ok_core::Library;
use ok_core::text::sanitize;
use serde::Serialize;

/// Sanitizes control characters, then HTML-escapes. Every piece of text this
/// module writes into a response — ZIM-sourced or a user's query string —
/// goes through this first.
fn esc(s: &str) -> String {
    ok_core::html::escape_html(&sanitize(s))
}

/// The full page: header chrome (persistent search, breadcrumb, live
/// region), `body`, and the help dialog. `q` prefills the search box.
pub fn shell(collection_title: &str, q: Option<&str>, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="/static/app.css">
</head>
<body>
<header class="chrome">
<form action="/search" method="get" class="searchbar" role="search">
<input type="text" name="q" id="search-input" value="{q}" placeholder="Search {title}" autocomplete="off"
 aria-autocomplete="list" aria-expanded="false" role="combobox" aria-controls="suggestions" aria-owns="suggestions">
<button type="submit">Search all text</button>
<ul id="suggestions" role="listbox" hidden></ul>
</form>
<div id="breadcrumb" class="breadcrumb"></div>
<div id="live" aria-live="polite" class="sr-only"></div>
</header>
<main id="main">{body}</main>
<dialog id="outline"></dialog>
<dialog id="help">
<h2>Keys</h2>
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
        title = esc(collection_title),
        q = esc(q.unwrap_or_default()),
        body = body,
    )
}

pub fn home_body(library: &Library) -> String {
    format!(
        r#"<section class="home">
<p class="count">{count} articles in <strong>{title}</strong>.</p>
<p class="hint">Start typing above for titles, or press Enter to search the full text. Press <kbd>?</kbd> for keys, <kbd>r</kbd> for a random article.</p>
</section>"#,
        count = library.article_count(),
        title = esc(&library.meta().title),
    )
}

pub struct SearchRow {
    pub title: String,
    pub path: String,
    pub summary: String,
}

pub fn search_body(query: &str, rows: &[SearchRow]) -> String {
    if rows.is_empty() {
        return format!(r#"<p class="empty">No articles mention "{q}".</p>"#, q = esc(query));
    }
    let items: String = rows
        .iter()
        .map(|r| {
            format!(
                r#"<li><a class="result" href="{href}"><span class="title">{title}</span><span class="summary">{summary}</span></a></li>"#,
                href = esc(&ok_core::html::wiki_href(&r.path, None)),
                title = esc(&r.title),
                summary = esc(&r.summary),
            )
        })
        .collect();
    format!(r#"<ul class="results">{items}</ul>"#)
}

#[derive(Serialize)]
pub struct SuggestDto {
    pub title: String,
    pub path: String,
    pub matched: Option<String>,
    pub fragment: Option<String>,
    pub inbound: u32,
}

/// A 400 or 500 HTML body. No internal error detail is ever interpolated in.
pub fn error_body(status_label: &str, message: &str) -> String {
    format!(r#"<section class="error"><h1>{status_label}</h1><p>{message}</p></section>"#, message = esc(message))
}
