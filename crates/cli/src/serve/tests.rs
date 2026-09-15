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
        .article("Albert_Einstein", "Albert Einstein", &page("Albert Einstein", r#"<p>A <a href="Physicist">physicist</a>.</p>"#))
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
async fn static_assets_are_served_with_a_long_cache_lifetime() {
    let (_d, app) = app();
    let res = get(&app, "/static/app.css").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/css");
    assert_eq!(res.headers().get("cache-control").unwrap(), "public, max-age=3600");

    let res = get(&app, "/static/app.js").await;
    assert_eq!(res.headers().get("content-type").unwrap(), "text/javascript");
}
