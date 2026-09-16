use ok_core::Resolution;
use ok_core::import::{ImportOptions, import};
use ok_zim::write::ZimBuilder;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;

use super::tools;
use super::*;

fn page(title: &str, body: &str) -> String {
    format!(r#"<html><body><h1>{title}</h1><div id="mw-content-text"><div class="mw-parser-output">{body}</div></div></body></html>"#)
}

/// A large multi-byte-heavy paragraph, for truncation-at-a-char-boundary
/// coverage: "é" is 2 bytes in UTF-8, so a byte-based cut near the 6000th
/// character would split one.
fn long_paragraph() -> String {
    format!("<p>{}</p>", "café ".repeat(1300))
}

fn wiki() -> Vec<u8> {
    ZimBuilder::new()
        .article(
            "Albert_Einstein",
            "Albert Einstein",
            &page(
                "Albert Einstein",
                &format!(
                    r#"<table class="infobox"><tr><th>Born</th><td>1879</td></tr><tr><th>Died</th><td>1955</td></tr></table>
                    <p>Albert Einstein was a physicist who developed <a href="Theory_of_relativity">relativity</a> and knew
                    <a href="Nowhere">nobody here</a>. See <a href="https://example.org">a source</a>.</p>
                    <div class="mw-heading mw-heading2"><h2 id="Life">Life</h2></div>
                    <p>Born in Ulm.</p>
                    <div class="mw-heading mw-heading3"><h3 id="Early_life">Early life</h3></div>
                    {}"#,
                    long_paragraph()
                ),
            ),
        )
        .article(
            "Theory_of_relativity",
            "Theory of relativity",
            &page("Theory of relativity", r#"<p>Developed by <a href="Einstein">Einstein</a>, a physicist.</p>"#),
        )
        .article(
            "Einstein_early_life",
            "Einstein early life",
            r#"<html><head><meta http-equiv="refresh" content="0;URL='./Albert_Einstein#Life'" /></head><body></body></html>"#,
        )
        // A section redirect whose fragment matches no real section in its
        // target (a stale link, or one written for a since-renamed
        // heading) — covers L7's fallback-to-overview behavior. Named to
        // avoid "einstein", so it doesn't become a third title hit for the
        // "einstein" query the search tests below rely on.
        .article(
            "Stale_reference",
            "Stale reference",
            r#"<html><head><meta http-equiv="refresh" content="0;URL='./Albert_Einstein#Nonexistent_Section'" /></head><body></body></html>"#,
        )
        .redirect("Einstein", "Einstein", "Albert_Einstein")
        .metadata("Title", "Tiny wiki")
        .build()
}

fn imported() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("t.zim");
    std::fs::write(&zim, wiki()).unwrap();
    import(&zim, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    let library = Library::open(&zim).unwrap();
    (dir, library)
}

/// A one-article ZIM, imported: for tests that need a specific dirent title
/// or HTML body a shared fixture's other assertions would be disturbed by.
fn library_with(bytes: Vec<u8>) -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let zim = dir.path().join("t.zim");
    std::fs::write(&zim, bytes).unwrap();
    import(&zim, &ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
    let library = Library::open(&zim).unwrap();
    (dir, library)
}

// ---------------------------------------------------------------------
// tools.rs unit tests — no rmcp machinery.
// ---------------------------------------------------------------------

#[test]
fn search_title_hits_before_fulltext_alias_shown_deduplicated() {
    let (_d, library) = imported();
    let text = tools::search_text(&library, "einstein", 8).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("2 shown for \"einstein\"") || lines[0].starts_with("1 shown for \"einstein\""), "{text}");
    // The title hit (via the "Einstein" alias) comes before any full-text
    // hit; each is a fenced, article-derived string, not bare framing text.
    let einstein_line = lines.iter().position(|l| l.starts_with("<article-text>Albert Einstein</article-text>")).unwrap();
    assert!(lines[einstein_line].contains("title match via \"<article-text>Einstein</article-text>\""), "{text}");
    assert!(lines[1..].iter().skip(1).all(|l| !l.contains("Albert Einstein")), "deduplicated: {text}");
}

/// The title FST is built straight from each dirent's raw title string, no
/// `<h1>`-time whitespace collapsing involved (`ArticleContext::title` is
/// only a fallback for a page with no `<h1>`), so a crafted title can carry
/// a literal newline. Unfenced, that would read as a second, unmarked
/// result line.
#[test]
fn search_result_title_collapses_an_embedded_newline_and_is_fenced() {
    let (_d, library) = library_with(
        ZimBuilder::new()
            .article("Newline_Title", "Ein\nstein Prize", &page("Einstein Prize", "<p>Some prose about a prize.</p>"))
            .metadata("Title", "Tiny wiki")
            .build(),
    );

    let text = tools::search_text(&library, "Ein", 5).unwrap();
    assert!(text.contains("<article-text>Ein stein Prize</article-text>"), "{text}");
    assert!(!text.lines().any(|l| l == "stein Prize"), "an embedded newline must not fake a second result line: {text}");
}

#[test]
fn search_empty_query_is_an_error() {
    let (_d, library) = imported();
    assert!(tools::search_text(&library, "   ", 8).is_err());
}

#[test]
fn search_header_names_the_next_step_or_the_truncation() {
    assert_eq!(tools::search_header(0, 0, "zzz"), "0 shown for \"zzz\" — try different words, or fewer of them");
    assert_eq!(tools::search_header(3, 3, "x"), "3 shown for \"x\"");
    assert_eq!(tools::search_header(2, 5, "x"), "2 shown of 5 for \"x\"");
}

#[test]
fn search_skips_fulltext_when_title_hits_already_fill_the_limit() {
    let (_d, library) = imported();
    // "einstein" alone at limit=1 is satisfied by the title/alias hit — the
    // full-text query is never reached (L16). Its output is unaffected by
    // the skip either way, since a filled limit truncates extra hits away
    // regardless; this pins that preserved contract.
    let text = tools::search_text(&library, "einstein", 1).unwrap();
    assert!(text.contains("Albert Einstein"), "{text}");
    assert_eq!(text.lines().count(), 2, "header plus exactly one result: {text}");
}

#[test]
fn read_without_section_shows_lead_facts_and_a_top_level_outline() {
    let (_d, library) = imported();
    let text = tools::read_text(&library, "Albert Einstein", None, None).unwrap();
    assert!(text.starts_with("<article-text>Albert Einstein</article-text> · "), "{text}");
    assert!(text.contains("physicist who developed"), "{text}");
    assert!(text.contains("Born: 1879"), "{text}");
    assert!(text.contains("Died: 1955"), "{text}");
    assert!(text.contains("<article-text>") && text.contains("</article-text>"), "the lead prose is fenced: {text}");
    assert!(text.contains("Sections:"), "{text}");
    assert!(text.contains("0  Albert Einstein ("), "{text}");
    assert!(text.contains("1  Life ("), "{text}");
    assert!(text.contains("+1 subsections"), "the nested \"Early life\" is summarized, not spelled out: {text}");
    assert!(!text.contains("Early life ("), "a level-3 heading doesn't appear at the top level: {text}");
    assert!(text.contains("section=\"outline\""), "says how to get the rest: {text}");
    // The lead's own paragraphs only — no facts/list markup leaking into it.
    assert!(!text.contains("Born in Ulm"), "the Life section's body must not appear in the overview: {text}");
}

/// `&lt;/article-text&gt;` in an article's HTML source decodes, like any
/// other HTML entity, to a literal `</article-text>` by the time it reaches
/// `read`'s output. Unescaped, that closes the fence early and lets
/// whatever follows in the response read as server-written framing instead
/// of article text — reproduced here with the same trick against both the
/// close and open markers.
#[test]
fn read_lead_text_escapes_a_forged_fence_marker_instead_of_letting_it_close_the_fence() {
    let (_d, library) = library_with(
        ZimBuilder::new()
            .article(
                "Forger",
                "Forger",
                &page("Forger", "<p>Some articles write &lt;/article-text&gt; and &lt;article-text&gt; as literal text.</p>"),
            )
            .metadata("Title", "Tiny wiki")
            .build(),
    );

    let text = tools::read_text(&library, "Forger", None, None).unwrap();
    assert!(text.contains("&lt;/article-text&gt;"), "the article's own closing tag is escaped, not literal: {text}");
    assert!(text.contains("&lt;article-text&gt;"), "the article's own opening tag is escaped, not literal: {text}");
    // Only the two real fences this module wrote remain literal: one around
    // the header title, one around the lead prose.
    assert_eq!(text.matches("<article-text>").count(), 2, "{text}");
    assert_eq!(text.matches("</article-text>").count(), 2, "{text}");
}

#[test]
fn read_section_outline_keyword_returns_the_full_outline() {
    let (_d, library) = imported();
    let text = tools::read_text(&library, "Albert Einstein", Some("outline"), None).unwrap();
    assert!(text.contains("2    Early life ("), "the full outline lists every section, nested ones included: {text}");
}

#[test]
fn read_section_by_index_and_by_heading_resolve_the_same_section() {
    let (_d, library) = imported();
    let by_index = tools::read_text(&library, "Albert Einstein", Some("1"), None).unwrap();
    let by_heading = tools::read_text(&library, "Albert Einstein", Some("Life"), None).unwrap();
    assert_eq!(by_index, by_heading);
    assert!(by_index.contains("Born in Ulm"), "{by_index}");
}

#[test]
fn read_section_does_not_print_the_heading_twice() {
    let (_d, library) = imported();
    let text = tools::read_text(&library, "Albert Einstein", Some("Life"), None).unwrap();
    assert_eq!(text.matches("Life").count(), 1, "\"Life\" the heading appears once, not once in the header and again in the body: {text}");
}

#[test]
fn read_section_redirect_with_no_explicit_section_opens_that_section() {
    let (_d, library) = imported();
    let text = tools::read_text(&library, "Einstein early life", None, None).unwrap();
    assert!(text.starts_with("<article-text>Life</article-text> ("), "{text}");
    assert!(text.contains("Born in Ulm"), "{text}");
}

#[test]
fn read_section_redirect_with_an_unmatched_fragment_falls_back_to_the_overview() {
    let (_d, library) = imported();
    let text = tools::read_text(&library, "Stale reference", None, None).unwrap();
    assert!(
        text.starts_with("<article-text>Albert Einstein</article-text> · "),
        "a section redirect whose fragment matches nothing must not error: {text}"
    );
}

#[test]
fn links_section_redirect_with_an_unmatched_fragment_falls_back_to_the_whole_article() {
    let (_d, library) = imported();
    let text = tools::links_text(&library, "Stale reference", None).unwrap();
    assert!(text.contains("linked from \"<article-text>Albert Einstein</article-text>\""), "{text}");
}

#[test]
fn read_out_of_range_section_names_the_actual_outline() {
    let (_d, library) = imported();
    let err = tools::read_text(&library, "Albert Einstein", Some("99"), None).unwrap_err();
    assert!(err.contains("no section \"99\""), "{err}");
    assert!(err.contains("Sections:") || err.contains("0  Albert Einstein"), "names the outline: {err}");
}

#[test]
fn read_section_offset_past_the_end_is_an_error_naming_the_actual_length() {
    let (_d, library) = imported();
    let err = tools::read_text(&library, "Albert Einstein", Some("Life"), Some(9999)).unwrap_err();
    assert!(err.contains("past the end"), "{err}");
    assert!(err.contains("Life"), "{err}");
}

#[test]
fn resolve_section_numeric_fragment_matches_the_heading_not_the_outline_index() {
    use ok_core::document::{Document, Section};
    let doc = Document {
        entry: 1,
        path: "P".into(),
        title: "T".into(),
        sections: vec![
            Section { level: 1, heading: "T".into(), anchor: None, blocks: vec![] },
            Section { level: 2, heading: "Other".into(), anchor: None, blocks: vec![] },
            Section { level: 2, heading: "1".into(), anchor: Some("1".into()), blocks: vec![] },
        ],
    };
    // Explicit caller input: "1" means outline index 1 ("Other").
    assert_eq!(tools::resolve_section(&doc, "1", true), Some(1));
    // A fragment (as from a section redirect) is never reinterpreted as an
    // index, even when it looks like one — it must match the heading/
    // anchor literally named "1".
    assert_eq!(tools::resolve_section(&doc, "1", false), Some(2));
}

#[test]
fn read_unknown_article_is_an_error_with_suggestions() {
    let (_d, library) = imported();
    let err = tools::read_text(&library, "Not A Real Title At All", None, None).unwrap_err();
    assert!(err.contains("no article titled"), "{err}");
}

/// A near-miss never silently reads a different article (item 7's contract,
/// which already held structurally here — `resolve_article` only returns
/// `Ok` on `Resolution::Found`): `read` on a typo still errors, and when the
/// suggestions came from a shortened prefix, the error says so instead of
/// presenting them as an answer to the query as typed (item 8).
#[test]
fn read_near_miss_is_an_error_naming_the_fallback_prefix_when_one_was_used() {
    let (_d, library) = library_with(
        ZimBuilder::new()
            .article("Cross_product", "Cross product", &page("Cross product", "<p>A binary operation on vectors.</p>"))
            .redirect("Xyzzy", "Xyzzy", "Cross_product")
            .metadata("Title", "Tiny wiki")
            .build(),
    );

    // "Xyzzyq" (6 chars) only matches via "Xyzzy" (5 chars): isError, not a
    // silent read of "Cross product", and the message names the prefix.
    let err = tools::read_text(&library, "Xyzzyq", None, None).unwrap_err();
    assert!(err.contains("titles starting with \"Xyzzy\""), "{err}");
    assert!(err.contains("Cross product"), "{err}");

    // "Xyzzyqqq" (8 chars) is far enough that even the shortened prefix
    // ("Xyzzy", 5/8 retained) falls under the fraction floor: no
    // suggestions at all, not "Cross product" presented as a guess.
    let err = tools::read_text(&library, "Xyzzyqqq", None, None).unwrap_err();
    assert!(!err.contains("Cross product"), "{err}");
}

/// The same uncapped-retry-loop cost `/wiki/{path}` had (L6) is reachable
/// through `read` and `links` too, both via `resolve_article` — capped
/// there rather than in each caller.
#[test]
fn read_and_links_reject_an_oversized_article_name_instead_of_a_slow_resolve() {
    let (_d, library) = imported();
    let long = "a".repeat(201);
    let read_err = tools::read_text(&library, &long, None, None).unwrap_err();
    assert!(read_err.contains("at most"), "{read_err}");
    let links_err = tools::links_text(&library, &long, None).unwrap_err();
    assert!(links_err.contains("at most"), "{links_err}");
}

#[test]
fn lookup_error_never_leaks_the_underlying_error_detail() {
    let e = ok_core::Error::IndexMismatch {
        index: std::path::PathBuf::from("/Users/alex/private/data.okx"),
        expected: "abc123".into(),
        found: "def456".into(),
    };
    let msg = tools::lookup_error("Some Article", e);
    assert!(!msg.contains("/Users/alex/private"), "{msg}");
    assert!(!msg.contains("abc123") && !msg.contains("def456"), "{msg}");
    assert!(msg.contains("Some Article"), "{msg}");
}

#[test]
fn read_section_truncates_at_a_char_boundary_and_round_trips_with_offset() {
    let (_d, library) = imported();
    let full = tools::read_text(&library, "Albert Einstein", Some("Early life"), None).unwrap();
    assert!(full.contains("…[truncated: call read with offset="), "{}", &full[..200.min(full.len())]);

    let offset: usize = full.split("offset=").nth(1).unwrap().trim_end_matches(']').parse().unwrap();
    let continued = tools::read_text(&library, "Albert Einstein", Some("Early life"), Some(offset)).unwrap();
    assert!(!continued.contains("truncated"), "one continuation is enough for this fixture: {continued}");

    // Each call's shape is "{heading} ({total} chars)\n\n<article-text>\n{body}\n</article-text>[…marker]";
    // strip the header and fence, and the two bodies concatenated must
    // exactly reproduce the whole section's text minus its own heading
    // line (the header above already names it — see read_section_does_not_print_the_heading_twice).
    fn body_of(text: &str) -> &str {
        let start = text.find("<article-text>\n").unwrap() + "<article-text>\n".len();
        let end = text.find("\n</article-text>").unwrap();
        &text[start..end]
    }
    let mut joined = body_of(&full).to_string();
    joined.push_str(body_of(&continued));

    let target = match library.resolve_title("Albert Einstein").unwrap() {
        Resolution::Found(t) => t,
        Resolution::NotFound { .. } => panic!("fixture article must resolve"),
    };
    let doc = library.article(target.entry).unwrap();
    let index = tools::resolve_section(&doc, "Early life", true).unwrap();
    let heading = ok_core::text::sanitize(&doc.sections[index].heading);
    let whole = ok_core::text::sanitize(&doc.section_text(index));
    let whole_body = whole.strip_prefix(&heading).and_then(|s| s.strip_prefix('\n')).unwrap_or(&whole);
    assert_eq!(joined, whole_body);
}

#[test]
fn links_deduplicates_counts_unique_missing_and_external() {
    let (_d, library) = imported();
    let text = tools::links_text(&library, "Albert Einstein", None).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("1 unique articles linked from \"<article-text>Albert Einstein</article-text>\""), "{text}");
    assert!(lines.contains(&"<article-text>Theory of relativity</article-text>"), "{text}");
    assert!(text.contains("1 not in this collection"), "{text}");
    assert!(text.contains("1 external"), "{text}");
}

