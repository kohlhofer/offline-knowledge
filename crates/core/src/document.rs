//! The document model every interface renders, and the HTML parser that builds it.
//!
//! Articles come out of the core as sections of blocks of styled inline runs,
//! with links already resolved to entry indices. Terminal, graphical and agent
//! frontends all consume this structure; none of them sees HTML.

use ego_tree::NodeRef;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Document {
    pub entry: u32,
    pub path: String,
    pub title: String,
    /// `sections[0]` is the lead: level 1, headed by the article title.
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub level: u8,
    pub heading: String,
    pub anchor: Option<String>,
    pub blocks: Vec<Block>,
}

impl Section {
    /// This section alone as plain text: the heading, then every block.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.heading);
        out.push('\n');
        for block in &self.blocks {
            block_text(block, &mut out);
            out.push('\n');
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph { content: Vec<Inline> },
    List { ordered: bool, items: Vec<ListItem> },
    Quote { content: Vec<Inline> },
    /// A hatnote such as "See also: …".
    Note { content: Vec<Inline> },
    /// Infobox rows. A fact with an empty value is a heading inside the box.
    Facts { facts: Vec<Fact> },
    Table { rows: Vec<Vec<Cell>> },
    Code { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListItem {
    pub depth: u8,
    pub content: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    pub label: String,
    pub value: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cell {
    pub header: bool,
    pub content: Vec<Inline>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inline {
    pub text: String,
    pub style: Style,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Link {
    /// Another article in the same file.
    Article { entry: u32, fragment: Option<String> },
    /// An internal link whose target is not in this file (common in subsets).
    Missing { path: String },
    /// A link to a section of the current article.
    Anchor { fragment: String },
    External { url: String },
}

/// Where a path leads: an article, and a section when the path was a
/// section redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub entry: u32,
    pub fragment: Option<String>,
}

/// Maps a decoded article path (no namespace) to the article it leads to.
pub trait LinkResolver {
    fn resolve(&self, path: &str) -> Option<Target>;
}

impl<F: Fn(&str) -> Option<Target>> LinkResolver for F {
    fn resolve(&self, path: &str) -> Option<Target> {
        self(path)
    }
}

/// An `href` classified, with internal paths made absolute and percent-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Href {
    Anchor(String),
    External(String),
    Internal { path: String, fragment: Option<String> },
}

/// Classifies `href` as found in the page at `page_path`.
pub fn classify_href(page_path: &str, href: &str) -> Option<Href> {
    classify_href_in(&base_dir(page_path), href)
}

fn classify_href_in(base_dir: &[String], href: &str) -> Option<Href> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    if let Some(fragment) = href.strip_prefix('#') {
        return Some(Href::Anchor(decode(fragment)));
    }
    if href.starts_with("//") || href.contains("://") || has_uri_scheme(href) {
        return Some(Href::External(href.to_string()));
    }
    let (path_part, fragment) = match href.split_once('#') {
        Some((p, f)) => (p, Some(decode(f)).filter(|f| !f.is_empty())),
        None => (href, None),
    };
    let path_part = path_part.split_once('?').map_or(path_part, |(p, _)| p);
    Some(Href::Internal { path: join_relative(base_dir, &decode(path_part)), fragment })
}

/// Whether `href` opens with an RFC 3986 URI scheme (a letter, then
/// letters/digits/`+`/`-`/`.`, then `:`): `geo:52.5,13.4`, `tel:+1-555-0100`,
/// `urn:isbn:0-486-27557-4`, `mailto:` (already matched above, but harmless
/// to match again). These have no `//` authority, so the checks above miss
/// them, and without this they read as a relative article path — one click
/// from a "missing article" link offering to search the URI's own text.
fn has_uri_scheme(href: &str) -> bool {
    let Some(colon) = href.find(':') else { return false };
    let scheme = &href[..colon];
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

impl Document {
    /// All article entries this document links to, in order, with repeats.
    pub fn article_links(&self) -> impl Iterator<Item = u32> + '_ {
        self.links().filter_map(|l| match l {
            Link::Article { entry, .. } => Some(*entry),
            _ => None,
        })
    }

    /// Every link in reading order.
    pub fn links(&self) -> impl Iterator<Item = &Link> + '_ {
        self.inlines().filter_map(|i| i.link.as_ref())
    }

    fn inlines(&self) -> impl Iterator<Item = &Inline> + '_ {
        self.sections.iter().flat_map(|s| s.blocks.iter()).flat_map(block_inlines)
    }

    /// The article as plain text: headings and blocks separated by newlines.
    pub fn plain_text(&self) -> String {
        self.sections.iter().map(Section::plain_text).collect()
    }

    /// The first paragraph, cut at a word boundary near `max_chars`.
    pub fn summary(&self, max_chars: usize) -> String {
        let first = self.sections.iter().flat_map(|s| &s.blocks).find_map(|b| match b {
            Block::Paragraph { content } => {
                let text = inline_text(content);
                (!text.trim().is_empty()).then_some(text)
            }
            _ => None,
        });
        let text = first.unwrap_or_default();
        truncate_words(text.trim(), max_chars)
    }

    /// The section at `index` and every following section nested under it
    /// (level greater than its own), as a half-open range of indices. The
    /// same look-ahead rule `prune_empty_sections` uses.
    fn section_range(&self, index: usize) -> std::ops::Range<usize> {
        let level = self.sections[index].level;
        let end = self.sections[index + 1..].iter().take_while(|s| s.level > level).count();
        index..index + 1 + end
    }

    /// The section at `index`, plus every section nested under it, as text.
    ///
    /// Panics if `index >= self.sections.len()`: every caller in this
    /// codebase resolves `index` against `self.sections.len()` first (see
    /// `mcp::tools::resolve_section`); a caller outside it must do the same.
    pub fn section_text(&self, index: usize) -> String {
        self.sections[self.section_range(index)].iter().map(Section::plain_text).collect()
    }

    /// Every link in the section at `index` and its nested subsections, in
    /// reading order.
    ///
    /// Panics if `index >= self.sections.len()`; see [`Self::section_text`].
    pub fn section_links(&self, index: usize) -> impl Iterator<Item = &Link> + '_ {
        self.sections[self.section_range(index)]
            .iter()
            .flat_map(|s| s.blocks.iter())
            .flat_map(block_inlines)
            .filter_map(|i| i.link.as_ref())
    }
}

