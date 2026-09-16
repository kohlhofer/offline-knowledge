use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use ok_core::Library;
use ok_core::import::{ImportOptions, import};
use ok_zim::write::ZimBuilder;
use tower::ServiceExt;

use super::router;

fn page(title: &str, body: &str) -> String {
    format!(r#"<html><body><h1>{title}</h1><div id="mw-content-text"><div class="mw-parser-output">{body}</div></div></body></html>"#)
}

fn library() -> (tempfile::TempDir, Library) {
    let bytes = ZimBuilder::new()
        .article(
            "Albert_Einstein",
            "Albert Einstein",
            &page(
                "Albert Einstein",
                r##"<table class="infobox"><tr><th>Born</th><td>1879</td></tr></table>
                <p>A <a href="Physicist">physicist</a> who knew <a href="Nowhere">nobody here</a> and cited
                <a href="https://example.org/x">a source</a>.</p>
                <div class="mw-heading mw-heading2"><h2 id="Life">Life</h2></div>
                <p>Born in Ulm.</p>"##,
            ),
        )
        .article("Physicist", "Physicist", &page("Physicist", "<p>Studies physics, like Einstein.</p>"))
        .article(
            "Einstein_early_life",
            "Einstein early life",
            r#"<html><head><meta http-equiv="refresh" content="0;URL='./Albert_Einstein#Life'" /></head><body></body></html>"#,
        )
        .redirect("Einstein", "Einstein", "Albert_Einstein")
        .resource("_res_/style.css", "text/css", b"p{}")
        .metadata("Title", "Tiny wiki")
        .build();
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("t.zim");
    std::fs::write(&zim, bytes).unwrap();
    import(&zim, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    (dir, Library::open(&zim).unwrap())
}

fn app() -> (tempfile::TempDir, Router) {
    let (dir, library) = library();
    (dir, router(Arc::new(library)))
}

async fn get(app: &Router, uri: &str) -> Response {
    app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap()
}

async fn body_text(response: Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn home_shows_article_count_and_collection_title_with_hints() {
    let (_d, app) = app();
    let res = get(&app, "/").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-store");
    let body = body_text(res).await;
    assert!(body.contains("2 articles"), "{body}");
    assert!(body.contains("Tiny wiki"), "{body}");
    assert!(body.contains('?'), "a hint mentions the help key: {body}");
    assert!(body.contains(r#"<a class="home-link" href="/">Tiny wiki</a>"#), "a persistent way home in the header: {body}");
}

#[test]
fn with_thousands_separates_every_three_digits() {
    assert_eq!(super::page::with_thousands(0), "0");
    assert_eq!(super::page::with_thousands(999), "999");
    assert_eq!(super::page::with_thousands(1000), "1,000");
    assert_eq!(super::page::with_thousands(50001), "50,001");
    assert_eq!(super::page::with_thousands(1_234_567), "1,234,567");
}

#[tokio::test]
async fn security_headers_present_on_every_route() {
    let (_d, app) = app();
    for uri in ["/", "/search?q=phys", "/api/suggest?q=phys", "/static/app.css", "/static/app.js"] {
        let res = get(&app, uri).await;
        assert_eq!(res.headers().get("x-content-type-options").unwrap(), "nosniff", "{uri}");
        assert_eq!(res.headers().get("x-frame-options").unwrap(), "DENY", "{uri}");
        assert_eq!(res.headers().get("referrer-policy").unwrap(), "no-referrer", "{uri}");
        assert!(res.headers().get("content-security-policy").unwrap().to_str().unwrap().contains("default-src 'none'"), "{uri}");
    }
}

#[tokio::test]
async fn search_happy_path_links_to_wiki_path_and_shows_a_summary() {
    let (_d, app) = app();
    let res = get(&app, "/search?q=physics").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
    let body = body_text(res).await;
    assert!(body.contains("href=\"/wiki/Physicist\""), "{body}");
    assert!(body.contains("Studies physics"), "{body}");
}

#[tokio::test]
async fn search_with_no_hits_shows_the_zero_results_state() {
    let (_d, app) = app();
    let body = body_text(get(&app, "/search?q=zzzznotaword").await).await;
    assert!(body.contains("<h1>0 results for"), "a heading names the query and the count: {body}");
    assert!(body.contains("No articles mention"), "{body}");
    assert!(body.contains("zzzznotaword"), "{body}");
}

#[tokio::test]
async fn search_shows_an_h1_with_the_query_and_the_result_count() {
    let (_d, app) = app();
    let body = body_text(get(&app, "/search?q=physics").await).await;
    assert!(body.contains("<h1>1 result for"), "{body}");
}

#[tokio::test]
async fn search_with_an_empty_query_prompts_instead_of_blaming_the_collection() {
    let (_d, app) = app();
    for uri in ["/search", "/search?q="] {
        let body = body_text(get(&app, uri).await).await;
        assert!(!body.contains("No articles mention"), "{uri}: {body}");
        assert!(body.contains("Type a query"), "{uri}: {body}");
    }
}

#[tokio::test]
async fn oversized_query_is_400_not_silently_truncated() {
    let (_d, app) = app();
    let long = "a".repeat(201);

    let res = get(&app, &format!("/search?q={long}")).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = get(&app, &format!("/api/suggest?q={long}")).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = body_text(res).await;
    assert!(body.contains("\"error\""), "{body}");
}

#[tokio::test]
async fn api_suggest_returns_title_path_matched_fragment_and_inbound() {
    let (_d, app) = app();
    let res = get(&app, "/api/suggest?q=albert").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "application/json");
    let body = body_text(res).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hit = &json[0];
    assert_eq!(hit["title"], "Albert Einstein");
    assert_eq!(hit["path"], "Albert_Einstein");
    assert!(hit.get("matched").is_some());
    assert!(hit.get("fragment").is_some());
    assert!(hit.get("inbound").is_some());
}

#[tokio::test]
async fn wiki_article_renders_title_infobox_and_breadcrumb() {
    let (_d, app) = app();
    let res = get(&app, "/wiki/Albert_Einstein").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
    let body = body_text(res).await;
    assert!(body.contains("<h1 id=\"Albert_Einstein\" data-path=\"Albert Einstein\">Albert Einstein</h1>"), "{body}");
    assert!(body.contains("class=\"infobox\""), "{body}");
    assert!(body.contains("<title>Albert Einstein — Tiny wiki</title>"), "the collection name is in the article title too: {body}");
}

#[tokio::test]
async fn wiki_path_resolved_by_title_redirects_to_the_canonical_path() {
    let (_d, app) = app();
    let res = get(&app, "/wiki/Einstein").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers().get("location").unwrap(), "/wiki/Albert_Einstein");
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn wiki_section_redirect_redirects_to_the_canonical_path_with_its_fragment() {
    let (_d, app) = app();
    let res = get(&app, "/wiki/Einstein_early_life").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers().get("location").unwrap(), "/wiki/Albert_Einstein#Life");
}

#[tokio::test]
async fn wiki_article_link_safety_missing_is_real_link_external_has_marker_and_norefferer() {
    let (_d, app) = app();
    let body = body_text(get(&app, "/wiki/Albert_Einstein").await).await;
    assert!(body.contains("<a class=\"link missing\" href=\"/wiki/Nowhere\">nobody here</a>"), "{body}");
    assert!(body.contains("<a class=\"link article\" href=\"/wiki/Physicist\">physicist</a>"), "{body}");
    assert!(
        body.contains("<a class=\"link external\" href=\"https://example.org/x\" rel=\"noreferrer\">a source</a>"),
        "{body}"
    );
    assert!(!body.contains("target="), "external links open in the same tab: {body}");
}

#[tokio::test]
async fn wiki_unknown_path_is_404_with_suggestions_and_a_search_all_text_link() {
    let (_d, app) = app();
    let res = get(&app, "/wiki/Not_A_Real_Page").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
    let body = body_text(res).await;
    assert!(body.contains("not in this collection"), "{body}");
    assert_eq!(body.matches("is not in this collection").count(), 1, "the miss is stated once, not in both the h1 and a paragraph: {body}");
    assert!(body.contains(r#"action="/search""#), "a working search-all-text fallback: {body}");
}

#[tokio::test]
async fn wiki_non_article_entry_is_404_not_500() {
    // The path exists (a real CSS resource in the ZIM's article namespace),
    // but it isn't a readable article; `resolve_title` refuses it, so the
    // route must land on the 404 page, not a 500.
    let (_d, app) = app();
    let res = get(&app, "/wiki/_res_/style.css").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = body_text(res).await;
    assert!(body.contains("not in this collection"), "{body}");
}

#[tokio::test]
async fn internal_error_body_never_leaks_the_error_detail() {
    let leaky = ok_core::Error::IndexMismatch {
        index: std::path::PathBuf::from("/Users/alex/private/data.okx"),
        expected: "abc123".into(),
        found: "def456".into(),
    };
    let res = super::routes::server_error_html(&leaky);
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_text(res).await;
    assert!(!body.contains("/Users/alex/private"), "{body}");
    assert!(!body.contains("abc123") && !body.contains("def456"), "{body}");
    assert!(body.contains("500 Internal Server Error"), "{body}");
}

#[tokio::test]
async fn suggest_error_json_answers_in_the_json_shape_not_html() {
    let res = super::routes::suggest_error_json("boom: /some/leaky/path");
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.headers().get("content-type").unwrap(), "application/json");
    let body = body_text(res).await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON, not an HTML body");
    assert!(json.get("error").is_some(), "{body}");
    assert!(!body.contains("/some/leaky/path"), "{body}");
}

#[tokio::test]
async fn random_redirects_to_a_valid_wiki_path() {
    let (_d, app) = app();
    let res = get(&app, "/random").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-store");
    let location = res.headers().get("location").unwrap().to_str().unwrap().to_string();
    assert!(location == "/wiki/Albert_Einstein" || location == "/wiki/Physicist", "{location}");
}

#[tokio::test]
async fn static_assets_are_served_with_a_long_cache_lifetime() {
    let (_d, app) = app();
    let res = get(&app, "/static/app.css").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/css");
    assert_eq!(res.headers().get("cache-control").unwrap(), "public, max-age=3600");

    let res = get(&app, "/static/app.js").await;
    assert_eq!(res.headers().get("content-type").unwrap(), "text/javascript");
}

/// A real headless-Chrome DOM dump against the real corpus, proving the JS
/// actually ran (it sets `data-perf-ready` once `DOMContentLoaded` and the
/// first `requestAnimationFrame` have both fired). Manual only: needs
/// `OK_ZIM` and a Chrome binary neither exists in CI nor is exercised by any
/// other test.
/// `OK_ZIM=... cargo test -p ok chrome_smoke -- --ignored`
///
/// Multi-threaded runtime: `Command::output()` below blocks its OS thread
/// for the whole Chrome run, which would starve a single-threaded runtime
/// and the spawned server would never get to accept Chrome's connection.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn chrome_smoke() {
    let zim = std::env::var("OK_ZIM").expect("OK_ZIM");
    let library = Library::open(&zim).expect("ok import must have already run");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router(Arc::new(library))).await;
    });

    let chrome = std::env::var("CHROME").unwrap_or_else(|_| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string());
    for path in ["/", "/wiki/Albert_Einstein"] {
        // No --virtual-time-budget: paired with --dump-dom it can hang this
        // Chrome build indefinitely instead of budgeting time as documented.
        let output = std::process::Command::new(&chrome)
            .args(["--headless=new", "--disable-gpu", "--no-sandbox", "--dump-dom", &format!("http://{addr}{path}")])
            .output()
            .unwrap_or_else(|e| panic!("failed to run {chrome}: {e}"));
        let dom = String::from_utf8_lossy(&output.stdout);
        assert!(dom.contains("data-perf-ready"), "JS did not set data-perf-ready for {path}:\n{dom}");
    }
    server.abort();
}

