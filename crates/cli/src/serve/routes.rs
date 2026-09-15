use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ok_core::{Library, Resolution};
use serde::Deserialize;

use super::page::{self, SearchRow, SuggestDto, SuggestionRow};

const MAX_QUERY_CHARS: usize = 200;

fn html_ok(cache: &'static str, body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, cache)], Html(body)).into_response()
}

pub(super) fn bad_request_html(message: &str) -> Response {
    let body = page::shell("Error", "Error", None, &page::error_body("400 Bad Request", message));
    (StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

pub(super) fn server_error_html(message: &str) -> Response {
    let body = page::shell("Error", "Error", None, &page::error_body("500 Internal Server Error", message));
    (StatusCode::INTERNAL_SERVER_ERROR, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
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
    let limit = params.limit.unwrap_or(30).clamp(1, 50);
    let lib = Arc::clone(&library);
    let query = q.clone();
    let rows = tokio::task::spawn_blocking(move || -> ok_core::Result<Vec<SearchRow>> {
        lib.search(&query, limit)?
            .into_iter()
            .map(|r| Ok(SearchRow { title: r.title, path: lib.path(r.article)?, summary: r.summary }))
            .collect()
    })
    .await
    .expect("search task panicked");
    match rows {
        Ok(rows) => {
            let page_title = format!("\"{q}\" — search");
            html_ok("no-cache", page::shell(&page_title, &library.meta().title, Some(&q), &page::search_body(&q, &rows)))
        }
        Err(e) => server_error_html(&e.to_string()),
    }
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
    let hits = match library.suggest(&q, limit) {
        Ok(hits) => hits,
        Err(e) => return server_error_html(&e.to_string()),
    };
    let dtos: Vec<SuggestDto> = hits
        .into_iter()
        .filter_map(|s| {
            let path = library.path(s.article).ok()?;
            Some(SuggestDto { title: s.title, path, matched: s.matched, fragment: s.fragment, inbound: s.inbound })
        })
        .collect();
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], axum::Json(dtos)).into_response()
}

enum ArticleOutcome {
    Found { title: String, html: String },
    NotFound { suggestions: Vec<SuggestionRow> },
}

/// Resolution, article load/parse and HTML rendering, all in one blocking
/// call — the whole reason `/wiki/{path}` is the heavy route.
fn render_article(library: &Library, path: &str) -> ok_core::Result<ArticleOutcome> {
    match library.resolve_title(path)? {
        Resolution::Found(target) => {
            let doc = library.article(target.entry)?;
            let html = doc.to_html(&|entry| library.path(entry).ok());
            Ok(ArticleOutcome::Found { title: doc.title, html })
        }
        Resolution::NotFound { suggestions } => {
            let rows = suggestions.into_iter().filter_map(|s| Some(SuggestionRow { path: library.path(s.article).ok()?, title: s.title })).collect();
            Ok(ArticleOutcome::NotFound { suggestions: rows })
        }
    }
}

/// The only article route: canonical, shareable `/wiki/{path}` URLs.
pub async fn wiki_article(State(library): State<Arc<Library>>, AxumPath(path): AxumPath<String>) -> Response {
    let lib = Arc::clone(&library);
    let requested = path.clone();
    let outcome = tokio::task::spawn_blocking(move || render_article(&lib, &requested)).await.expect("wiki_article task panicked");
    match outcome {
        Ok(ArticleOutcome::Found { title, html }) => {
            html_ok("no-cache", page::shell(&title, &library.meta().title, None, &page::article_body(&html)))
        }
        Ok(ArticleOutcome::NotFound { suggestions }) => {
            let body = page::shell("Not found", &library.meta().title, None, &page::not_found_body(&path, &suggestions));
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], Html(body))
                .into_response()
        }
        Err(e) => server_error_html(&e.to_string()),
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
