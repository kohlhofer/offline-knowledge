use super::*;

fn resolver(path: &str) -> Option<Target> {
    let entry = match path {
        "Theoretical_physicist" => Some(10),
        "Theory_of_relativity" => Some(11),
        "Mass–energy_equivalence" => Some(12),
        "AC/DC" => Some(13),
        "Einstein_family" => Some(14),
        "Foo" => Some(15),
        "Section_redirect" => Some(16),
        _ => None,
    }?;
    let fragment = (entry == 16).then(|| "Plot".to_string());
    Some(Target { entry, fragment })
}

fn parse(body: &str) -> Document {
    parse_with_path(body, "Albert_Einstein")
}

fn parse_with_path(body: &str, path: &str) -> Document {
    let html = format!(
        r#"<!DOCTYPE html><html><head><title>T</title><style>.x{{}}</style></head><body>
        <main><header><h1 id="firstHeading"><span class="mw-page-title-main">Albert Einstein</span></h1></header>
        <div id="mw-content-text"><div class="mw-parser-output">{body}</div></div>
        <div class="zim-footer">This article is issued from Wikipedia.</div></main></body></html>"#
    );
    let ctx = ArticleContext { entry: 1, path, title: "fallback" };
    parse_article(&html, &ctx, &resolver)
}

fn texts(content: &[Inline]) -> Vec<&str> {
    content.iter().map(|i| i.text.as_str()).collect()
}

#[test]
fn lead_paragraph_with_links_styles_and_references_removed() {
    let doc = parse(
        r##"<p class="mw-empty-elt"> </p>
        <p><b>Albert Einstein</b><sup id="cite_ref-6" class="reference"><a href="#cite_note-6"><span class="cite-bracket">[</span>a<span class="cite-bracket">]</span></a></sup> (14 March 1879&nbsp;– 18 April 1955) was a German-born <a href="Theoretical_physicist" class="mw-redirect">theoretical physicist</a> best known for the <a href="Theory_of_relativity">theory of
        relativity</a>. His <a href="Mass%E2%80%93energy_equivalence#Mass–velocity_relationship"><i>E</i> = <i>mc</i><sup>2</sup></a> formula.</p>"##,
    );
    assert_eq!(doc.title, "Albert Einstein");
    assert_eq!(doc.sections.len(), 1);
    let Block::Paragraph { content } = &doc.sections[0].blocks[0] else { panic!("{:?}", doc.sections[0].blocks) };
    assert_eq!(
        inline_text(content),
        "Albert Einstein (14 March 1879 – 18 April 1955) was a German-born theoretical physicist best known for the theory of relativity. His E = mc2 formula."
    );
    assert!(content[0].style.bold);
    let physicist = content.iter().find(|i| i.text == "theoretical physicist").unwrap();
    assert_eq!(physicist.link, Some(Link::Article { entry: 10, fragment: None }));
    let relativity = content.iter().find(|i| i.text == "theory of relativity").unwrap();
    assert_eq!(relativity.link, Some(Link::Article { entry: 11, fragment: None }));
    let e = content.iter().find(|i| i.text == "E").unwrap();
    assert!(e.style.italic);
    assert_eq!(e.link, Some(Link::Article { entry: 12, fragment: Some("Mass–velocity_relationship".into()) }));
    assert!(!inline_text(content).contains("cite"));
}

