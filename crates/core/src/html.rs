//! Renders a [`Document`] to HTML. Hand-rolled, no templating crate: every
//! text run is escaped, every link is classified before it becomes an
//! `<a>`, and the lead section's infobox is pulled out into an `<aside>`.
//!
//! `paths` maps an article entry to its `/wiki/{path}` path (about 1 µs per
//! lookup); it is a closure rather than a `&Library` so this module stays
//! free of any dependency on how a caller stores articles.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::document::{Block, Cell, Document, Fact, Inline, Link, ListItem, Section};
use crate::text::sanitize;

/// Unreserved URL characters kept literal; everything else (including `"`
/// and `<`) is percent-encoded, so the output is always a safe attribute
/// value with no further escaping needed for the path itself.
const PATH_SAFE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~').remove(b'/');

impl Document {
    /// The article as HTML: one heading element per section (carrying an
    /// `id` and a `data-path` breadcrumb) followed by its blocks, with the
    /// lead section's infobox pulled into a floated `<aside>`.
    pub fn to_html(&self, paths: &dyn Fn(u32) -> Option<String>) -> String {
        let mut out = String::new();
        for (index, section) in self.sections.iter().enumerate() {
            render_heading(&mut out, &self.sections, index);
            if index == 0 {
                render_lead_blocks(&mut out, &section.blocks, paths);
            } else {
                for block in &section.blocks {
                    render_block(&mut out, block, paths);
                }
            }
        }
        out
    }
}

fn render_heading(out: &mut String, sections: &[Section], index: usize) {
    let section = &sections[index];
    let level = section.level.clamp(1, 6);
    let tag = if level <= 1 { "h1".to_string() } else { format!("h{level}") };
    let anchor = section.anchor.clone().unwrap_or_else(|| slugify(&section.heading));
    let path = data_path(sections, index);
    out.push_str(&format!(
        "<{tag} id=\"{}\" data-path=\"{}\">{}</{tag}>",
        escape_html(&sanitize(&anchor)),
        escape_html(&sanitize(&path)),
        escape_html(&sanitize(&section.heading)),
    ));
}

/// Headings of the sections that contain section `index`, outermost first,
/// leaving out the article title. Mirrors `tui::outline::ancestors`, which
/// operates on the TUI's own `SectionSpot`, not `ok_core::document::Section`.
fn ancestors(sections: &[Section], index: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut level = sections[index].level;
    for s in sections[..index].iter().rev() {
        if s.level < level && s.level > 1 {
            out.push(s.heading.as_str());
            level = s.level;
        }
    }
    out.reverse();
    out
}

fn data_path(sections: &[Section], index: usize) -> String {
    let mut parts = ancestors(sections, index);
    parts.push(sections[index].heading.as_str());
    parts.join(" › ")
}

