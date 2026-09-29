use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxumPath, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ok_core::{Library, Resolution};
use serde::Deserialize;

use super::AppState;
use super::page::{self, Active, SearchRow, SuggestDto, SuggestionRow};

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

/// Entry indices are per ZIM, so an entry index alone is not a key: entry
/// 42 of one collection would serve another's cached render.
pub(super) type CacheKey = (usize, u32);

struct LruInner {
    /// Most recently used at the front.
    entries: VecDeque<(CacheKey, Arc<CachedArticle>)>,
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

    pub(super) fn get(&self, key: CacheKey) -> Option<Arc<CachedArticle>> {
        let mut inner = self.0.lock().expect("cache lock");
        let pos = inner.entries.iter().position(|(k, _)| *k == key)?;
        let hit = inner.entries.remove(pos).expect("just found");
        let article = Arc::clone(&hit.1);
        inner.entries.push_front(hit);
        Some(article)
    }

    pub(super) fn insert(&self, key: CacheKey, article: CachedArticle) -> Arc<CachedArticle> {
        let article = Arc::new(article);
        let mut inner = self.0.lock().expect("cache lock");
        inner.total_bytes += article.bytes();
        inner.entries.push_front((key, Arc::clone(&article)));
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
    let body = page::shell("Error", None, None, &page::error_body("400 Bad Request", message));
    (StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// Logs the detail (which can carry a filesystem path, per `ok_core::Error`'s
/// `NotImported`/`IndexMismatch` variants, or a panicking blocking task's
/// `JoinError`) to stderr; the response only ever gets one fixed sentence,
/// matching `page::error_body`'s own "no internal detail" rule.
pub(super) fn server_error_html(e: impl std::fmt::Display) -> Response {
    eprintln!("500: {e}");
    let body = page::shell("Error", None, None, &page::error_body("500 Internal Server Error", "Something went wrong loading this page."));
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

/// The collection a request names, falling back to the default. `?c=` is
/// absent from every link earlier versions wrote and empty on a form a page
/// left unfilled; a label that names nothing loaded is a stale bookmark,
/// which the default answers rather than failing the page over.
fn active<'a>(state: &'a AppState, wanted: Option<&str>) -> Active<'a> {
    let index = wanted
        .filter(|w| !w.is_empty())
        .and_then(|w| index_of(state, w))
        .unwrap_or_else(|| state.collections.default_index());
    at(state, index)
}

fn index_of(state: &AppState, label: &str) -> Option<usize> {
    state.labels.iter().position(|l| l.as_str() == label)
}

fn at(state: &AppState, index: usize) -> Active<'_> {
    Active { collections: &state.collections, labels: &state.labels, index }
}

/// The active collection's library, or `None` for a collection that failed
/// to open. Opening is lazy, so a ZIM that no longer matches its index
/// surfaces here rather than at startup; the detail goes to stderr on the
/// one open that failed, and the outcome is cached, so neither the open nor
/// the line repeats. Every caller answers a `None` with
/// [`unavailable_html`].
fn library_of(active: &Active) -> Option<Arc<Library>> {
    let collection = active.collection();
    let first_attempt = collection.failure().is_none();
    match collection.library() {
        Ok(library) => Some(library),
        Err(e) => {
            if first_attempt {
                eprintln!("503: collection \"{}\" could not be opened: {e}", active.label());
            }
            None
        }
    }
}

/// The 503 a failed collection's pages serve: its label and one fixed
/// sentence, never a path — the same no-internal-detail rule
/// [`server_error_html`] follows.
fn unavailable_html(active: &Active) -> Response {
    let message = format!("The \"{}\" collection could not be opened. Re-run `ok import` for it.", active.label());
    let body = page::shell("Unavailable", Some(active), None, &page::error_body("503 Service Unavailable", &message));
    (StatusCode::SERVICE_UNAVAILABLE, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// A first path segment that is no collection's label. The default
/// collection's chrome comes with it, so the reader has a way out.
fn unknown_collection_html(state: &AppState, label: &str) -> Response {
    let active = active(state, None);
    let message = format!("There is no collection labeled \"{label}\" here.");
    let body = page::shell("Not found", Some(&active), None, &page::error_body("404 Not Found", &message));
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], Html(body))
        .into_response()
}

#[derive(Deserialize)]
pub struct HomeParams {
    q: Option<String>,
}

/// `/`: the collection list when the process holds more than one, and that
/// one collection's own home when it holds one.
pub async fn home(State(state): State<AppState>, Query(params): Query<HomeParams>) -> Response {
    let active = active(&state, None);
    let (title, body) = if state.collections.len() > 1 {
        ("Collections".to_string(), page::collections_body(&active))
    } else {
        (active.brand().to_string(), page::home_body(active.collection()))
    };
    let body = page::shell(&title, Some(&active), params.q.as_deref(), &body);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// `/{collection}`: one collection's home. Reads its article count from
/// `meta.json`, so it opens no library.
pub async fn collection_home(
    State(state): State<AppState>,
    AxumPath(label): AxumPath<String>,
    Query(params): Query<HomeParams>,
) -> Response {
    if query_too_long(&label) {
        return bad_request_html(&format!("a path accepts at most {MAX_QUERY_CHARS} characters"));
    }
    let Some(index) = index_of(&state, &label) else { return unknown_collection_html(&state, &label) };
    let active = at(&state, index);
    let body = page::shell(active.brand(), Some(&active), params.q.as_deref(), &page::home_body(active.collection()));
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
    /// The collection to search, by label. Its brand and `<title>` come
    /// from here too, not from the default collection's.
    c: Option<String>,
}

pub async fn search(State(state): State<AppState>, Query(params): Query<SearchParams>) -> Response {
    let active = active(&state, params.c.as_deref());
    let q = params.q.unwrap_or_default();
    if query_too_long(&q) {
        return bad_request_html(&format!("the search box accepts at most {MAX_QUERY_CHARS} characters"));
    }
    if q.trim().is_empty() {
        return html_ok("no-cache", page::shell("Search", Some(&active), None, &page::search_prompt_body()));
    }
    let Some(library) = library_of(&active) else { return unavailable_html(&active) };
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
    html_ok("no-cache", page::shell(&page_title, Some(&active), Some(&q), &page::search_body(&active.base(), &q, &rows)))
}

#[derive(Deserialize)]
pub struct SuggestParams {
    q: Option<String>,
    limit: Option<usize>,
    /// The collection to suggest from, by label.
    c: Option<String>,
}

pub async fn api_suggest(State(state): State<AppState>, Query(params): Query<SuggestParams>) -> Response {
    let active = active(&state, params.c.as_deref());
    let q = params.q.unwrap_or_default();
    if query_too_long(&q) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            axum::Json(serde_json::json!({"error": format!("q accepts at most {MAX_QUERY_CHARS} characters")})),
        )
            .into_response();
    }
    let Some(library) = library_of(&active) else { return unavailable_html(&active) };
    let limit = params.limit.unwrap_or(12).clamp(1, 50);
    let lib = Arc::clone(&library);
    let base = active.base();
    let dtos = match tokio::task::spawn_blocking(move || -> ok_core::Result<Vec<SuggestDto>> {
        Ok(lib
            .suggest(&q, limit)?
            .into_iter()
            .filter_map(|s| {
                let path = lib.path(s.article).ok()?;
                let href = ok_core::html::article_href(&base, &path, s.fragment.as_deref());
                Some(SuggestDto { title: s.title, path, href, matched: s.matched, fragment: s.fragment, inbound: s.inbound })
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
    NotFound { suggestions: Vec<SuggestionRow>, fallback_prefix: Option<String> },
}

/// Resolution, article load/parse and HTML rendering, all in one blocking
/// call — the whole reason `/wiki/{path}` is the heavy route. A cache hit
/// on the resolved entry skips load/parse/render entirely.
///
/// `resolve_title` already refuses non-article targets, but `article` is
/// re-checked defensively: `Error::NotArticle` still means "not found", not
/// a server error — that second path used `suggest` (no shorter-prefix
/// fallback, no relevance floor) where the first used
/// `suggest_with_fallback`; both now go through the one function.
fn render_article(library: &Library, cache: &ArticleCache, collection: usize, base: &str, path: &str) -> ok_core::Result<ArticleOutcome> {
    let (suggestions, fallback_prefix) = match library.resolve_title(path)? {
        Resolution::Found(target) => {
            let canonical = library.path(target.entry)?;
            if canonical != path || target.fragment.is_some() {
                // Round-trips the requested path through the query string so
                // the target page can say "Redirected from X" (N13): landing
                // mid-article, on a differently titled page, with no
                // indication of how the reader got there otherwise.
                let location = ok_core::html::article_href_redirected_from(base, &canonical, target.fragment.as_deref(), path);
                return Ok(ArticleOutcome::Redirect { location });
            }
            if let Some(cached) = cache.get((collection, target.entry)) {
                return Ok(ArticleOutcome::Found { title: cached.title.clone(), html: cached.html.clone() });
            }
            match library.article(target.entry) {
                Ok(doc) => {
                    let html = doc.to_html(base, &|entry| library.path(entry).ok());
                    let cached = cache.insert((collection, target.entry), CachedArticle { title: doc.title, html });
                    return Ok(ArticleOutcome::Found { title: cached.title.clone(), html: cached.html.clone() });
                }
                Err(ok_core::Error::NotArticle(_)) => library.suggest_with_fallback(path, 5)?,
                Err(e) => return Err(e),
            }
        }
        Resolution::NotFound { suggestions, fallback_prefix } => (suggestions, fallback_prefix),
    };
    let rows = suggestions.into_iter().filter_map(|s| Some(SuggestionRow { path: library.path(s.article).ok()?, title: s.title })).collect();
    Ok(ArticleOutcome::NotFound { suggestions: rows, fallback_prefix })
}

#[derive(Deserialize)]
pub struct WikiArticleParams {
    redirected_from: Option<String>,
}

/// The only article route: canonical, shareable `/{collection}/{path}`
/// URLs. Two mechanisms on purpose — the collection is a path segment here
/// and a `?c=` parameter on `/search`, `/api/suggest` and `/random`,
/// because `/{collection}/search` would make "search" unreachable as an
/// article title, and ZIM titles certainly include it.
pub async fn article(
    State(state): State<AppState>,
    AxumPath((label, path)): AxumPath<(String, String)>,
    Query(params): Query<WikiArticleParams>,
) -> Response {
    if query_too_long(&path) || query_too_long(&label) {
        return bad_request_html(&format!("a path accepts at most {MAX_QUERY_CHARS} characters"));
    }
    let Some(index) = index_of(&state, &label) else { return unknown_collection_html(&state, &label) };
    let active = at(&state, index);
    let Some(library) = library_of(&active) else { return unavailable_html(&active) };
    // Cosmetic only (the banner below): an oversized value is ignored
    // rather than failing the whole page load over it.
    let redirected_from = params.redirected_from.filter(|s| !query_too_long(s));
    let lib = Arc::clone(&library);
    let cache = state.cache.clone();
    let requested = path.clone();
    let base = active.base();
    let rendering = base.clone();
    let outcome = match tokio::task::spawn_blocking(move || render_article(&lib, &cache, index, &rendering, &requested)).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => return server_error_html(&e),
        Err(e) => return server_error_html(e),
    };
    match outcome {
        ArticleOutcome::Found { title, html } => {
            let page_title = format!("{title} — {}", active.brand());
            let body = page::article_body(&html, redirected_from.as_deref());
            html_ok("no-cache", page::shell(&page_title, Some(&active), None, &body))
        }
        ArticleOutcome::Redirect { location } => {
            (StatusCode::FOUND, [(header::LOCATION, location), (header::CACHE_CONTROL, "no-store".to_string())]).into_response()
        }
        ArticleOutcome::NotFound { suggestions, fallback_prefix } => {
            // Only on a miss, and only an exact-title probe: the one
            // cross-collection read anything here does.
            let elsewhere = active.elsewhere(&path);
            let body = page::not_found_body(&active, &path, &suggestions, fallback_prefix.as_deref(), &elsewhere);
            let body = page::shell("Not found", Some(&active), None, &body);
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], Html(body))
                .into_response()
        }
    }
}

/// `/wiki/{path}`: every article URL this server handed out before
/// collections existed. A 302 rather than a 301 — permanently cached in
/// every browser that saw it would make the URL shape effectively
/// irreversible, and the redirect is noise next to a 4 ms article render.
/// The query string comes along, so the `?redirected_from=` round trip
/// still works through it.
pub async fn wiki_redirect(State(state): State<AppState>, AxumPath(path): AxumPath<String>, RawQuery(query): RawQuery) -> Response {
    if query_too_long(&path) {
        return bad_request_html(&format!("a path accepts at most {MAX_QUERY_CHARS} characters"));
    }
    // The collection labeled `wikipedia` is where these URLs used to point;
    // with nothing labeled that, the default is the best guess left.
    let index = index_of(&state, "wikipedia").unwrap_or_else(|| state.collections.default_index());
    let mut location = ok_core::html::article_href(&at(&state, index).base(), &path, None);
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        location.push('?');
        location.push_str(&query);
    }
    (StatusCode::FOUND, [(header::LOCATION, location), (header::CACHE_CONTROL, "no-store".to_string())]).into_response()
}

#[derive(Deserialize)]
pub struct RandomParams {
    /// The collection to pick from, by label.
    c: Option<String>,
}

pub async fn random(State(state): State<AppState>, Query(params): Query<RandomParams>) -> Response {
    let active = active(&state, params.c.as_deref());
    let Some(library) = library_of(&active) else { return unavailable_html(&active) };
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    match library.random_article(seed).and_then(|entry| library.path(entry).ok()) {
        Some(path) => (
            StatusCode::FOUND,
            [(header::LOCATION, ok_core::html::article_href(&active.base(), &path, None)), (header::CACHE_CONTROL, "no-store".to_string())],
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