#[test]
fn spaces_stay_outside_link_text() {
    let doc = parse(r#"<p>a <a href="Foo">link</a> b</p>"#);
    let Block::Paragraph { content } = &doc.sections[0].blocks[0] else { panic!() };
    assert_eq!(texts(content), ["a ", "link", " b"]);
}

#[test]
fn headings_make_sections_and_empty_ones_are_pruned() {
    let doc = parse(
        r#"<p>Lead.</p>
        <div class="mw-heading mw-heading2"><h2 id="Life_and_career">Life and career</h2></div>
        <div class="mw-heading mw-heading3"><h3 id="Childhood">Childhood</h3></div>
        <div role="note" class="hatnote navigation-not-searchable">See also: <a href="Einstein_family">Einstein family</a></div>
        <p>Born in Ulm.</p>
        <div class="mw-heading mw-heading2"><h2 id="References">References</h2></div>
        <div class="mw-references-wrap"><ol class="references"><li>cite</li></ol></div>
        <div class="mw-heading mw-heading2"><h2 id="Legacy">Legacy</h2></div>
        <div class="mw-heading mw-heading3"><h3 id="Empty">Empty</h3></div>
        <p>Still read.</p>"#,
    );
    let headings: Vec<(u8, &str)> = doc.sections.iter().map(|s| (s.level, s.heading.as_str())).collect();
    assert_eq!(headings, [(1, "Albert Einstein"), (2, "Life and career"), (3, "Childhood"), (2, "Legacy"), (3, "Empty")]);
    assert_eq!(doc.sections[2].anchor.as_deref(), Some("Childhood"));
    assert!(matches!(&doc.sections[2].blocks[0], Block::Note { content } if inline_text(content) == "See also: Einstein family"));
    // "Legacy" has no blocks of its own but keeps its non-empty child.
    assert!(doc.sections[3].blocks.is_empty());
}

#[test]
fn nested_lists_flatten_with_depth() {
    let doc = parse(
        r#"<ul><li>One<ul><li>One-a</li><li>One-b<ol><li>deep</li></ol></li></ul></li><li>Two</li></ul>
        <dl><dt>Term</dt><dd>Definition</dd></dl>"#,
    );
    let Block::List { ordered, items } = &doc.sections[0].blocks[0] else { panic!() };
    assert!(!ordered);
    let flat: Vec<(u8, String)> = items.iter().map(|i| (i.depth, inline_text(&i.content))).collect();
    assert_eq!(
        flat,
        [(0, "One".into()), (1, "One-a".into()), (1, "One-b".into()), (2, "deep".into()), (0, "Two".into())]
    );
    let Block::List { items, .. } = &doc.sections[0].blocks[1] else { panic!() };
    assert_eq!(items[0].depth, 0);
    assert!(items[0].content[0].style.bold);
    assert_eq!(items[1].depth, 1);
}

#[test]
fn infobox_becomes_facts_and_navboxes_vanish() {
    let doc = parse(
        r#"<table class="infobox biography vcard"><tbody>
          <tr><th colspan="2" class="infobox-above"><div class="fn">Albert Einstein</div></th></tr>
          <tr><td colspan="2" class="infobox-image"><div class="infobox-caption">Einstein in 1947</div></td></tr>
          <tr><th scope="row" class="infobox-label">Born</th><td class="infobox-data">14 March 1879<br>Ulm</td></tr>
          <tr><th colspan="2" class="infobox-header">Scientific career</th></tr>
          <tr><th class="infobox-label">Fields</th><td class="infobox-data"><a href="Foo">Physics</a></td></tr>
        </tbody></table>
        <div role="navigation" class="navbox"><table><tr><td>nav</td></tr></table></div>
        <p>Body.</p>"#,
    );
    let Block::Facts { facts } = &doc.sections[0].blocks[0] else { panic!("{:?}", doc.sections[0].blocks) };
    let flat: Vec<(&str, String)> = facts.iter().map(|f| (f.label.as_str(), inline_text(&f.value))).collect();
    assert_eq!(flat, [("Born", "14 March 1879\nUlm".into()), ("Scientific career", String::new()), ("Fields", "Physics".into())]);
    assert_eq!(doc.sections[0].blocks.len(), 2);
    assert!(!doc.plain_text().contains("nav"));
}

#[test]
fn tables_quotes_code_and_math() {
    let doc = parse(
        r#"<table class="wikitable"><tr><th>Year</th><th>Prize</th></tr><tr><td>1921</td><td>Nobel</td></tr></table>
        <blockquote class="templatequote"><p>Imagination is more important.</p></blockquote>
        <pre>fn main() {}
</pre>
        <p><span class="mwe-math-element"><math alttext="{\displaystyle E=mc^{2}}"><semantics><mi>E</mi></semantics></math></span></p>"#,
    );
    let blocks = &doc.sections[0].blocks;
    let Block::Table { rows } = &blocks[0] else { panic!() };
    assert!(rows[0][0].header);
    assert_eq!(inline_text(&rows[1][1].content), "Nobel");
    assert!(matches!(&blocks[1], Block::Quote { content } if inline_text(content) == "Imagination is more important."));
    assert!(matches!(&blocks[2], Block::Code { text } if text == "fn main() {}"));
    assert!(matches!(&blocks[3], Block::Paragraph { content } if inline_text(content) == "{\\displaystyle E=mc^{2}}"));
}

#[test]
fn link_kinds_and_relative_paths() {
    let doc = parse_with_path(
        r##"<p><a href="#Life">here</a> <a href="https://example.org/x">ext</a> <a href="Missing_page">gone</a> <a href="./Foo">dot</a> <a href="../AC/DC">slash</a></p>"##,
        "Some/Nested",
    );
    let Block::Paragraph { content } = &doc.sections[0].blocks[0] else { panic!() };
    let links: Vec<Option<Link>> = content.iter().filter(|i| i.link.is_some()).map(|i| i.link.clone()).collect();
    assert_eq!(
        links,
        [
            Some(Link::Anchor { fragment: "Life".into() }),
            Some(Link::External { url: "https://example.org/x".into() }),
            Some(Link::Missing { path: "Some/Missing_page".into() }),
            Some(Link::Missing { path: "Some/Foo".into() }),
            Some(Link::Article { entry: 13, fragment: None }),
        ]
    );
    let doc = parse(r#"<p><a href="./Foo">dot</a> <a href="Section_redirect">stub</a> <a href="Section_redirect#Cast">stub</a></p>"#);
    assert_eq!(doc.article_links().collect::<Vec<_>>(), [15, 16, 16]);
    let fragments: Vec<Option<&str>> = doc
        .links()
        .filter_map(|l| match l {
            Link::Article { entry: 16, fragment } => Some(fragment.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(fragments, [Some("Plot"), Some("Cast")]);
}

#[test]
fn classifies_hrefs_relative_to_the_page() {
    assert_eq!(classify_href("A/B", "../C#x%20y"), Some(Href::Internal { path: "C".into(), fragment: Some("x y".into()) }));
    assert_eq!(classify_href("Page", "./Other#"), Some(Href::Internal { path: "Other".into(), fragment: None }));
    assert_eq!(classify_href("Page", "Other?action=edit"), Some(Href::Internal { path: "Other".into(), fragment: None }));
    assert_eq!(classify_href("Page", "  "), None);
}

#[test]
fn invisible_characters_are_dropped_and_unlabeled_facts_print_plainly() {
    let doc = parse(
        "<table class=\"infobox\"><tr><th>Spouses</th><td>Mileva Mari\u{107}<br>\u{200b}(m.&#8203; 1903)</td></tr>\
         <tr><td colspan=\"2\">Scientific career</td></tr></table>",
    );
    let text = doc.plain_text();
    assert!(!text.contains('\u{200b}'), "{text:?}");
    assert!(text.contains("Spouses: Mileva Mari\u{107}\n(m. 1903)"), "{text:?}");
    assert!(text.contains("\nScientific career\n"), "{text:?}");
}

#[test]
fn summary_cuts_at_a_word() {
    let doc = parse("<p>One two three four five six seven.</p>");
    assert_eq!(doc.summary(100), "One two three four five six seven.");
    assert_eq!(doc.summary(15), "One two three…");
}

#[test]
fn falls_back_to_body_and_given_title() {
    let ctx = ArticleContext { entry: 3, path: "X", title: "Given" };
    let doc = parse_article("<html><body><p>Plain page</p></body></html>", &ctx, &resolver);
    assert_eq!(doc.title, "Given");
    assert!(matches!(&doc.sections[0].blocks[0], Block::Paragraph { content } if inline_text(content) == "Plain page"));
}
