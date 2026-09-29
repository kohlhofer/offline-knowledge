use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxumPath, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ok_core::{Collections, Library, Resolution};
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

/// The collection a request names, or `None` when it names one that is not
/// loaded. `?c=` is absent from every link earlier versions wrote and empty
/// on a form a page left unfilled, so both of those are the default. A label
/// that names nothing loaded is not: answering it from the default means a
/// reader who asked one collection reads another under its brand, and on a
/// set whose default has failed, `/search?q=x&c=typo` answers 503 about a
/// collection the caller never named.
fn active<'a>(state: &'a AppState, wanted: Option<&str>) -> Option<Active<'a>> {
    match wanted.filter(|w| !w.is_empty()) {
        Some(label) => index_of(state, label).map(|index| at(state, index)),
        None => Some(default_active(state)),
    }
}

/// The default collection's chrome, for a page that belongs to no collection
/// of its own: `/`, and a 404 about a label that names nothing.
fn default_active(state: &AppState) -> Active<'_> {
    at(state, state.collections.default_index())
}

fn index_of(state: &AppState, label: &str) -> Option<usize> {
    state.labels.iter().position(|l| l.as_str() == label)
}

fn at(state: &AppState, index: usize) -> Active<'_> {
    Active { collections: &state.collections, labels: &state.labels, index }
}

/// What a blocking handler task can fail with: the collection could not be
/// opened at all, which is a 503, or the work itself failed, which is a 500.
enum TaskError {
    Unavailable,
    Failed(ok_core::Error),
}

impl From<ok_core::Error> for TaskError {
    fn from(e: ok_core::Error) -> TaskError {
        TaskError::Failed(e)
    }
}

/// The collection's library, opened inside the blocking task that needs it
/// rather than on the async task that spawned it. `Library::open` reads
/// `inbound.u32` and `stubs.bin`, mmaps the title FST and opens Tantivy
/// (20 ms cold), and `OnceLock::get_or_init` makes every concurrent
/// first-requester wait on whichever thread runs it, which must therefore
/// not be a tokio worker. Before collections every open happened before the
/// runtime existed; `ok mcp` has always done it inside `run_blocking`.
///
/// Opening is lazy, so a ZIM that no longer matches its index surfaces here
/// rather than at startup; the detail goes to stderr on the one open that
/// failed, and the outcome is cached, so neither the open nor the line
/// repeats. Every caller answers [`TaskError::Unavailable`] with
/// [`unavailable_html`].
fn library_in_task(collections: &Collections, index: usize, label: &str) -> Result<Arc<Library>, TaskError> {
    let collection = collections.at(index).ok_or(TaskError::Unavailable)?;
    let first_attempt = collection.failure().is_none();
    collection.library().map_err(|e| {
        if first_attempt {
            eprintln!("503: collection \"{label}\" could not be opened: {e}");
        }
        TaskError::Unavailable
    })
}

/// The 503 a failed collection's pages serve: its label and one fixed
/// sentence, never a path — the same no-internal-detail rule
/// [`server_error_html`] follows.
fn unavailable_html(active: &Active) -> Response {
    let message = format!("The \"{}\" collection could not be opened. Re-run `ok import` for it.", active.label());
    let body = page::shell_with(
        "Unavailable",
        Some(active),
        None,
        &page::error_body("503 Service Unavailable", &message),
        page::Chrome::Unavailable,
    );
    (StatusCode::SERVICE_UNAVAILABLE, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

/// A label that names no loaded collection, named back with what is loaded,
/// as the CLI and MCP both do. The default collection's chrome comes with it,
/// so the reader has a way out.
fn unknown_collection_html(state: &AppState, label: &str) -> Response {
    let active = default_active(state);
    let loaded = state.labels.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ");
    let message = format!("There is no collection labeled \"{label}\" here. Loaded: {loaded}.");
    let body = page::shell("Not found", Some(&active), None, &page::error_body("404 Not Found", &message));
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], Html(body))
        .into_response()
}

/// A first path segment that is no collection's label. With exactly one
/// collection loaded there is only one thing such a URL can mean — trimmed
/// to its article, or written before collections existed — so `/{rest}`
/// redirects to `/{label}/{rest}` rather than dead-ending.
fn unknown_segment_html(state: &AppState, rest: &str, label: &str) -> Response {
    if let [only] = &state.labels[..] {
        let location = ok_core::html::article_href(&format!("/{only}"), rest, None);
        return (StatusCode::FOUND, [(header::LOCATION, location), (header::CACHE_CONTROL, "no-store".to_string())]).into_response();
    }
    unknown_collection_html(state, label)
}

