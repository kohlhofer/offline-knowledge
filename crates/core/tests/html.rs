//! Black-box tests for `Document::to_html`: build a `Document` directly (no
//! ZIM, no parser) and check the HTML it renders.

use ok_core::document::{Block, Cell, Document, Fact, Inline, Link, ListItem, Section, Style};

fn text(s: &str) -> Inline {
    Inline { text: s.into(), style: Style::default(), link: None }
}

fn linked(s: &str, link: Link) -> Inline {
    Inline { text: s.into(), style: Style::default(), link: Some(link) }
}

fn doc(sections: Vec<Section>) -> Document {
    Document { entry: 1, path: "Article".into(), title: "Article".into(), sections }
}

fn section(level: u8, heading: &str, blocks: Vec<Block>) -> Section {
    Section { level, heading: heading.into(), anchor: Some(heading.replace(' ', "_")), blocks }
}

fn paths(entry: u32) -> Option<String> {
    (entry == 2).then(|| "Other_Article".to_string())
}

#[test]
fn each_block_variant_renders_its_expected_tag() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![
            Block::Paragraph { content: vec![text("a paragraph")] },
            Block::Quote { content: vec![text("a quote")] },
            Block::Note { content: vec![text("see also")] },
            Block::List { ordered: true, items: vec![ListItem { depth: 0, content: vec![text("item one")] }] },
            Block::Table { rows: vec![vec![Cell { header: true, content: vec![text("h")] }, Cell { header: false, content: vec![text("d")] }]] },
            Block::Code { text: "fn main() {}".into() },
        ],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<p>a paragraph</p>"), "{html}");
    assert!(html.contains("<blockquote><p>a quote</p></blockquote>"), "{html}");
    assert!(html.contains("<p class=\"hatnote\">see also</p>"), "{html}");
    assert!(html.contains("<ol><li>item one</li></ol>"), "{html}");
    assert!(html.contains("<table><tr><th>h</th><td>d</td></tr></table>"), "{html}");
    assert!(html.contains("<pre><code>fn main() {}</code></pre>"), "{html}");
}

#[test]
fn script_tag_text_is_escaped_not_executed() {
    let d = doc(vec![section(1, "Article", vec![Block::Paragraph { content: vec![text("<script>alert(1)</script>")] }])]);
    let html = d.to_html(&paths);
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{html}");
}

#[test]
fn missing_link_is_a_real_anchor_not_an_inert_span() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("gone", Link::Missing { path: "Gone_Page".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<a class=\"link missing\" href=\"/wiki/Gone_Page\">gone</a>"), "{html}");
    assert!(!html.contains("<span"), "no inert span for a missing link: {html}");
}

#[test]
fn allowlisted_external_link_is_a_real_anchor_with_noreferrer_and_no_target() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("site", Link::External { url: "https://example.org/x".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<a class=\"link external\" href=\"https://example.org/x\" rel=\"noreferrer\">site</a>"), "{html}");
    assert!(!html.contains("target="), "{html}");
    assert!(html.contains("example.org"), "the domain is shown next to the link: {html}");
}

#[test]
fn external_link_href_and_domain_marker_are_sanitized() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("site", Link::External { url: "https://exa\u{7}mple.org/x".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<a class=\"link external\" href=\"https://example.org/x\" rel=\"noreferrer\">site</a>"), "{html}");
    assert!(!html.contains('\u{7}'), "the BEL control character must not reach the response: {html}");
    assert!(html.contains("example.org"), "{html}");
}

#[test]
fn javascript_scheme_link_renders_inert() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("click", Link::External { url: "javascript:alert(1)".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(!html.contains("<a"), "{html}");
    assert!(html.contains("click"), "the text still shows, just not as a link: {html}");
}

#[test]
fn unsafe_schemes_render_inert_including_mixed_case_and_leading_whitespace() {
    for url in ["data:text/html,<script>alert(1)</script>", "vbscript:msgbox(1)", "JavaScript:alert(1)", " javascript:alert(1)"] {
        let d = doc(vec![section(1, "Article", vec![Block::Paragraph { content: vec![linked("x", Link::External { url: url.into() })] }])]);
        let html = d.to_html(&paths);
        assert!(!html.contains("<a"), "{url} must render inert: {html}");
    }
}

#[test]
fn allowlisted_scheme_check_is_case_insensitive() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("site", Link::External { url: "HTTPS://example.org/x".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<a class=\"link external\""), "an allowed scheme in upper case must still render as a real link: {html}");
}

/// The real corpus's shape for a coordinate link (Germany's infobox): a
/// `geo:` URI with no `//` authority. `Document::links()`/the parser already
/// classify this as `Link::External` (see `document/tests.rs`), not
/// `Link::Missing` — this asserts the renderer's side of the same fix, that
/// an unlisted external scheme still renders as inert text, never a
/// "missing article" link offering to search the raw URI.
#[test]
fn non_web_uri_scheme_link_renders_inert_not_a_missing_article_link() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("Berlin", Link::External { url: "geo:52.5,13.4".into() })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(!html.contains("<a"), "{html}");
    assert!(!html.contains("link missing"), "{html}");
    assert!(html.contains("Berlin"), "the text still shows, just not as a link: {html}");
}

