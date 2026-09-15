use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ok_core::Library;
use serde::Deserialize;

use super::page::{self, SearchRow, SuggestDto};

const MAX_QUERY_CHARS: usize = 200;

fn html_ok(cache: &'static str, body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, cache)], Html(body)).into_response()
}

pub(super) fn bad_request_html(message: &str) -> Response {
    let body = page::shell("Error", None, &page::error_body("400 Bad Request", message));
    (StatusCode::BAD_REQUEST, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

pub(super) fn server_error_html(message: &str) -> Response {
    let body = page::shell("Error", None, &page::error_body("500 Internal Server Error", message));
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
    let body = page::shell(&library.meta().title, params.q.as_deref(), &page::home_body(&library));
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
        Ok(rows) => html_ok("no-cache", page::shell(&library.meta().title, Some(&q), &page::search_body(&q, &rows))),
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

pub async fn static_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "public, max-age=3600")], super::static_assets::APP_CSS)
}

pub async fn static_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "public, max-age=3600")], super::static_assets::APP_JS)
}