fn block_inlines(block: &Block) -> Box<dyn Iterator<Item = &Inline> + '_> {
    match block {
        Block::Paragraph { content } | Block::Quote { content } | Block::Note { content } => Box::new(content.iter()),
        Block::List { items, .. } => Box::new(items.iter().flat_map(|i| i.content.iter())),
        Block::Facts { facts } => Box::new(facts.iter().flat_map(|f| f.value.iter())),
        Block::Table { rows } => Box::new(rows.iter().flatten().flat_map(|c| c.content.iter())),
        Block::Code { .. } => Box::new(std::iter::empty()),
    }
}

pub fn inline_text(content: &[Inline]) -> String {
    content.iter().map(|i| i.text.as_str()).collect()
}

fn push_line(out: &mut String, content: &[Inline]) {
    for inline in content {
        out.push_str(&inline.text);
    }
    out.push('\n');
}

fn block_text(block: &Block, out: &mut String) {
    match block {
        Block::Paragraph { content } | Block::Quote { content } | Block::Note { content } => push_line(out, content),
        Block::List { items, .. } => items.iter().for_each(|i| push_line(out, &i.content)),
        Block::Facts { facts } => {
            for f in facts {
                out.push_str(&f.label);
                if !f.label.is_empty() && !f.value.is_empty() {
                    out.push_str(": ");
                }
                push_line(out, &f.value);
            }
        }
        Block::Table { rows } => {
            for row in rows {
                for cell in row {
                    out.push_str(&inline_text(&cell.content));
                    out.push_str(" | ");
                }
                out.push('\n');
            }
        }
        Block::Code { text } => {
            out.push_str(text);
            out.push('\n');
        }
    }
}

/// Cuts `text` at a word boundary near `max_chars`, appending `…` when it
/// truncated.
pub fn truncate_words(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    let at = cut.rfind(char::is_whitespace).unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end_matches([',', ';', ':', ' ']))
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const SKIP_TAGS: &[&str] = &[
    "style", "script", "noscript", "link", "meta", "img", "figure", "audio", "video", "source", "svg", "button",
    "input", "map", "area", "iframe", "object", "template",
];