/// `E = mc²` linked to the same article: three `Inline`s (only "E" and "mc"
/// are italic), one logical link. Adjacent runs sharing a link merge into
/// one anchor, not one per run.
#[test]
fn adjacent_runs_sharing_an_article_link_merge_into_one_anchor() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph {
            content: vec![
                Inline { text: "E".into(), style: Style { bold: false, italic: true }, link: Some(Link::Article { entry: 2, fragment: None }) },
                Inline { text: " = ".into(), style: Style::default(), link: Some(Link::Article { entry: 2, fragment: None }) },
                Inline { text: "mc".into(), style: Style { bold: false, italic: true }, link: Some(Link::Article { entry: 2, fragment: None }) },
            ],
        }],
    )]);
    let html = d.to_html(&paths);
    assert_eq!(html.matches("<a ").count(), 1, "one anchor for the whole run, not one per style change: {html}");
    assert!(html.contains("<a class=\"link article\" href=\"/wiki/Other_Article\"><em>E</em> = <em>mc</em></a>"), "{html}");
}

/// Two adjacent runs pointing at the same external URL must not each carry
/// their own "↗ domain" marker.
#[test]
fn adjacent_runs_sharing_an_external_link_get_one_domain_marker() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph {
            content: vec![
                linked("Example", Link::External { url: "https://example.org/x".into() }),
                Inline { text: " Site".into(), style: Style::default(), link: Some(Link::External { url: "https://example.org/x".into() }) },
            ],
        }],
    )]);
    let html = d.to_html(&paths);
    assert_eq!(html.matches("<a ").count(), 1, "one anchor for the whole run: {html}");
    assert_eq!(html.matches('↗').count(), 1, "one domain marker for the whole run, not one per source run: {html}");
    assert!(html.contains("<a class=\"link external\" href=\"https://example.org/x\" rel=\"noreferrer\">Example Site</a>"), "{html}");
}

/// A different link (or no link) still starts a new anchor — merging must
/// not bleed across an actual boundary.
#[test]
fn different_adjacent_links_do_not_merge() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph {
            content: vec![
                linked("known", Link::Article { entry: 2, fragment: None }),
                text(" and "),
                linked("gone", Link::Missing { path: "Elsewhere".into() }),
            ],
        }],
    )]);
    let html = d.to_html(&paths);
    assert_eq!(html.matches("<a").count(), 2, "two distinct links must not merge into one: {html}");
}

#[test]
fn article_link_resolves_through_the_paths_closure_with_fragment() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("other", Link::Article { entry: 2, fragment: Some("Life".into()) })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<a class=\"link article\" href=\"/wiki/Other_Article#Life\">other</a>"), "{html}");
}

#[test]
fn article_link_whose_path_lookup_fails_renders_as_missing_not_silently_as_plain_text() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![Block::Paragraph { content: vec![linked("gone", Link::Article { entry: 99, fragment: None })] }],
    )]);
    let html = d.to_html(&paths);
    assert!(html.contains("<span class=\"link missing\">gone</span>"), "{html}");
    assert!(!html.contains("<a"), "no real link when the path lookup failed: {html}");
}

#[test]
fn lead_section_puts_the_first_paragraph_before_the_infobox_aside() {
    let d = doc(vec![section(
        1,
        "Article",
        vec![
            Block::Note { content: vec![text("a hatnote before the lead paragraph")] },
            Block::Paragraph { content: vec![text("the lead paragraph")] },
            Block::Facts { facts: vec![Fact { label: "Born".into(), value: vec![text("1879")] }] },
            Block::Paragraph { content: vec![text("a second paragraph")] },
        ],
    )]);
    let html = d.to_html(&paths);
    let lead_para = html.find("the lead paragraph").unwrap();
    let aside_open = html.find("<aside class=\"infobox\">").unwrap();
    let aside_close = html.find("</aside>").unwrap();
    let hatnote = html.find("a hatnote").unwrap();
    let second_para = html.find("a second paragraph").unwrap();
    assert!(lead_para < aside_open, "the first paragraph renders before the infobox: {html}");
    assert!(aside_open < aside_close, "{html}");
    // The infobox is pulled out of its original position (after the hatnote,
    // before the second paragraph) to right after the lead paragraph.
    assert!(aside_open < hatnote || hatnote < lead_para, "{html}");
    assert!(aside_close < second_para, "the rest of the section still follows, in order: {html}");
}

