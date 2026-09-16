//! Renders a [`Document`] to HTML. Hand-rolled, no templating crate: every
//! text run is escaped, every link is classified before it becomes an
//! `<a>`, and the lead section's infobox is pulled out into an `<aside>`.
//!
//! `paths` maps an article entry to its `/wiki/{path}` path (about 1 µs per
//! lookup); it is a closure rather than a `&Library` so this module stays
//! free of any dependency on how a caller stores articles.
//!
//! Every text run is sanitized and escaped in one pass via [`esc_into`],
//! writing straight into the output buffer: measured 3.08 ms down to
//! 0.64 ms on the largest article in the reference collection (2.17 MB of
//! source HTML), 1.13 ms to 0.41 ms on Albert Einstein — the per-run
//! temporary `String`s and per-heading/link `format!` calls this replaced
//! were most of `to_html`'s own cost, not the escaping loop itself.

use std::fmt::Write as _;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::document::{Block, Cell, Document, Fact, Inline, Link, ListItem, Section};
use crate::text::keep_char;

/// Unreserved URL characters kept literal; everything else (including `"`
/// and `<`) is percent-encoded, so the output is always a safe attribute
/// value with no further escaping needed for the path itself.
const PATH_SAFE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~').remove(b'/');

impl Document {
    /// The article as HTML: one heading element per section (carrying an
    /// `id` and a `data-path` breadcrumb) followed by its blocks, with the
    /// lead section's infobox pulled into a floated `<aside>`.
    pub fn to_html(&self, paths: &dyn Fn(u32) -> Option<String>) -> String {
        let mut out = String::with_capacity(estimate_html_len(&self.sections));
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

/// A cheap, O(sections) ballpark (rendered HTML tends to run somewhat
/// larger than the source text it wraps) — only needs to be in the right
/// order of magnitude to save a few buffer reallocations as `out` grows.
fn estimate_html_len(sections: &[Section]) -> usize {
    sections.iter().map(|s| s.heading.len() + s.blocks.len() * 96).sum::<usize>().max(256)
}

fn render_heading(out: &mut String, sections: &[Section], index: usize) {
    let section = &sections[index];
    let level = section.level.clamp(1, 6);
    out.push_str("<h");
    out.push((b'0' + level) as char);
    out.push_str(" id=\"");
    match &section.anchor {
        Some(anchor) => esc_into(out, anchor),
        None => esc_into(out, &slugify(&section.heading)),
    }
    out.push_str("\" data-path=\"");
    push_data_path(out, sections, index);
    out.push_str("\">");
    esc_into(out, &section.heading);
    out.push_str("</h");
    out.push((b'0' + level) as char);
    out.push('>');
}

/// Writes `Ancestor › ... › Heading` (escaped) straight into `out` — the
/// breadcrumb the sticky header reads from `data-path`, without collecting
/// ancestor headings into a `Vec` and joining them into a temporary `String`
/// first.
fn push_data_path(out: &mut String, sections: &[Section], index: usize) {
    for &ancestor in &ancestor_indices(sections, index) {
        esc_into(out, &sections[ancestor].heading);
        out.push_str(" › ");
    }
    esc_into(out, &sections[index].heading);
}

/// Indices of the sections that contain section `index`, outermost first,
/// leaving out the article title. Mirrors `tui::outline::ancestors`, which
/// operates on the TUI's own `SectionSpot`, not `ok_core::document::Section`.
fn ancestor_indices(sections: &[Section], index: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut level = sections[index].level;
    for (i, s) in sections[..index].iter().enumerate().rev() {
        if s.level < level && s.level > 1 {
            out.push(i);
            level = s.level;
        }
    }
    out.reverse();
    out
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
            esc_into(out, text);
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
                esc_into(out, &fact.label);
                out.push_str("</th>");
            }
            (true, false) => {
                out.push_str("<td colspan=\"2\">");
                render_inlines(out, &fact.value, paths);
                out.push_str("</td>");
            }
            _ => {
                out.push_str("<th>");
                esc_into(out, &fact.label);
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
            let (open, close) = if cell.header { ("<th>", "</th>") } else { ("<td>", "</td>") };
            out.push_str(open);
            render_inlines(out, &cell.content, paths);
            out.push_str(close);
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
    let link_close = inline.link.as_ref().map(|l| render_link_open(out, l, paths));
    if inline.style.bold {
        out.push_str("<strong>");
    }
    if inline.style.italic {
        out.push_str("<em>");
    }
    render_text(out, &inline.text);
    if inline.style.italic {
        out.push_str("</em>");
    }
    if inline.style.bold {
        out.push_str("</strong>");
    }
    if let Some(close) = link_close {
        render_link_close(out, close);
    }
}

/// Escapes text, turning an internal `\n` (used for in-paragraph breaks such
/// as an address split over lines) into `<br>`.
fn render_text(out: &mut String, text: &str) {
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push_str("<br>");
        }
        esc_into(out, line);
        first = false;
    }
}

