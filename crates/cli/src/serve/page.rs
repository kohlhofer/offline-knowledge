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
/// region), `body`, and the help dialog. `page_title` becomes the `<title>`
/// tag; `collection_title` is the search box's placeholder, constant across
/// every page. `q` prefills the search box.
pub fn shell(page_title: &str, collection_title: &str, q: Option<&str>, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en" class="no-js">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{page_title}</title>
<link rel="stylesheet" href="/static/app.css">
</head>
<body>
<header class="chrome">
<form action="/search" method="get" class="searchbar" role="search">
<input type="text" name="q" id="search-input" value="{q}" placeholder="Search {collection_title}" aria-label="Search {collection_title}" autocomplete="off"
 aria-autocomplete="list" aria-expanded="false" role="combobox" aria-controls="suggestions">
<button type="submit">Search all text</button>
<ul id="suggestions" role="listbox" hidden></ul>
</form>
<div id="breadcrumb" class="breadcrumb"></div>
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
        collection_title = esc(collection_title),
        q = esc(q.unwrap_or_default()),
        body = body,
    )
}

pub fn home_body(library: &Library) -> String {
    format!(
        r#"<section class="home">
<p class="count">{count} articles in <strong>{title}</strong>.</p>
<p class="hint">Start typing above for titles, or press Enter to search the full text. <span class="js-only">Press <kbd>?</kbd> for keys, <kbd>r</kbd> for a random article.</span></p>
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

pub fn search_body(query: &str, rows: &[SearchRow]) -> String {
    let count = rows.len();
    let heading = format!(
        r#"<h1>{count} result{plural} for "{q}"</h1>"#,
        plural = if count == 1 { "" } else { "s" },
        q = esc(query)
    );
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
                href = esc(&ok_core::html::wiki_href(&r.path, None)),
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
    pub matched: Option<String>,
    pub fragment: Option<String>,
    pub inbound: u32,
}

pub fn article_body(html: &str) -> String {
    format!(r#"<article id="article">{html}</article>"#)
}

pub struct SuggestionRow {
    pub title: String,
    pub path: String,
}

/// The not-found page: honest about the miss, with up to 5 title suggestions
/// and a way to fall back to full-text search, both real links/forms that
/// work with no JS.
pub fn not_found_body(requested_path: &str, suggestions: &[SuggestionRow]) -> String {
    let display = requested_path.replace('_', " ");
    let suggestion_list = if suggestions.is_empty() {
        String::new()
    } else {
        let items: String = suggestions
            .iter()
            .map(|s| format!(r#"<li><a href="{href}">{title}</a></li>"#, href = esc(&ok_core::html::wiki_href(&s.path, None)), title = esc(&s.title)))
            .collect();
        format!(r#"<p>Maybe one of these:</p><ul class="suggestions">{items}</ul>"#)
    };
    format!(
        r#"<section class="not-found">
<h1>Not in this collection</h1>
<p>"{display}" is not in this collection.</p>
{suggestion_list}
<form action="/search" method="get"><input type="hidden" name="q" value="{display}"><button type="submit">Search all text for "{display}"</button></form>
</section>"#,
        display = esc(&display),
        suggestion_list = suggestion_list,
    )
}

/// A 400 or 500 HTML body. No internal error detail is ever interpolated in.
pub fn error_body(status_label: &str, message: &str) -> String {
    format!(r#"<section class="error"><h1>{status_label}</h1><p>{message}</p></section>"#, message = esc(message))
}
