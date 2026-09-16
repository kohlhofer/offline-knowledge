use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ok_core::{Library, Resolution};
use serde::Deserialize;

use super::page::{self, SearchRow, SuggestDto, SuggestionRow};

const MAX_QUERY_CHARS: usize = 200;

/// A revisit to an already-rendered article costs a full parse and render
/// again (53-61 ms for the largest article in the reference collection) —
/// this cache makes it ~0.2 ms. Never invalidated: the ZIM backing `Library`
/// is immutable while `ok serve` has it open, so a cached render can never
/// go stale.
pub(super) const MAX_CACHE_ENTRIES: usize = 32;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct CachedArticle {
    pub(super) title: String,
    pub(super) html: String,
}

impl CachedArticle {
    fn bytes(&self) -> usize {
        self.title.len() + self.html.len()
    }
}

struct LruInner {
    /// Most recently used at the front.
    entries: VecDeque<(u32, Arc<CachedArticle>)>,
    total_bytes: usize,
}

/// A small LRU of rendered article HTML, keyed by (already redirect-
/// resolved) entry index. Cheap to clone — every clone shares the same
/// lock, so it can be handed to each request via axum's `State` extractor.
#[derive(Clone)]
pub(super) struct ArticleCache(Arc<Mutex<LruInner>>);

impl ArticleCache {
    pub(super) fn new() -> Self {
        ArticleCache(Arc::new(Mutex::new(LruInner { entries: VecDeque::new(), total_bytes: 0 })))
    }

    pub(super) fn get(&self, entry: u32) -> Option<Arc<CachedArticle>> {
        let mut inner = self.0.lock().expect("cache lock");
        let pos = inner.entries.iter().position(|(e, _)| *e == entry)?;
        let hit = inner.entries.remove(pos).expect("just found");
        let article = Arc::clone(&hit.1);
        inner.entries.push_front(hit);
        Some(article)
    }

    pub(super) fn insert(&self, entry: u32, article: CachedArticle) -> Arc<CachedArticle> {
        let article = Arc::new(article);
        let mut inner = self.0.lock().expect("cache lock");
        inner.total_bytes += article.bytes();
        inner.entries.push_front((entry, Arc::clone(&article)));
        while inner.entries.len() > MAX_CACHE_ENTRIES || inner.total_bytes > MAX_CACHE_BYTES {
            let Some((_, evicted)) = inner.entries.pop_back() else { break };
            inner.total_bytes -= evicted.bytes();
        }
        article
    }
}

fn html_ok(cache: &'static str, body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, cache)], Html(body)).into_response()
}

pub(super) fn bad_request_html(message: &str) -> Response {
    let body = page::shell("Error", "Error", None, &page::error_body("400 Bad Request", message));
    (StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// Logs the detail (which can carry a filesystem path, per `ok_core::Error`'s
/// `NotImported`/`IndexMismatch` variants, or a panicking blocking task's
/// `JoinError`) to stderr; the response only ever gets one fixed sentence,
/// matching `page::error_body`'s own "no internal detail" rule.
pub(super) fn server_error_html(e: impl std::fmt::Display) -> Response {
    eprintln!("500: {e}");
    let body = page::shell("Error", "Error", None, &page::error_body("500 Internal Server Error", "Something went wrong loading this page."));
    (StatusCode::INTERNAL_SERVER_ERROR, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// `/api/suggest`'s own 500: the same "log the detail, say one fixed
/// sentence" rule as `server_error_html`, but in the JSON shape this
/// endpoint's happy path and 400 already use — an error response with an
/// HTML body on a JSON endpoint is one more thing a fetch() caller has to
/// guard against.
pub(super) fn suggest_error_json(e: impl std::fmt::Display) -> Response {
    eprintln!("500: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, [(header::CACHE_CONTROL, "no-store")], axum::Json(serde_json::json!({"error": "something went wrong loading suggestions"})))
        .into_response()
}

fn query_too_long(q: &str) -> bool {
    q.chars().count() > MAX_QUERY_CHARS
}

#[derive(Deserialize)]
pub struct HomeParams {
    q: Option<String>,
}

pub async fn home(State(library): State<Arc<Library>>, Query(params): Query<HomeParams>) -> Response {
    let title = &library.meta().title;
    let body = page::shell(title, title, params.q.as_deref(), &page::home_body(&library));
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
}

pub async fn search(State(library): State<Arc<Library>>, Query(params): Query<SearchParams>) -> Response {
    let q = params.q.unwrap_or_default();
    if query_too_long(&q) {
        return bad_request_html(&format!("the search box accepts at most {MAX_QUERY_CHARS} characters"));
    }
    if q.trim().is_empty() {
        let title = &library.meta().title;
        return html_ok("no-cache", page::shell("Search", title, None, &page::search_prompt_body()));
    }
    let limit = params.limit.unwrap_or(30).clamp(1, 50);
    let lib = Arc::clone(&library);
    let query = q.clone();
    let rows = match tokio::task::spawn_blocking(move || -> ok_core::Result<Vec<SearchRow>> {
        lib.search(&query, limit)?
            .into_iter()
            .map(|r| Ok(SearchRow { title: r.title, path: lib.path(r.article)?, summary: r.summary }))
            .collect()
    })
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => return server_error_html(&e),
        Err(e) => return server_error_html(e),
    };
    let page_title = format!("\"{q}\" — search");
    html_ok("no-cache", page::shell(&page_title, &library.meta().title, Some(&q), &page::search_body(&q, &rows)))
}

#[derive(Deserialize)]
pub struct SuggestParams {
    q: Option<String>,
    limit: Option<usize>,
}

pub async fn api_suggest(State(library): State<Arc<Library>>, Query(params): Query<SuggestParams>) -> Response {
    let q = params.q.unwrap_or_default();
    if query_too_long(&q) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            axum::Json(serde_json::json!({"error": format!("q accepts at most {MAX_QUERY_CHARS} characters")})),
        )
            .into_response();
    }
    let limit = params.limit.unwrap_or(12).clamp(1, 50);
    let lib = Arc::clone(&library);
    let dtos = match tokio::task::spawn_blocking(move || -> ok_core::Result<Vec<SuggestDto>> {
        Ok(lib
            .suggest(&q, limit)?
            .into_iter()
            .filter_map(|s| {
                let path = lib.path(s.article).ok()?;
                Some(SuggestDto { title: s.title, path, matched: s.matched, fragment: s.fragment, inbound: s.inbound })
            })
            .collect())
    })
    .await
    {
        Ok(Ok(dtos)) => dtos,
        Ok(Err(e)) => return suggest_error_json(e),
        Err(e) => return suggest_error_json(e),
    };
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], axum::Json(dtos)).into_response()
}