/// Real corpus shape: `Definition_of_"racial_discrimination"`. The server's
/// half of the L3/S1 fix is escaping this in the `id`/`data-path`
/// attributes (unchanged here — `escape_html` already covered `"`); the
/// client half, that app.js's outline dialog no longer re-interpolates the
/// browser-decoded quote into an `href` string, lives in
/// `serve/assets/app.js` and has no automated coverage (no JS test harness
/// in this repo; verified manually in Chrome).
#[test]
fn heading_anchor_containing_a_quote_is_escaped_in_the_id_attribute() {
    let d = doc(vec![section(2, "Definition of \"racial discrimination\"", vec![])]);
    let html = d.to_html(&paths);
    assert!(
        html.contains("<h2 id=\"Definition_of_&quot;racial_discrimination&quot;\" data-path=\"Definition of &quot;racial discrimination&quot;\">"),
        "{html}"
    );
    assert!(!html.contains("id=\"Definition_of_\"racial_discrimination\"\""), "the raw quote must never appear unescaped inside the attribute: {html}");
}

/// A byte-exact pin over one fixture touching every block and link kind:
/// the `esc_into`-based fused sanitize+escape rewrite (C1) must not change
/// a single byte of output, only how it gets built.
#[test]
fn to_html_output_is_byte_identical_over_a_fixture_touching_every_kind() {
    let d = doc(vec![
        section(
            1,
            "Article",
            vec![
                Block::Paragraph {
                    content: vec![
                        Inline { text: "Bold and italic".into(), style: Style { bold: true, italic: true }, link: None },
                        text(" plain "),
                        linked("known", Link::Article { entry: 2, fragment: Some("Sec".into()) }),
                        text(" "),
                        linked("dangling", Link::Article { entry: 99, fragment: None }),
                        text(" "),
                        linked("gone", Link::Missing { path: "Gone_Page".into() }),
                        text(" "),
                        linked("here", Link::Anchor { fragment: "Sec".into() }),
                        text(" "),
                        linked("ext", Link::External { url: "https://example.org/x?a=1&b=2".into() }),
                        text(" "),
                        linked("bad", Link::External { url: "javascript:alert(1)".into() }),
                    ],
                },
                Block::Quote { content: vec![text("a quote")] },
                Block::Note { content: vec![text("see also")] },
                Block::List {
                    ordered: false,
                    items: vec![
                        ListItem { depth: 0, content: vec![text("one")] },
                        ListItem { depth: 1, content: vec![text("one-a")] },
                    ],
                },
                Block::Facts {
                    facts: vec![
                        Fact { label: "Heading".into(), value: vec![] },
                        Fact { label: String::new(), value: vec![text("caption")] },
                        Fact { label: "Born".into(), value: vec![text("1879")] },
                    ],
                },
                Block::Table {
                    rows: vec![vec![Cell { header: true, content: vec![text("H")] }, Cell { header: false, content: vec![text("D")] }]],
                },
                Block::Code { text: "fn f() {}".into() },
            ],
        ),
        section(2, "Sub \"heading\"", vec![Block::Paragraph { content: vec![text("nested")] }]),
    ]);
    let html = d.to_html(&paths);
    assert_eq!(
        html,
        "<h1 id=\"Article\" data-path=\"Article\">Article</h1><p><strong><em>Bold and italic</em></strong> plain \
<a class=\"link article\" href=\"/wiki/Other_Article#Sec\">known</a> <span class=\"link missing\">dangling</span> \
<a class=\"link missing\" href=\"/wiki/Gone_Page\">gone</a> <a href=\"#Sec\">here</a> \
<a class=\"link external\" href=\"https://example.org/x?a=1&amp;b=2\" rel=\"noreferrer\">ext</a>\
<span class=\"external\"> ↗ example.org</span> bad</p><aside class=\"infobox\"><table class=\"facts\">\
<tr><th colspan=\"2\">Heading</th></tr><tr><td colspan=\"2\">caption</td></tr><tr><th>Born</th><td>1879</td></tr>\
</table></aside><blockquote><p>a quote</p></blockquote><p class=\"hatnote\">see also</p>\
<ul><li>one<ul><li>one-a</li></ul></li></ul><table><tr><th>H</th><td>D</td></tr></table>\
<pre><code>fn f() {}</code></pre>\
<h2 id=\"Sub_&quot;heading&quot;\" data-path=\"Sub &quot;heading&quot;\">Sub &quot;heading&quot;</h2><p>nested</p>",
        "{html}"
    );
}

#[test]
fn heading_carries_an_id_and_a_data_path_breadcrumb() {
    // The lead section always has `anchor: None` from the parser, so its id
    // falls back to a slugified heading.
    let lead = Section { level: 1, heading: "Albert Einstein".into(), anchor: None, blocks: vec![] };
    let d = doc(vec![lead, section(2, "Life and career", vec![]), section(3, "Early life", vec![])]);
    let html = d.to_html(&paths);
    assert!(html.contains("<h1 id=\"Albert_Einstein\" data-path=\"Albert Einstein\">Albert Einstein</h1>"), "{html}");
    assert!(
        html.contains("<h2 id=\"Life_and_career\" data-path=\"Life and career\">Life and career</h2>"),
        "{html}"
    );
    assert!(
        html.contains("<h3 id=\"Early_life\" data-path=\"Life and career › Early life\">Early life</h3>"),
        "{html}"
    );
}
