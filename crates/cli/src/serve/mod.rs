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

use anyhow::Result;
use axum::Router;
use axum::extract::Request;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ok_core::Library;

/// Builds its own runtime and blocks on it: `ok serve` is the only reason
/// this process needs an async executor at all.
pub fn run(library: Library, bind: SocketAddr) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve(library, bind))
}

async fn serve(library: Library, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("listening on http://{bind}");
    axum::serve(listener, router(Arc::new(library))).await?;
    Ok(())
}

pub(crate) fn router(library: Arc<Library>) -> Router {
    Router::new()
        .route("/", get(routes::home))
        .route("/search", get(routes::search))
        .route("/api/suggest", get(routes::api_suggest))
        .route("/wiki/{*path}", get(routes::wiki_article))
        .route("/random", get(routes::random))
        .route("/static/app.css", get(routes::static_css))
        .route("/static/app.js", get(routes::static_js))
        .with_state(library)
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
