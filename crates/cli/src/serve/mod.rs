//! `ok serve`: a web UI over the same [`Library`] the TUI reads, matching
//! its UX (search as you type, read, follow links) as closely as a full
//! page load allows. See CLAUDE.md: frontends stay thin, everything else
//! lives in `ok-core`.

mod page;
mod routes;
mod static_assets;

#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use axum::extract::Request;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use hyper_util::service::TowerToHyperService;
use ok_core::text::sanitize_line;
use ok_core::{Collections, Label};

use routes::ArticleCache;
use tower::limit::ConcurrencyLimitLayer;

/// A client that never finishes sending its request headers (or sends them
/// one byte at a time) must not hold a connection — and the article it's
/// mid-request on — open forever. `axum::serve` leaves this unset.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The first path segments the router owns itself. matchit gives a static
/// segment priority over `/{collection}`, so a collection labeled with one
/// of these would be unreachable rather than ambiguous, and `/wiki/x` on a
/// collection labeled `wiki` would redirect to itself forever. `main` passes
/// this list to `Collections::open`, which skips such a label like any other
/// unusable one. `wiki` is here so no collection can shadow `/wiki/{*path}`,
/// the compatibility redirect that keeps every URL this server handed out
/// before collections existed.
pub(crate) const RESERVED_SEGMENTS: &[&str] = &["api", "random", "search", "static", "wiki"];

/// A ceiling well above any legitimate browser's concurrency and well below
/// the flood level that pushed RSS from 62 to 151 MB in testing: the one
/// control this unauthenticated, unrate-limited server has on memory use.
const MAX_CONCURRENT_REQUESTS: usize = 64;

/// Builds its own runtime and blocks on it: `ok serve` is the only reason
/// this process needs an async executor at all.
pub fn run(collections: Arc<Collections>, bind: SocketAddr) -> Result<()> {
    eprint!("{}", announce(&collections));
    tokio::runtime::Runtime::new()?.block_on(serve(collections, bind))
}

/// What `ok serve` says before it binds, and the default collection opened
/// while it says it.
///
/// The lines say what is being served before saying where: in the container
/// one new ZIM can change which collection the home page and the brand
/// belong to, and a line saying only "listening" leaves that invisible.
/// Article counts come from each index's `meta.json`, so nothing is opened
/// for them.
///
/// The default collection is opened here, before the runtime exists, the way
/// every open happened before collections were lazy: it is 20 ms cold, and
/// it otherwise lands inside the first request this server answers, after
/// the startup line has already promised a server that can answer. A failure
/// is that collection's own 503 later, not a reason not to serve the others.
fn announce(collections: &Collections) -> String {
    let mut out = String::new();
    for (i, collection) in collections.iter().enumerate() {
        let Ok(label) = collection.label() else { continue };
        let default = if i == collections.default_index() { " (default)" } else { "" };
        let _ = writeln!(out, "{label}{default}  {}  {} articles", sanitize_line(collection.title()), collection.article_count());
    }
    if let Err(e) = collections.default().library() {
        let _ = writeln!(out, "warning: the default collection could not be opened: {e}");
    }
    out
}

async fn serve(collections: Arc<Collections>, bind: SocketAddr) -> Result<()> {
    // Before the port: `router` is fallible, and binding first meant
    // printing "listening on ..." and then exiting.
    let app = router(collections)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("listening on http://{bind}");
    accept_loop(listener, app, HEADER_READ_TIMEOUT).await
}

/// A descriptor-exhaustion flood must not take the process down: an accept
/// error on one connection is that connection's problem, and EMFILE/ENFILE
/// mean the fix is time, not a retry — a short sleep before the next
/// `accept()` gives in-flight connections a chance to close and free a
/// descriptor, echoing what `axum::serve`'s own accept loop already did.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// The accept loop `serve` runs, with the header-read timeout as a parameter
/// so a test can use a short one instead of waiting out
/// [`HEADER_READ_TIMEOUT`]. Bypasses `axum::serve` (which builds a
/// [`ConnBuilder`] with no timer and no header-read timeout of its own) so
/// this timeout can be set at all.
async fn accept_loop(listener: tokio::net::TcpListener, app: Router, header_read_timeout: Duration) -> Result<()> {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                if is_descriptor_exhaustion(&err) {
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                }
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let mut builder = ConnBuilder::new(TokioExecutor::new());
            builder.http1().timer(TokioTimer::new()).header_read_timeout(header_read_timeout);
            let _ = builder.serve_connection_with_upgrades(io, TowerToHyperService::new(app)).await;
        });
    }
}

/// `ErrorKind::TooManyOpenFiles` is nightly-only as of this toolchain; the
/// raw errno is stable across `accept()`'s Unix targets (Linux and macOS
/// both use EMFILE 24, ENFILE 23), so match on that instead.
fn is_descriptor_exhaustion(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(24) | Some(23))
}

/// The router's state: the whole collection set, every label resolved once
/// so no request pays for one, and the article-render cache. Every handler
/// asks for all of it — which collection answers is per request now, so
/// there is nothing left for a handler to opt out of.
#[derive(Clone)]
struct AppState {
    collections: Arc<Collections>,
    /// One label per collection, in the set's own order. Resolved while the
    /// router is built: a legacy index reads its label from the ZIM (0.9 to
    /// 1.8 ms), which belongs at startup rather than in a page render, and
    /// the reserved-segment check below needs all of them anyway.
    labels: Arc<[Label]>,
    cache: ArticleCache,
}

/// Resolves every label and wires the routes up. Fallible because a label
/// is: a set with one it cannot resolve must stop `ok serve` starting rather
/// than serve a collection no URL can reach. A label that would shadow one
/// of these routes is gone before this runs, skipped by
/// `Collections::resolve_labels` along with every other unusable one.
pub(crate) fn router(collections: Arc<Collections>) -> Result<Router> {
    let mut labels = Vec::with_capacity(collections.len());
    for collection in collections.iter() {
        labels.push(collection.label()?.clone());
    }
    let state = AppState { collections, labels: labels.into(), cache: ArticleCache::new() };
    Ok(Router::new()
        .route("/", get(routes::home))
        .route("/search", get(routes::search))
        .route("/api/suggest", get(routes::api_suggest))
        .route("/random", get(routes::random))
        .route("/static/app.css", get(routes::static_css))
        .route("/static/app.js", get(routes::static_js))
        .route("/wiki/{*path}", get(routes::wiki_redirect))
        .route("/{collection}", get(routes::collection_home))
        .route("/{collection}/{*path}", get(routes::article))
        .with_state(state)
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(middleware::from_fn(security_headers)))
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