/// A real TCP round trip, not `oneshot()`: proves the router actually binds
/// and serves over a socket, using bench's own hand-rolled HTTP client so
/// there is exactly one such client in the codebase.
#[tokio::test]
async fn real_socket_smoke_test_serves_home_and_an_article() {
    let (_d, router) = app();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let home = crate::bench::http_get(addr, "/").await.unwrap();
    assert_eq!(home.status, 200);
    assert!(String::from_utf8_lossy(&home.body).contains("Tiny wiki"), "{}", String::from_utf8_lossy(&home.body));

    let article = crate::bench::http_get(addr, "/wiki/Albert_Einstein").await.unwrap();
    assert_eq!(article.status, 200);
    assert!(String::from_utf8_lossy(&article.body).contains("Albert Einstein"));

    server.abort();
}

/// A client that sends a request line but never finishes its headers (no
/// blank line) must not hold the connection forever: `accept_loop`'s
/// header-read timeout, exercised here with a short one instead of
/// production's ten seconds.
#[tokio::test]
async fn half_open_connection_is_closed_after_the_header_read_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (_d, library) = library();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(super::accept_loop(listener, Arc::new(library), std::time::Duration::from_millis(150)));

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n").await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "a connection whose headers never finish must be closed by the server");

    server.abort();
}