const SKIP_CLASSES: &[&str] = &[
    "reference",
    "mw-references-wrap",
    "references",
    "reflist",
    "refbegin",
    "navbox",
    "navbox-styles",
    "vertical-navbox",
    "sidebar",
    "thumb",
    "gallery",
    "mw-editsection",
    "metadata",
    "noprint",
    "mw-empty-elt",
    "ambox",
    "ombox",
    "tmbox",
    "side-box",
    "sistersitebox",
    "portalbox",
    "catlinks",
    "zim-footer",
    "mw-cite-backlink",
    "printfooter",
    "shortdescription",
    "mw-jump-link",
    "toc",
    "cite-bracket",
    "infobox-image",
    "infobox-caption",
    "mw-default-size",
];

const INLINE_TAGS: &[&str] = &[
    "a", "b", "strong", "i", "em", "span", "sup", "sub", "small", "abbr", "cite", "q", "code", "bdi", "bdo", "s", "u",
    "time", "var", "kbd", "samp", "mark", "del", "ins", "font", "big", "tt", "math", "br", "wbr", "data", "dfn", "label",
];

/// Where relative links in the article resolve from.
pub struct ArticleContext<'a> {
    pub entry: u32,
    /// The article's path, without namespace.
    pub path: &'a str,
    /// Used when the page has no `<h1>`.
    pub title: &'a str,
}

pub fn parse_article(html: &str, ctx: &ArticleContext<'_>, resolver: &dyn LinkResolver) -> Document {
    let dom = Html::parse_document(html);
    let root = select_first(&dom, "div.mw-parser-output")
        .or_else(|| select_first(&dom, "#mw-content-text"))
        .or_else(|| select_first(&dom, "body"));
    let title = select_first(&dom, "h1")
        .map(|h| collapse_whitespace(&h.text().collect::<String>()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| ctx.title.to_string());

    let mut walker = Walker {
        base_dir: base_dir(ctx.path),
        resolver,
        sections: vec![Section { level: 1, heading: title.clone(), anchor: None, blocks: Vec::new() }],
        pending: Runs::default(),
    };
    if let Some(root) = root {
        walker.block_children(*root);
    }
    walker.flush_paragraph();
    let sections = prune_empty_sections(walker.sections);
    Document { entry: ctx.entry, path: ctx.path.to_string(), title, sections }
}

fn select_first<'a>(dom: &'a Html, css: &str) -> Option<ElementRef<'a>> {
    dom.select(&Selector::parse(css).expect("static selector")).next()
}

struct Walker<'r> {
    base_dir: Vec<String>,
    resolver: &'r dyn LinkResolver,
    sections: Vec<Section>,
    pending: Runs,
}