enum ArticleOutcome {
    Found { title: String, html: String },
    /// The request resolved, but not to the canonical URL: a section
    /// redirect's fragment, or a path that reached the article by title
    /// rather than its own path. One canonical `/wiki/{path}` per article.
    Redirect { location: String },
    NotFound { suggestions: Vec<SuggestionRow> },
}

/// Resolution, article load/parse and HTML rendering, all in one blocking
/// call — the whole reason `/wiki/{path}` is the heavy route. A cache hit
/// on the resolved entry skips load/parse/render entirely.
///
/// `resolve_title` already refuses non-article targets, but `article` is
/// re-checked defensively: `Error::NotArticle` still means "not found", not
/// a server error.
fn render_article(library: &Library, cache: &ArticleCache, path: &str) -> ok_core::Result<ArticleOutcome> {
    let suggestions = match library.resolve_title(path)? {
        Resolution::Found(target) => {
            let canonical = library.path(target.entry)?;
            if canonical != path || target.fragment.is_some() {
                let location = ok_core::html::wiki_href(&canonical, target.fragment.as_deref());
                return Ok(ArticleOutcome::Redirect { location });
            }
            if let Some(cached) = cache.get(target.entry) {
                return Ok(ArticleOutcome::Found { title: cached.title.clone(), html: cached.html.clone() });
            }
            match library.article(target.entry) {
                Ok(doc) => {
                    let html = doc.to_html(&|entry| library.path(entry).ok());
                    let cached = cache.insert(target.entry, CachedArticle { title: doc.title, html });
                    return Ok(ArticleOutcome::Found { title: cached.title.clone(), html: cached.html.clone() });
                }
                Err(ok_core::Error::NotArticle(_)) => library.suggest(path, 5)?,
                Err(e) => return Err(e),
            }
        }
        Resolution::NotFound { suggestions } => suggestions,
    };
    let rows = suggestions.into_iter().filter_map(|s| Some(SuggestionRow { path: library.path(s.article).ok()?, title: s.title })).collect();
    Ok(ArticleOutcome::NotFound { suggestions: rows })
}

/// The only article route: canonical, shareable `/wiki/{path}` URLs.
pub async fn wiki_article(State(library): State<Arc<Library>>, State(cache): State<ArticleCache>, AxumPath(path): AxumPath<String>) -> Response {
    let lib = Arc::clone(&library);
    let requested = path.clone();
    let outcome = match tokio::task::spawn_blocking(move || render_article(&lib, &cache, &requested)).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => return server_error_html(&e),
        Err(e) => return server_error_html(e),
    };
    match outcome {
        ArticleOutcome::Found { title, html } => {
            let page_title = format!("{title} — {}", library.meta().title);
            html_ok("no-cache", page::shell(&page_title, &library.meta().title, None, &page::article_body(&html)))
        }
        ArticleOutcome::Redirect { location } => {
            (StatusCode::FOUND, [(header::LOCATION, location), (header::CACHE_CONTROL, "no-store".to_string())]).into_response()
        }
        ArticleOutcome::NotFound { suggestions } => {
            let body = page::shell("Not found", &library.meta().title, None, &page::not_found_body(&path, &suggestions));
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], Html(body))
                .into_response()
        }
    }
}

pub async fn random(State(library): State<Arc<Library>>) -> Response {
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    match library.random_article(seed).and_then(|entry| library.path(entry).ok()) {
        Some(path) => (
            StatusCode::FOUND,
            [(header::LOCATION, ok_core::html::wiki_href(&path, None)), (header::CACHE_CONTROL, "no-store".to_string())],
        )
            .into_response(),
        None => server_error_html("this collection has no articles"),
    }
}

pub async fn static_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "public, max-age=3600")], super::static_assets::APP_CSS)
}

pub async fn static_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "public, max-age=3600")], super::static_assets::APP_JS)
}
