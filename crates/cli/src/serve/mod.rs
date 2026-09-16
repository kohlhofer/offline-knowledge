//! `ok serve`: a web UI over the same [`Library`] the TUI reads, matching
//! its UX (search as you type, read, follow links) as closely as a full
//! page load allows. See CLAUDE.md: frontends stay thin, everything else
//! lives in `ok-core`.

mod page;
mod routes;
mod static_assets;

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use axum::extract::{FromRef, Request};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use hyper_util::service::TowerToHyperService;
use ok_core::Library;
use routes::ArticleCache;
use tower::limit::ConcurrencyLimitLayer;

/// A client that never finishes sending its request headers (or sends them
/// one byte at a time) must not hold a connection — and the article it's
/// mid-request on — open forever. `axum::serve` leaves this unset.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// A ceiling well above any legitimate browser's concurrency and well below
/// the flood level that pushed RSS from 62 to 151 MB in testing: the one
/// control this unauthenticated, unrate-limited server has on memory use.
const MAX_CONCURRENT_REQUESTS: usize = 64;

/// Builds its own runtime and blocks on it: `ok serve` is the only reason
/// this process needs an async executor at all.
pub fn run(library: Library, bind: SocketAddr) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve(library, bind))
}

async fn serve(library: Library, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("listening on http://{bind}");
    accept_loop(listener, Arc::new(library), HEADER_READ_TIMEOUT).await
}

/// The accept loop `serve` runs, with the header-read timeout as a parameter
/// so a test can use a short one instead of waiting out
/// [`HEADER_READ_TIMEOUT`]. Bypasses `axum::serve` (which builds a
/// [`ConnBuilder`] with no timer and no header-read timeout of its own) so
/// this timeout can be set at all.
async fn accept_loop(listener: tokio::net::TcpListener, library: Arc<Library>, header_read_timeout: Duration) -> Result<()> {
    let app = router(library);
    loop {
        let (stream, _addr) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let mut builder = ConnBuilder::new(TokioExecutor::new());
            builder.http1().timer(TokioTimer::new()).header_read_timeout(header_read_timeout);
            let _ = builder.serve_connection_with_upgrades(io, TowerToHyperService::new(app)).await;
        });
    }
}

/// The router's state: `Library` and the article-render cache, extracted
/// independently via `FromRef` so only `wiki_article` needs to name the
/// cache at all — every other handler still just asks for `Arc<Library>`.
#[derive(Clone)]
struct AppState {
    library: Arc<Library>,
    cache: ArticleCache,
}

impl FromRef<AppState> for Arc<Library> {
    fn from_ref(state: &AppState) -> Arc<Library> {
        Arc::clone(&state.library)
    }
}

impl FromRef<AppState> for ArticleCache {
    fn from_ref(state: &AppState) -> ArticleCache {
        state.cache.clone()
    }
}

pub(crate) fn router(library: Arc<Library>) -> Router {
    let state = AppState { library, cache: ArticleCache::new() };
    Router::new()
        .route("/", get(routes::home))
        .route("/search", get(routes::search))
        .route("/api/suggest", get(routes::api_suggest))
        .route("/wiki/{*path}", get(routes::wiki_article))
        .route("/random", get(routes::random))
        .route("/static/app.css", get(routes::static_css))
        .route("/static/app.js", get(routes::static_js))
        .with_state(state)
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(middleware::from_fn(security_headers))
}

/// Every response, regardless of route: the threat model is "content is
/// untrusted, the network client is not" (loopback/host-only by default),
/// so this is defense in depth rather than the primary control.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
            .parse()
            .expect("static header value"),
    );
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().expect("static header value"));
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().expect("static header value"));
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().expect("static header value"));
    response.into_response()
}
