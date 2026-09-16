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
<a class="home-link" href="/">{collection_title}</a>
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
        count = with_thousands(library.article_count()),
        title = esc(&library.meta().title),
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
pub fn search_body(query: &str, rows: &[SearchRow]) -> String {
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
pub fn not_found_body(requested_path: &str, suggestions: &[SuggestionRow], fallback_prefix: Option<&str>) -> String {
    let display = requested_path.replace('_', " ");
    let suggestion_list = if suggestions.is_empty() {
        String::new()
    } else {
        let items: String = suggestions
            .iter()
            .map(|s| format!(r#"<li><a href="{href}">{title}</a></li>"#, href = esc(&ok_core::html::wiki_href(&s.path, None)), title = esc(&s.title)))
            .collect();
        let lead = match fallback_prefix {
            Some(prefix) => format!("Titles starting with \"{}\":", esc(&prefix.replace('_', " "))),
            None => "Maybe one of these:".to_string(),
        };
        format!(r#"<p>{lead}</p><ul class="suggestions">{items}</ul>"#)
    };
    format!(
        r#"<section class="not-found">
<h1>"{display}" is not in this collection</h1>
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