/// Invoked only as a subprocess by
/// `accept_loop_survives_descriptor_exhaustion`, which lowers *its own*
/// child process's descriptor limit with `ulimit` before re-exec'ing this
/// same test binary — the machine's real limit, and every other test's, is
/// never touched.
#[tokio::test]
#[ignore = "run only as a subprocess with ulimit -n already lowered; see accept_loop_survives_descriptor_exhaustion"]
async fn accept_loop_flood_worker() {
    let (_d, library) = library();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("PORT={}", listener.local_addr().unwrap().port());
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    let _ = super::accept_loop(listener, Arc::new(library), std::time::Duration::from_secs(5)).await;
}

/// The regression this guards: `listener.accept().await?` used to propagate
/// any accept error straight out of the loop, so a descriptor-exhaustion
/// flood killed the whole process (reproduced by hand with `ulimit -n 96`
/// against a real 90-connection flood: "Too many open files (os error 24)",
/// then the process gone). Exhausting descriptors for real needs its own
/// process, done here by re-exec'ing this test binary as a child with a
/// lowered `ulimit -n` of its own.
#[test]
fn accept_loop_survives_descriptor_exhaustion() {
    let exe = std::env::current_exe().expect("current test binary path");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"ulimit -n 96 && exec "$0" serve::tests::accept_loop_flood_worker --exact --ignored --nocapture"#)
        .arg(&exe)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the flood worker");

    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = std::io::BufReader::new(stdout);
    // The test harness itself writes a blank line and "running 1 test"
    // ahead of the worker's own output (guaranteed by `--nocapture`, not
    // suppressed), so skip lines until the worker's PORT= line shows up.
    let port: u16 = loop {
        let mut line = String::new();
        let n = std::io::BufRead::read_line(&mut reader, &mut line).expect("read a line from the worker");
        if n == 0 {
            let mut stderr_buf = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut stderr, &mut stderr_buf);
            }
            panic!("worker closed stdout before printing a PORT= line; stderr: {stderr_buf}");
        }
        if let Some(value) = line.trim().strip_prefix("PORT=") {
            break value.parse().expect("a numeric port");
        }
    };

    // Hold connections open (not close them) so the worker's own accept()
    // calls, not just its connecting peers, are the ones that run into
    // EMFILE/ENFILE past its 96-descriptor cap.
    let mut held = Vec::new();
    for _ in 0..90 {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => held.push(s),
            Err(_) => break,
        }
    }
    assert!(held.len() > 20, "expected a meaningful flood before something stopped us, got {}", held.len());

    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(child.try_wait().expect("try_wait").is_none(), "an accept error must not exit the process");

    // Recovery: release most of the flood and confirm the worker still serves.
    held.truncate(5);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut probe = std::net::TcpStream::connect(("127.0.0.1", port)).expect("still accepting connections after the flood");
    std::io::Write::write_all(&mut probe, b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut probe, &mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "worker did not answer after recovering from the flood: {response}");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn is_descriptor_exhaustion_matches_emfile_and_enfile_only() {
    use super::is_descriptor_exhaustion;

    let emfile = std::io::Error::from_raw_os_error(24);
    let enfile = std::io::Error::from_raw_os_error(23);
    let other = std::io::Error::from(std::io::ErrorKind::ConnectionAborted);

    assert!(is_descriptor_exhaustion(&emfile), "EMFILE (24) must be recognized");
    assert!(is_descriptor_exhaustion(&enfile), "ENFILE (23) must be recognized");
    assert!(!is_descriptor_exhaustion(&other), "an unrelated accept error must not trigger the backoff");
}