impl Walker<'_> {
    fn push_block(&mut self, block: Block) {
        self.sections.last_mut().expect("lead section exists").blocks.push(block);
    }

    fn flush_paragraph(&mut self) {
        let content = std::mem::take(&mut self.pending).finish();
        if !content.is_empty() {
            self.push_block(Block::Paragraph { content });
        }
    }

    fn block_children(&mut self, node: NodeRef<'_, Node>) {
        for child in node.children() {
            self.block(child);
        }
    }

    fn block(&mut self, node: NodeRef<'_, Node>) {
        let el = match node.value() {
            Node::Text(t) => {
                self.pending.push(t, Style::default(), None);
                return;
            }
            Node::Element(el) => el,
            _ => return,
        };
        if skipped(node) {
            return;
        }
        let name = el.name();
        match name {
            "h1" => {}
            "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
                let level = name.as_bytes()[1] - b'0';
                let heading = collapse_whitespace(&visible_text(node));
                self.sections.push(Section { level, heading, anchor: el.attr("id").map(str::to_string), blocks: Vec::new() });
            }
            "p" => {
                self.flush_paragraph();
                let content = self.inline_block(node);
                if !content.is_empty() {
                    self.push_block(Block::Paragraph { content });
                }
            }
            "ul" | "ol" => {
                self.flush_paragraph();
                let mut items = Vec::new();
                self.list(node, 0, &mut items);
                if !items.is_empty() {
                    self.push_block(Block::List { ordered: name == "ol", items });
                }
            }
            "dl" => {
                self.flush_paragraph();
                let mut items = Vec::new();
                self.definition_list(node, 0, &mut items);
                if !items.is_empty() {
                    self.push_block(Block::List { ordered: false, items });
                }
            }
            "blockquote" => {
                self.flush_paragraph();
                let content = self.inline_block(node);
                if !content.is_empty() {
                    self.push_block(Block::Quote { content });
                }
            }
            "pre" => {
                self.flush_paragraph();
                let text = visible_text(node);
                if !text.trim().is_empty() {
                    self.push_block(Block::Code { text: text.trim_end().to_string() });
                }
            }
            "table" => {
                self.flush_paragraph();
                if el.classes().any(|c| c == "infobox") {
                    let facts = self.infobox(node);
                    if !facts.is_empty() {
                        self.push_block(Block::Facts { facts });
                    }
                } else {
                    let rows = self.table(node);
                    if !rows.is_empty() {
                        self.push_block(Block::Table { rows });
                    }
                }
            }
            "div" if el.classes().any(|c| c == "hatnote") => {
                self.flush_paragraph();
                let content = self.inline_block(node);
                if !content.is_empty() {
                    self.push_block(Block::Note { content });
                }
            }
            _ if INLINE_TAGS.contains(&name) => {
                let mut runs = std::mem::take(&mut self.pending);
                self.inline(node, Style::default(), None, &mut runs);
                self.pending = runs;
            }
            _ => {
                self.flush_paragraph();
                self.block_children(node);
                self.flush_paragraph();
            }
        }
    }

    fn inline_block(&self, node: NodeRef<'_, Node>) -> Vec<Inline> {
        let mut runs = Runs::default();
        for child in node.children() {
            self.inline(child, Style::default(), None, &mut runs);
        }
        runs.finish()
    }

    fn inline(&self, node: NodeRef<'_, Node>, style: Style, link: Option<&Link>, runs: &mut Runs) {
        let el = match node.value() {
            Node::Text(t) => {
                runs.push(t, style, link);
                return;
            }
            Node::Element(el) => el,
            _ => return,
        };
        if skipped(node) {
            return;
        }
        let mut style = style;
        let mut own_link = None;
        match el.name() {
            "br" => {
                runs.push_break(style, link);
                return;
            }
            "math" => {
                if let Some(alt) = el.attr("alttext") {
                    runs.push(alt, style, link);
                }
                return;
            }
            "b" | "strong" => style.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" => style.italic = true,
            "a" => own_link = el.attr("href").and_then(|href| self.link(href)),
            // Block elements inside inline content (a list in a table cell, say)
            // become separated text rather than being dropped.
            "p" | "div" | "li" | "ul" | "ol" | "dl" | "dd" | "dt" | "table" | "tr" | "blockquote" => {
                runs.push_break(style, link);
            }
            _ => {}
        }
        let link = own_link.as_ref().or(link);
        for child in node.children() {
            self.inline(child, style, link, runs);
        }
    }

    fn list(&self, node: NodeRef<'_, Node>, depth: u8, items: &mut Vec<ListItem>) {
        for li in node.children().filter(|c| is_element(*c, "li") && !skipped(*c)) {
            let mut runs = Runs::default();
            let mut nested = Vec::new();
            for child in li.children() {
                match element_name(child) {
                    Some("ul" | "ol") if !skipped(child) => nested.push(child),
                    Some("dl") if !skipped(child) => nested.push(child),
                    _ => self.inline(child, Style::default(), None, &mut runs),
                }
            }
            let content = runs.finish();
            if !content.is_empty() {
                items.push(ListItem { depth, content });
            }
            for n in nested {
                if element_name(n) == Some("dl") {
                    self.definition_list(n, depth.saturating_add(1), items);
                } else {
                    self.list(n, depth.saturating_add(1), items);
                }
            }
        }
    }

    fn definition_list(&self, node: NodeRef<'_, Node>, depth: u8, items: &mut Vec<ListItem>) {
        for child in node.children().filter(|c| !skipped(*c)) {
            let (style, item_depth) = match element_name(child) {
                Some("dt") => (Style { bold: true, italic: false }, depth),
                Some("dd") => (Style::default(), depth.saturating_add(1)),
                _ => continue,
            };
            let mut runs = Runs::default();
            let mut nested = Vec::new();
            for grandchild in child.children() {
                match element_name(grandchild) {
                    Some("ul" | "ol" | "dl") if !skipped(grandchild) => nested.push(grandchild),
                    _ => self.inline(grandchild, style, None, &mut runs),
                }
            }
            let content = runs.finish();
            if !content.is_empty() {
                items.push(ListItem { depth: item_depth, content });
            }
            for n in nested {
                if element_name(n) == Some("dl") {
                    self.definition_list(n, item_depth.saturating_add(1), items);
                } else {
                    self.list(n, item_depth.saturating_add(1), items);
                }
            }
        }
    }

    fn infobox(&self, node: NodeRef<'_, Node>) -> Vec<Fact> {
        let mut facts = Vec::new();
        for row in descendants_named(node, "tr") {
            let cells: Vec<NodeRef<'_, Node>> =
                row.children().filter(|c| matches!(element_name(*c), Some("th" | "td")) && !skipped(*c)).collect();
            match cells.as_slice() {
                [label, value] if element_name(*label) == Some("th") => {
                    let label = collapse_whitespace(&visible_text(*label));
                    let value = self.inline_block(*value);
                    if !label.is_empty() && !value.is_empty() {
                        facts.push(Fact { label, value });
                    }
                }
                [only] => {
                    let has = |class: &str| {
                        only.value().as_element().is_some_and(|e| e.classes().any(|c| c == class))
                    };
                    if has("infobox-above") || has("infobox-title") {
                        continue;
                    }
                    if element_name(*only) == Some("th") {
                        let label = collapse_whitespace(&visible_text(*only));
                        if !label.is_empty() {
                            facts.push(Fact { label, value: Vec::new() });
                        }
                    } else {
                        let value = self.inline_block(*only);
                        if !value.is_empty() {
                            facts.push(Fact { label: String::new(), value });
                        }
                    }
                }
                _ => {}
            }
        }
        facts
    }

    fn table(&self, node: NodeRef<'_, Node>) -> Vec<Vec<Cell>> {
        let mut rows = Vec::new();
        for row in descendants_named(node, "tr") {
            let cells: Vec<Cell> = row
                .children()
                .filter(|c| !skipped(*c))
                .filter_map(|c| match element_name(c) {
                    Some(tag @ ("th" | "td")) => Some(Cell { header: tag == "th", content: self.inline_block(c) }),
                    _ => None,
                })
                .collect();
            if cells.iter().any(|c| !c.content.is_empty()) {
                rows.push(cells);
            }
        }
        rows
    }

    fn link(&self, href: &str) -> Option<Link> {
        Some(match classify_href_in(&self.base_dir, href)? {
            Href::Anchor(fragment) => Link::Anchor { fragment },
            Href::External(url) => Link::External { url },
            Href::Internal { path, fragment } => match self.resolver.resolve(&path) {
                // A fragment in the link wins over one from a section redirect.
                Some(target) => Link::Article { entry: target.entry, fragment: fragment.or(target.fragment) },
                None => Link::Missing { path },
            },
        })
    }
}