/// What [`render_link_close`] needs to close the tag [`render_link_open`]
/// opened: a plain `</a>`, a `</span>` (a dangling article link, marked
/// missing but with no href to give it), none at all (inert text, an
/// unlisted external scheme), or `</a>` plus the external-link domain
/// marker (which needs the sanitized domain text).
enum LinkClose {
    None,
    Anchor,
    Span,
    External(String),
}

/// Writes the opening tag for `link` (or nothing, for inert text) straight
/// into `out`, returning what [`render_link_close`] should write once the
/// run's styled, escaped text is in place.
fn render_link_open(out: &mut String, link: &Link, paths: &dyn Fn(u32) -> Option<String>) -> LinkClose {
    match link {
        Link::Article { entry, fragment } => match paths(*entry) {
            Some(path) => {
                out.push_str("<a class=\"link article\" href=\"");
                push_wiki_href(out, &path, fragment.as_deref());
                out.push_str("\">");
                LinkClose::Anchor
            }
            // `paths` has no path to offer (a stale or dangling entry index):
            // marked the same as a known-missing link rather than silently
            // dropped to unstyled plain text — just not a real anchor, since
            // there is no href to give it.
            None => {
                out.push_str("<span class=\"link missing\">");
                LinkClose::Span
            }
        },
        Link::Missing { path } => {
            out.push_str("<a class=\"link missing\" href=\"");
            push_wiki_href(out, path, None);
            out.push_str("\">");
            LinkClose::Anchor
        }
        Link::Anchor { fragment } => {
            out.push_str("<a href=\"#");
            push_encoded(out, fragment);
            out.push_str("\">");
            LinkClose::Anchor
        }
        Link::External { url } => {
            if is_allowed_scheme(url) {
                let url = crate::text::sanitize(url);
                out.push_str("<a class=\"link external\" href=\"");
                esc_into(out, &url);
                out.push_str("\" rel=\"noreferrer\">");
                LinkClose::External(crate::text::sanitize(&domain_of(&url)))
            } else {
                LinkClose::None
            }
        }
    }
}

fn render_link_close(out: &mut String, close: LinkClose) {
    match close {
        LinkClose::None => {}
        LinkClose::Anchor => out.push_str("</a>"),
        LinkClose::Span => out.push_str("</span>"),
        LinkClose::External(domain) => {
            out.push_str("</a><span class=\"external\"> ↗ ");
            esc_into(out, &domain);
            out.push_str("</span>");
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

/// Percent-encodes `path` straight into `out` via the encoder's own
/// `Display` impl, rather than collecting it into a temporary `String` first.
fn push_encoded(out: &mut String, path: &str) {
    let _ = write!(out, "{}", utf8_percent_encode(path, PATH_SAFE));
}

fn push_wiki_href(out: &mut String, path: &str, fragment: Option<&str>) {
    out.push_str("/wiki/");
    push_encoded(out, path);
    if let Some(fragment) = fragment {
        out.push('#');
        push_encoded(out, fragment);
    }
}

/// A percent-encoded `/wiki/{path}[#fragment]` href, the canonical URL for
/// an article. Shared by the article renderer above and `ok serve`'s route
/// handlers, so there is exactly one place that knows how a path becomes a URL.
pub fn wiki_href(path: &str, fragment: Option<&str>) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    push_wiki_href(&mut out, path, fragment);
    out
}

/// Sanitizes (strips control characters other than `\n`, and bidi
/// overrides) and HTML-escapes `s` in one pass, appending straight into
/// `out` — no intermediate `String` for either step. Every piece of
/// ZIM-sourced or user-supplied text this crate or a frontend writes into
/// HTML goes through this (or [`escape_html`], for text a caller has
/// already sanitized itself) rather than a one-off escaper.
pub fn esc_into(out: &mut String, s: &str) {
    for c in s.chars() {
        if !keep_char(c) {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

/// Escapes `&`, `<`, `>`, `"` and `'` — no sanitizing. For text a caller
/// already ran through [`crate::text::sanitize`] itself (`ok serve`'s page
/// shell); [`esc_into`] does both in one pass for everything in this module.
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
