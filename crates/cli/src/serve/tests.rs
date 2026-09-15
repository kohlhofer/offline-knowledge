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

fn app() -> (tempfile::TempDir, Router) {
    let bytes = ZimBuilder::new()
        .article(
            "Albert_Einstein",
            "Albert Einstein",
            &page(
                "Albert Einstein",
                r##"<table class="infobox"><tr><th>Born</th><td>1879</td></tr></table>
                <p>A <a href="Physicist">physicist</a> who knew <a href="Nowhere">nobody here</a> and cited
                <a href="https://example.org/x">a source</a>.</p>"##,
            ),
        )
        .article("Physicist", "Physicist", &page("Physicist", "<p>Studies physics, like Einstein.</p>"))
        .metadata("Title", "Tiny wiki")
        .build();
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("t.zim");
    std::fs::write(&zim, bytes).unwrap();
    import(&zim, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    let library = Library::open(&zim).unwrap();
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
    assert!(body.contains("No articles mention"), "{body}");
    assert!(body.contains("zzzznotaword"), "{body}");
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
    assert!(body.contains("<title>Albert Einstein</title>"), "{body}");
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
    assert!(body.contains(r#"action="/search""#), "a working search-all-text fallback: {body}");
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