fn is_element(node: NodeRef<'_, Node>, name: &str) -> bool {
    element_name(node) == Some(name)
}

fn element_name<'a>(node: NodeRef<'a, Node>) -> Option<&'a str> {
    node.value().as_element().map(|e| e.name())
}

fn skipped(node: NodeRef<'_, Node>) -> bool {
    let Some(el) = node.value().as_element() else { return false };
    SKIP_TAGS.contains(&el.name())
        || el.classes().any(|c| SKIP_CLASSES.contains(&c))
        || el.attr("role") == Some("navigation")
        || el.attr("style").is_some_and(|s| s.replace(' ', "").contains("display:none"))
}

/// Text of a subtree, leaving out skipped elements.
fn visible_text(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

fn collect_text(node: NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(t),
        Node::Element(el) if !skipped(node) => {
            if el.name() == "br" {
                out.push('\n');
            }
            for child in node.children() {
                collect_text(child, out);
            }
        }
        _ => {}
    }
}

fn descendants_named<'a>(node: NodeRef<'a, Node>, name: &'a str) -> impl Iterator<Item = NodeRef<'a, Node>> + 'a {
    // Rows of this table only: do not descend into nested tables.
    let mut stack: Vec<NodeRef<'a, Node>> = node.children().collect::<Vec<_>>().into_iter().rev().collect();
    std::iter::from_fn(move || {
        while let Some(n) = stack.pop() {
            match element_name(n) {
                Some(tag) if tag == name => return Some(n),
                Some("table") => continue,
                Some(_) if !skipped(n) => stack.extend(n.children().collect::<Vec<_>>().into_iter().rev()),
                _ => {}
            }
        }
        None
    })
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn base_dir(path: &str) -> Vec<String> {
    let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
    parts.pop();
    parts
}