#[test]
fn links_with_a_section_names_the_section_not_the_article() {
    let (_d, library) = imported();
    let text = tools::links_text(&library, "Albert Einstein", Some("Life")).unwrap();
    let header = text.lines().next().unwrap();
    assert!(header.contains("linked from \"<article-text>Life</article-text>\""), "{header}");
    assert!(!header.contains("Albert Einstein"), "the section's own name, not the article's: {header}");
}

/// `Library::title` is the same raw dirent string as the title FST (L18),
/// so a linked-to article's title can carry the same forged newline.
#[test]
fn links_target_title_collapses_an_embedded_newline_and_is_fenced() {
    let (_d, library) = library_with(
        ZimBuilder::new()
            .article("Home", "Home", &page("Home", r#"<p>See <a href="Target">the target</a>.</p>"#))
            .article("Target", "Two\nLines", &page("Two Lines", "<p>Some prose.</p>"))
            .metadata("Title", "Tiny wiki")
            .build(),
    );

    let text = tools::links_text(&library, "Home", None).unwrap();
    assert!(text.contains("<article-text>Two Lines</article-text>"), "{text}");
    assert!(!text.lines().any(|l| l == "Lines"), "an embedded newline must not fake a second title line: {text}");
}

#[test]
fn links_unknown_article_is_an_error_with_suggestions() {
    let (_d, library) = imported();
    let err = tools::links_text(&library, "Not A Real Title At All", None).unwrap_err();
    assert!(err.contains("no article titled"), "{err}");
}

// ---------------------------------------------------------------------
// End-to-end through the real Mcp/ServerHandler: proves the rmcp wiring
// itself (routing, Parameters<T> deserialization, CallToolResult envelope).
// ---------------------------------------------------------------------

async fn connected_client(library: Library) -> (tokio::task::JoinHandle<()>, rmcp::service::RunningService<rmcp::RoleClient, ()>) {
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let running = Mcp::new(Arc::new(library)).serve(server_io).await.expect("server serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client serve");
    (server, client)
}

#[tokio::test]
async fn list_tools_returns_exactly_three_tools_with_required_fields() {
    let (_d, library) = imported();
    let (server, client) = connected_client(library).await;

    let tools = client.list_tools(None).await.unwrap().tools;
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(names, ["links", "read", "search"]);

    let required_of = |name: &str| -> Vec<String> {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        tool.input_schema.get("required").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert_eq!(required_of("search"), vec!["query"]);
    assert_eq!(required_of("read"), vec!["article"]);
    assert_eq!(required_of("links"), vec!["article"]);

    drop(client);
    server.abort();
}

#[tokio::test]
async fn call_tool_search_round_trips_through_rmcp() {
    let (_d, library) = imported();
    let (server, client) = connected_client(library).await;

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("Einstein"));
    let result = client.call_tool(CallToolRequestParams::new("search").with_arguments(args)).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert!(result.content[0].as_text().unwrap().text.contains("Albert Einstein"));

    drop(client);
    server.abort();
}

#[tokio::test]
async fn call_tool_read_round_trips_through_rmcp() {
    let (_d, library) = imported();
    let (server, client) = connected_client(library).await;

    let mut args = serde_json::Map::new();
    args.insert("article".to_string(), serde_json::json!("Albert Einstein"));
    let result = client.call_tool(CallToolRequestParams::new("read").with_arguments(args)).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert!(result.content[0].as_text().unwrap().text.starts_with("<article-text>Albert Einstein</article-text>"));

    drop(client);
    server.abort();
}

#[tokio::test]
async fn call_tool_links_round_trips_through_rmcp() {
    let (_d, library) = imported();
    let (server, client) = connected_client(library).await;

    let mut args = serde_json::Map::new();
    args.insert("article".to_string(), serde_json::json!("Albert Einstein"));
    let result = client.call_tool(CallToolRequestParams::new("links").with_arguments(args)).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert!(result.content[0].as_text().unwrap().text.contains("Theory of relativity"));

    drop(client);
    server.abort();
}

#[tokio::test]
async fn call_tool_unknown_article_is_a_tool_level_error_not_a_protocol_error() {
    let (_d, library) = imported();
    let (server, client) = connected_client(library).await;

    let mut args = serde_json::Map::new();
    args.insert("article".to_string(), serde_json::json!("Nonexistent Nowhere Title"));
    let result = client.call_tool(CallToolRequestParams::new("read").with_arguments(args)).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0].as_text().unwrap().text.contains("no article titled"));

    drop(client);
    server.abort();
}