/// `ConcurrencyLimitLayer`, applied in isolation to a synthetic slow service:
/// the real routes all resolve in well under a millisecond, so there is no
/// window in which an end-to-end request could observe queueing — this
/// proves the layer this router installs actually bounds concurrency.
#[tokio::test]
async fn concurrency_limit_layer_bounds_in_flight_requests() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tower::limit::ConcurrencyLimitLayer;
    use tower::{Layer, Service};

    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let (in_flight_for_service, max_seen_for_service) = (Arc::clone(&in_flight), Arc::clone(&max_seen));
    let service = tower::service_fn(move |_req: ()| {
        let (in_flight, max_seen) = (Arc::clone(&in_flight_for_service), Arc::clone(&max_seen_for_service));
        async move {
            let n = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok::<(), std::convert::Infallible>(())
        }
    });
    let limited = ConcurrencyLimitLayer::new(2).layer(service);

    let mut handles = Vec::new();
    for _ in 0..6 {
        let mut svc = limited.clone();
        handles.push(tokio::spawn(async move { svc.ready().await.unwrap().call(()).await.unwrap() }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    assert!(max_seen.load(Ordering::SeqCst) <= 2, "concurrency cap not enforced: saw {} requests in flight", max_seen.load(Ordering::SeqCst));
}

#[test]
fn article_cache_hit_returns_what_was_inserted() {
    use super::routes::{ArticleCache, CachedArticle};

    let cache = ArticleCache::new();
    assert!(cache.get(1).is_none(), "empty cache misses");
    cache.insert(1, CachedArticle { title: "A".into(), html: "<p>a</p>".into() });
    let hit = cache.get(1).expect("just inserted");
    assert_eq!(hit.title, "A");
    assert_eq!(hit.html, "<p>a</p>");
}

#[test]
fn article_cache_evicts_the_least_recently_used_entry_past_the_cap() {
    use super::routes::{ArticleCache, CachedArticle, MAX_CACHE_ENTRIES};

    let cache = ArticleCache::new();
    for i in 0..MAX_CACHE_ENTRIES as u32 {
        cache.insert(i, CachedArticle { title: i.to_string(), html: "x".into() });
    }
    // One more push past the cap evicts entry 0, the least recently used:
    // nothing has been looked up since the fill loop, so eviction order is
    // exactly insertion order.
    cache.insert(MAX_CACHE_ENTRIES as u32, CachedArticle { title: "new".into(), html: "y".into() });
    assert!(cache.get(0).is_none(), "the least recently used entry is evicted past the cap");
    assert!(cache.get(1).is_some(), "everything else survives");
    assert!(cache.get(MAX_CACHE_ENTRIES as u32).is_some(), "the newest entry is present");
}

#[tokio::test]
async fn wiki_article_second_request_serves_the_same_content_from_the_cache() {
    let (_d, app) = app();
    let first = body_text(get(&app, "/wiki/Albert_Einstein").await).await;
    let second = body_text(get(&app, "/wiki/Albert_Einstein").await).await;
    assert_eq!(first, second, "a cache hit renders the same page as the first request");
}