#[derive(Deserialize)]
pub struct HomeParams {
    q: Option<String>,
}

/// `/`: the collection list when the process holds more than one, and that
/// one collection's own home when it holds one.
pub async fn home(State(state): State<AppState>, Query(params): Query<HomeParams>) -> Response {
    let active = default_active(&state);
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
    let Some(index) = index_of(&state, &label) else { return unknown_segment_html(&state, &label, &label) };
    let active = at(&state, index);
    // `failure()` is a `OnceLock::get`, so this page still opens no library:
    // it says nothing about a collection nothing has tried yet, and refuses
    // to serve a front page with a healthy article count for one already
    // known to be broken, every link out of which is a 503.
    if active.collection().failure().is_some() {
        return unavailable_html(&active);
    }
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
    let Some(active) = active(&state, params.c.as_deref()) else {
        return unknown_collection_html(&state, &params.c.unwrap_or_default());
    };
    let q = params.q.unwrap_or_default();
    if query_too_long(&q) {
        return bad_request_html(&format!("the search box accepts at most {MAX_QUERY_CHARS} characters"));
    }
    if q.trim().is_empty() {
        return html_ok("no-cache", page::shell("Search", Some(&active), None, &page::search_prompt_body()));
    }
    let limit = params.limit.unwrap_or(30).clamp(1, 50);
    let collections = Arc::clone(&state.collections);
    let index = active.index;
    let label = active.label().to_string();
    let query = q.clone();
    let rows = match tokio::task::spawn_blocking(move || -> Result<Vec<SearchRow>, TaskError> {
        let lib = library_in_task(&collections, index, &label)?;
        let rows: ok_core::Result<Vec<SearchRow>> = lib
            .search(&query, limit)?
            .into_iter()
            .map(|r| Ok(SearchRow { title: r.title, path: lib.path(r.article)?, summary: r.summary }))
            .collect();
        Ok(rows?)
    })
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(TaskError::Unavailable)) => return unavailable_html(&active),
        Ok(Err(TaskError::Failed(e))) => return server_error_html(&e),
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
    let Some(active) = active(&state, params.c.as_deref()) else {
        return unknown_collection_html(&state, &params.c.unwrap_or_default());
    };
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
    let collections = Arc::clone(&state.collections);
    let index = active.index;
    let label = active.label().to_string();
    let base = active.base();
    let dtos = match tokio::task::spawn_blocking(move || -> Result<Vec<SuggestDto>, TaskError> {
        let lib = library_in_task(&collections, index, &label)?;
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
        Ok(Err(TaskError::Unavailable)) => return unavailable_html(&active),
        Ok(Err(TaskError::Failed(e))) => return suggest_error_json(e),
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
    let Some(index) = index_of(&state, &label) else {
        return unknown_segment_html(&state, &format!("{label}/{path}"), &label);
    };
    let active = at(&state, index);
    // Cosmetic only (the banner below): an oversized value is ignored
    // rather than failing the whole page load over it.
    let redirected_from = params.redirected_from.filter(|s| !query_too_long(s));
    let collections = Arc::clone(&state.collections);
    let label = active.label().to_string();
    let cache = state.cache.clone();
    let requested = path.clone();
    let rendering = active.base();
    let outcome = match tokio::task::spawn_blocking(move || -> Result<ArticleOutcome, TaskError> {
        let lib = library_in_task(&collections, index, &label)?;
        Ok(render_article(&lib, &cache, index, &rendering, &requested)?)
    })
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(TaskError::Unavailable)) => return unavailable_html(&active),
        Ok(Err(TaskError::Failed(e))) => return server_error_html(&e),
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
    let Some(active) = active(&state, params.c.as_deref()) else {
        return unknown_collection_html(&state, &params.c.unwrap_or_default());
    };
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let collections = Arc::clone(&state.collections);
    let index = active.index;
    let label = active.label().to_string();
    // The pick itself is two ZIM reads, but the open in front of it is 20 ms
    // cold, and this is the route a reader reaches from a keypress.
    let picked = match tokio::task::spawn_blocking(move || -> Result<Option<String>, TaskError> {
        let lib = library_in_task(&collections, index, &label)?;
        Ok(lib.random_article(seed).and_then(|entry| lib.path(entry).ok()))
    })
    .await
    {
        Ok(Ok(picked)) => picked,
        Ok(Err(TaskError::Unavailable)) => return unavailable_html(&active),
        Ok(Err(TaskError::Failed(e))) => return server_error_html(&e),
        Err(e) => return server_error_html(e),
    };
    match picked {
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