/// A deterministic id for a heading with no anchor from the source (the lead
/// section always lacks one). Not a MediaWiki-compatible slug, just stable
/// and unique enough to be a fragment target.
fn slugify(heading: &str) -> String {
    let mut out = String::new();
    let mut last_was_space = false;
    for c in heading.trim().chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push('_');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

/// The lead section: the first paragraph, then any facts as a floated
/// infobox, then the rest of the section in original order. Every other
/// section renders in document order (see `to_html`).
fn render_lead_blocks(out: &mut String, blocks: &[Block], paths: &dyn Fn(u32) -> Option<String>) {
    let first_paragraph = blocks.iter().position(|b| matches!(b, Block::Paragraph { .. }));
    if let Some(i) = first_paragraph {
        render_block(out, &blocks[i], paths);
    }
    for block in blocks.iter().filter(|b| matches!(b, Block::Facts { .. })) {
        out.push_str("<aside class=\"infobox\">");
        render_block(out, block, paths);
        out.push_str("</aside>");
    }
    for (i, block) in blocks.iter().enumerate() {
        if Some(i) == first_paragraph || matches!(block, Block::Facts { .. }) {
            continue;
        }
        render_block(out, block, paths);
    }
}

fn render_block(out: &mut String, block: &Block, paths: &dyn Fn(u32) -> Option<String>) {
    match block {
        Block::Paragraph { content } => {
            out.push_str("<p>");
            render_inlines(out, content, paths);
            out.push_str("</p>");
        }
        Block::Quote { content } => {
            out.push_str("<blockquote><p>");
            render_inlines(out, content, paths);
            out.push_str("</p></blockquote>");
        }
        Block::Note { content } => {
            out.push_str("<p class=\"hatnote\">");
            render_inlines(out, content, paths);
            out.push_str("</p>");
        }
        Block::List { ordered, items } => render_list(out, *ordered, items, paths),
        Block::Facts { facts } => render_facts(out, facts, paths),
        Block::Table { rows } => render_table(out, rows, paths),
        Block::Code { text } => {
            out.push_str("<pre><code>");
            out.push_str(&escape_html(&sanitize(text)));
            out.push_str("</code></pre>");
        }
    }
}

/// Renders a flat, depth-tagged list as properly nested `<ul>`/`<ol>`. The
/// document model only ever increases depth by one level at a time (the
/// parser's list/definition-list recursion always does), so a deeper jump
/// in one step is not handled specially.
fn render_list(out: &mut String, ordered: bool, items: &[ListItem], paths: &dyn Fn(u32) -> Option<String>) {
    let (open_tag, close_tag) = if ordered { ("<ol>", "</ol>") } else { ("<ul>", "</ul>") };
    out.push_str(open_tag);
    let mut depth: u8 = 0;
    let mut first = true;
    for item in items {
        if item.depth > depth {
            for _ in depth..item.depth {
                out.push_str(open_tag);
            }
        } else {
            if !first {
                out.push_str("</li>");
            }
            for _ in item.depth..depth {
                out.push_str(close_tag);
                out.push_str("</li>");
            }
        }
        depth = item.depth;
        out.push_str("<li>");
        render_inlines(out, &item.content, paths);
        first = false;
    }
    if !first {
        out.push_str("</li>");
    }
    for _ in 0..depth {
        out.push_str(close_tag);
        out.push_str("</li>");
    }
    out.push_str(close_tag);
}

/// A fact with a value is a `label: value` row; one with an empty value is a
/// heading inside the box; one with an empty label is a caption row.
fn render_facts(out: &mut String, facts: &[Fact], paths: &dyn Fn(u32) -> Option<String>) {
    out.push_str("<table class=\"facts\">");
    for fact in facts {
        out.push_str("<tr>");
        match (fact.label.is_empty(), fact.value.is_empty()) {
            (false, true) => {
                out.push_str("<th colspan=\"2\">");
                out.push_str(&escape_html(&sanitize(&fact.label)));
                out.push_str("</th>");
            }
            (true, false) => {
                out.push_str("<td colspan=\"2\">");
                render_inlines(out, &fact.value, paths);
                out.push_str("</td>");
            }
            _ => {
                out.push_str("<th>");
                out.push_str(&escape_html(&sanitize(&fact.label)));
                out.push_str("</th><td>");
                render_inlines(out, &fact.value, paths);
                out.push_str("</td>");
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

fn render_table(out: &mut String, rows: &[Vec<Cell>], paths: &dyn Fn(u32) -> Option<String>) {
    out.push_str("<table>");
    for row in rows {
        out.push_str("<tr>");
        for cell in row {
            let tag = if cell.header { "th" } else { "td" };
            out.push_str(&format!("<{tag}>"));
            render_inlines(out, &cell.content, paths);
            out.push_str(&format!("</{tag}>"));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

fn render_inlines(out: &mut String, content: &[Inline], paths: &dyn Fn(u32) -> Option<String>) {
    for inline in content {
        render_inline(out, inline, paths);
    }
}

fn render_inline(out: &mut String, inline: &Inline, paths: &dyn Fn(u32) -> Option<String>) {
    let mut style_open = String::new();
    let mut style_close = String::new();
    if inline.style.bold {
        style_open.push_str("<strong>");
        style_close.insert_str(0, "</strong>");
    }
    if inline.style.italic {
        style_open.push_str("<em>");
        style_close.insert_str(0, "</em>");
    }
    let (link_open, link_close) = inline.link.as_ref().map(|l| render_link(l, paths)).unwrap_or_default();
    out.push_str(&link_open);
    out.push_str(&style_open);
    render_text(out, &inline.text);
    out.push_str(&style_close);
    out.push_str(&link_close);
}

/// Escapes text, turning an internal `\n` (used for in-paragraph breaks such
/// as an address split over lines) into `<br>`.
fn render_text(out: &mut String, text: &str) {
    let mut first = true;
    for line in sanitize(text).split('\n') {
        if !first {
            out.push_str("<br>");
        }
        out.push_str(&escape_html(line));
        first = false;
    }
}

/// The open/close tag pair for a link run, or two empty strings when the
/// link renders as inert text (an unlisted external scheme).
fn render_link(link: &Link, paths: &dyn Fn(u32) -> Option<String>) -> (String, String) {
    match link {
        Link::Article { entry, fragment } => match paths(*entry) {
            Some(path) => {
                let href = wiki_href(&path, fragment.as_deref());
                (format!("<a class=\"link article\" href=\"{}\">", escape_html(&href)), "</a>".to_string())
            }
            None => (String::new(), String::new()),
        },
        Link::Missing { path } => {
            let href = wiki_href(path, None);
            (format!("<a class=\"link missing\" href=\"{}\">", escape_html(&href)), "</a>".to_string())
        }
        Link::Anchor { fragment } => (format!("<a href=\"#{}\">", escape_html(&encode(fragment))), "</a>".to_string()),
        Link::External { url } => {
            if is_allowed_scheme(url) {
                let marker = format!("</a><span class=\"external\"> ↗ {}</span>", escape_html(&domain_of(url)));
                (format!("<a class=\"link external\" href=\"{}\" rel=\"noreferrer\">", escape_html(url)), marker)
            } else {
                (String::new(), String::new())
            }
        }
    }
}

fn is_allowed_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("mailto:")
}

/// A hand-rolled host extractor: no `url` crate dependency for one field.
fn domain_of(url: &str) -> String {
    let host_part = match url.strip_prefix("mailto:") {
        Some(addr) => addr.rsplit('@').next().unwrap_or(addr),
        None => url.split_once("://").map_or(url, |(_, rest)| rest),
    };
    let end = host_part.find(['/', '?', '#']).unwrap_or(host_part.len());
    host_part[..end].to_string()
}

fn encode(path: &str) -> String {
    utf8_percent_encode(path, PATH_SAFE).to_string()
}

/// A percent-encoded `/wiki/{path}[#fragment]` href, the canonical URL for
/// an article. Shared by the article renderer above and `ok serve`'s route
/// handlers, so there is exactly one place that knows how a path becomes a URL.
pub fn wiki_href(path: &str, fragment: Option<&str>) -> String {
    format!("/wiki/{}{}", encode(path), fragment.map(|f| format!("#{}", encode(f))).unwrap_or_default())
}

/// Escapes `&`, `<`, `>`, `"` and `'`. Every piece of ZIM-sourced or
/// user-supplied text this crate or a frontend writes into HTML goes through
/// this (after [`crate::text::sanitize`]) rather than a one-off escaper.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