/// Resolves `href` against the directory of the current article, URL-style.
fn join_relative(base_dir: &[String], href: &str) -> String {
    let mut parts: Vec<&str> = if href.starts_with('/') { Vec::new() } else { base_dir.iter().map(String::as_str).collect() };
    for segment in href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Drops sections with no blocks and no non-empty subsection. The lead always stays.
fn prune_empty_sections(sections: Vec<Section>) -> Vec<Section> {
    let keep: Vec<bool> = (0..sections.len())
        .map(|i| {
            i == 0
                || !sections[i].blocks.is_empty()
                || sections[i + 1..]
                    .iter()
                    .take_while(|s| s.level > sections[i].level)
                    .any(|s| !s.blocks.is_empty())
        })
        .collect();
    sections.into_iter().zip(keep).filter_map(|(s, k)| k.then_some(s)).collect()
}

/// Accumulates inline runs with HTML whitespace collapsing.
#[derive(Default)]
struct Runs {
    runs: Vec<Inline>,
    pending_space: bool,
}

impl Runs {
    fn push(&mut self, text: &str, style: Style, link: Option<&Link>) {
        let mut word = String::new();
        for c in text.chars() {
            // Zero-width spaces, joiners and soft hyphens are hints for browsers;
            // in a terminal they only leave invisible characters behind.
            if matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{ad}') {
                continue;
            }
            if c.is_whitespace() && c != '\u{a0}' {
                if !word.is_empty() {
                    self.push_word(&std::mem::take(&mut word), style, link);
                }
                self.pending_space = true;
            } else {
                word.push(if c == '\u{a0}' { ' ' } else { c });
            }
        }
        if !word.is_empty() {
            self.push_word(&word, style, link);
        }
    }

    fn push_word(&mut self, word: &str, style: Style, link: Option<&Link>) {
        let need_space = self.pending_space && !self.runs.is_empty() && !self.ends_with_break();
        self.pending_space = false;
        if let Some(last) = self.runs.last_mut() {
            if last.style == style && last.link.as_ref() == link {
                if need_space {
                    last.text.push(' ');
                }
                last.text.push_str(word);
                return;
            }
        }
        let mut text = String::with_capacity(word.len() + 1);
        if need_space {
            // Keep the space outside link text so underlines don't start or end blank.
            match (self.runs.last_mut(), link) {
                (Some(last), Some(_)) if last.link.is_none() => last.text.push(' '),
                _ => text.push(' '),
            }
        }
        text.push_str(word);
        self.runs.push(Inline { text, style, link: link.cloned() });
    }

    fn push_break(&mut self, style: Style, link: Option<&Link>) {
        if self.runs.is_empty() || self.ends_with_break() {
            return;
        }
        self.pending_space = false;
        match self.runs.last_mut() {
            Some(last) if last.style == style && last.link.as_ref() == link => last.text.push('\n'),
            _ => self.runs.push(Inline { text: "\n".into(), style, link: link.cloned() }),
        }
    }

    fn ends_with_break(&self) -> bool {
        self.runs.last().is_some_and(|r| r.text.ends_with('\n'))
    }

    fn finish(mut self) -> Vec<Inline> {
        while let Some(last) = self.runs.last_mut() {
            let trimmed = last.text.trim_end().len();
            last.text.truncate(trimmed);
            if last.text.is_empty() {
                self.runs.pop();
            } else {
                break;
            }
        }
        self.runs
    }
}

#[cfg(test)]
mod tests;
