//! Lays a document out into terminal lines at a given width.
//!
//! Kept free of ratatui types so it can be tested and timed on its own; the
//! renderer maps span kinds to styles.

use ok_core::document::{Block, Document, Inline, Link, Style};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq)]
pub struct Laid {
    pub lines: Vec<Line>,
    /// One entry per link run, in reading order; spans refer to these by index.
    pub links: Vec<LinkSpot>,
    pub sections: Vec<SectionSpot>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text(Style),
    Link { index: usize, style: Style },
    Title,
    Heading(u8),
    Marker,
    Label,
    Note(Style),
    Code,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkSpot {
    pub line: usize,
    pub link: Link,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SectionSpot {
    pub line: usize,
    pub level: u8,
    pub heading: String,
    pub anchor: Option<String>,
}

impl Line {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

impl Laid {
    /// The line a section anchor (or a heading written with underscores) starts on.
    pub fn section_line(&self, fragment: &str) -> Option<usize> {
        let wanted = fragment.replace('_', " ");
        self.sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some(fragment) || s.heading.eq_ignore_ascii_case(&wanted))
            .map(|s| s.line)
    }

    /// The innermost section containing `line`.
    pub fn section_at(&self, line: usize) -> Option<&SectionSpot> {
        self.sections.iter().take_while(|s| s.line <= line).last()
    }
}

const TEXT: Kind = Kind::Text(Style { bold: false, italic: false });

pub fn layout(doc: &Document, width: u16) -> Laid {
    let mut w = Writer {
        width: usize::from(width.max(20)),
        laid: Laid { lines: Vec::new(), links: Vec::new(), sections: Vec::new() },
    };
    for (i, section) in doc.sections.iter().enumerate() {
        if i > 0 {
            w.blank();
        }
        w.laid.sections.push(SectionSpot {
            line: w.laid.lines.len(),
            level: section.level,
            heading: section.heading.clone(),
            anchor: section.anchor.clone(),
        });
        let kind = if section.level <= 1 { Kind::Title } else { Kind::Heading(section.level) };
        w.wrap(&[Piece::fixed(&section.heading, kind)], 0, 0);
        if section.level <= 2 {
            let rule = if section.level <= 1 { "═" } else { "─" };
            let len = display_width(&section.heading).min(w.width);
            w.laid.lines.push(Line { spans: vec![Span { text: rule.repeat(len), kind: Kind::Marker }] });
        }
        for block in &section.blocks {
            w.blank();
            w.block(block);
        }
    }
    w.laid
}

struct Writer {
    width: usize,
    laid: Laid,
}

#[derive(Clone, Copy)]
struct Piece<'a> {
    text: &'a str,
    kind: Kind,
    link: Option<&'a Link>,
}

impl<'a> Piece<'a> {
    fn fixed(text: &'a str, kind: Kind) -> Self {
        Piece { text, kind, link: None }
    }
}

fn inline_pieces<'a>(content: &'a [Inline], base: Option<Kind>, out: &mut Vec<Piece<'a>>) {
    for inline in content {
        let kind = match base {
            Some(Kind::Note(style)) => Kind::Note(merge(style, inline.style)),
            Some(Kind::Text(style)) => Kind::Text(merge(style, inline.style)),
            Some(other) => other,
            None => Kind::Text(inline.style),
        };
        out.push(Piece { text: &inline.text, kind, link: inline.link.as_ref() });
    }
}

impl Writer {
    fn blank(&mut self) {
        if self.laid.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.laid.lines.push(Line::default());
        }
    }

    fn block(&mut self, block: &Block) {
        let mut pieces = Vec::new();
        match block {
            Block::Paragraph { content } => {
                inline_pieces(content, None, &mut pieces);
                self.wrap(&pieces, 0, 0);
            }
            Block::Quote { content } => {
                inline_pieces(content, Some(Kind::Note(Style::default())), &mut pieces);
                self.wrap(&pieces, 4, 4);
            }
            Block::Note { content } => {
                inline_pieces(content, Some(Kind::Note(Style { bold: false, italic: true })), &mut pieces);
                self.wrap(&pieces, 2, 2);
            }
            Block::List { ordered, items } => {
                let mut number = 0;
                for item in items {
                    let indent = 2 * usize::from(item.depth.min(8));
                    let marker = if *ordered && item.depth == 0 {
                        number += 1;
                        format!("{number}. ")
                    } else {
                        ["• ", "◦ ", "▪ "][usize::from(item.depth) % 3].to_string()
                    };
                    let mut pieces = vec![Piece::fixed(&marker, Kind::Marker)];
                    inline_pieces(&item.content, None, &mut pieces);
                    self.wrap(&pieces, indent, indent + display_width(&marker));
                }
            }
            Block::Facts { facts } => {
                for fact in facts {
                    if fact.value.is_empty() {
                        self.wrap(&[Piece::fixed(&fact.label, Kind::Heading(4))], 0, 0);
                        continue;
                    }
                    let label = if fact.label.is_empty() { String::new() } else { format!("{}: ", fact.label) };
                    let mut pieces = vec![Piece::fixed(&label, Kind::Label)];
                    inline_pieces(&fact.value, None, &mut pieces);
                    self.wrap(&pieces, 0, 2);
                }
            }
            Block::Table { rows } => {
                for row in rows {
                    let mut pieces = Vec::new();
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            pieces.push(Piece::fixed(" │ ", Kind::Marker));
                        }
                        let base = cell.header.then_some(Kind::Text(Style { bold: true, italic: false }));
                        inline_pieces(&cell.content, base, &mut pieces);
                    }
                    self.wrap(&pieces, 0, 2);
                }
            }
            Block::Code { text } => {
                for line in text.lines() {
                    let clipped = clip(&sanitize(line), self.width);
                    self.laid.lines.push(Line { spans: vec![Span { text: clipped, kind: Kind::Code }] });
                }
            }
        }
    }

    /// Greedy word wrap. Whitespace collapses to single spaces, `\n` breaks the
    /// line, and words wider than the line are split.
    fn wrap(&mut self, pieces: &[Piece<'_>], first_indent: usize, rest_indent: usize) {
        let mut line = LineBuilder::new(first_indent);
        let mut pending_space = false;
        for piece in pieces {
            let kind = match piece.link {
                Some(link) => {
                    let index = self.laid.links.len();
                    self.laid.links.push(LinkSpot { line: usize::MAX, link: link.clone(), text: piece.text.to_string() });
                    let style = match piece.kind {
                        Kind::Text(s) | Kind::Note(s) => s,
                        _ => Style::default(),
                    };
                    Kind::Link { index, style }
                }
                None => piece.kind,
            };
            let text = sanitize(piece.text);
            let mut word = String::new();
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                let boundary = c.is_whitespace();
                if !boundary {
                    word.push(c);
                }
                if boundary || chars.peek().is_none() {
                    if !word.is_empty() {
                        self.place_word(&mut line, &word, kind, pending_space, rest_indent);
                        word.clear();
                        pending_space = false;
                    }
                    if c == '\n' {
                        self.finish(&mut line, rest_indent, true);
                        pending_space = false;
                    } else if boundary {
                        pending_space = line.has_content;
                    }
                }
            }
        }
        if line.has_content {
            self.laid.lines.push(Line { spans: line.spans });
        }
    }

    fn place_word(&mut self, line: &mut LineBuilder, word: &str, kind: Kind, space: bool, rest_indent: usize) {
        let width = display_width(word);
        let space = space && line.has_content;
        if line.has_content && line.col + usize::from(space) + width > self.width {
            self.finish(line, rest_indent, false);
        } else if space {
            let space_kind = match line.spans.last() {
                Some(last) if last.kind == kind => kind,
                _ => TEXT,
            };
            line.push(" ", space_kind, 1);
        }
        if let Kind::Link { index, .. } = kind {
            let spot = &mut self.laid.links[index];
            if spot.line == usize::MAX {
                spot.line = self.laid.lines.len();
            }
        }
        let mut rest = word;
        loop {
            let room = self.width.saturating_sub(line.col).max(1);
            if display_width(rest) <= room {
                line.push(rest, kind, display_width(rest));
                return;
            }
            let (head, tail) = split_at_width(rest, room);
            line.push(head, kind, display_width(head));
            self.finish(line, rest_indent, false);
            rest = tail;
        }
    }

    fn finish(&mut self, line: &mut LineBuilder, rest_indent: usize, keep_empty: bool) {
        if line.has_content || keep_empty {
            let done = std::mem::replace(line, LineBuilder::new(rest_indent));
            self.laid.lines.push(Line { spans: done.spans });
        }
    }
}

struct LineBuilder {
    spans: Vec<Span>,
    col: usize,
    has_content: bool,
}

impl LineBuilder {
    fn new(indent: usize) -> Self {
        let spans = if indent > 0 { vec![Span { text: " ".repeat(indent), kind: TEXT }] } else { Vec::new() };
        LineBuilder { spans, col: indent, has_content: false }
    }

    fn push(&mut self, text: &str, kind: Kind, width: usize) {
        match self.spans.last_mut() {
            Some(last) if last.kind == kind && self.has_content => last.text.push_str(text),
            _ => self.spans.push(Span { text: text.to_string(), kind }),
        }
        self.col += width;
        self.has_content = true;
    }
}

fn merge(a: Style, b: Style) -> Style {
    Style { bold: a.bold || b.bold, italic: a.italic || b.italic }
}

/// Moved to `ok_core::text` so the HTML renderer and MCP's plain-text output
/// share the same control-character stripping the TUI has always done.
pub use ok_core::text::sanitize;

pub fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > width && i > 0 {
            return text.split_at(i);
        }
        used += w;
    }
    (text, "")
}

fn clip(text: &str, width: usize) -> String {
    split_at_width(text, width).0.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ok_core::document::{Fact, ListItem, Section};

    fn text(s: &str) -> Inline {
        Inline { text: s.into(), style: Style::default(), link: None }
    }

    fn link(s: &str, entry: u32) -> Inline {
        Inline { text: s.into(), style: Style::default(), link: Some(Link::Article { entry, fragment: None }) }
    }

    fn doc(sections: Vec<Section>) -> Document {
        Document { entry: 1, path: "P".into(), title: "Title".into(), sections }
    }

    fn section(level: u8, heading: &str, blocks: Vec<Block>) -> Section {
        Section { level, heading: heading.into(), anchor: Some(heading.replace(' ', "_")), blocks }
    }

    fn texts(laid: &Laid) -> Vec<String> {
        laid.lines.iter().map(Line::text).collect()
    }

    #[test]
    fn wraps_words_and_keeps_links_whole_across_lines() {
        let d = doc(vec![section(
            1,
            "Title",
            vec![Block::Paragraph {
                content: vec![text("one two three four five "), link("the linked words", 7), text(" six seven")],
            }],
        )]);
        let laid = layout(&d, 20);
        assert_eq!(texts(&laid), ["Title", "═════", "", "one two three four", "five the linked", "words six seven"]);
        assert!(laid.lines.iter().all(|l| display_width(&l.text()) <= 20));
        assert_eq!(laid.links.len(), 1);
        assert_eq!(laid.links[0].line, 4);
        let link_spans: Vec<&str> = laid
            .lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| matches!(s.kind, Kind::Link { index: 0, .. }))
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(link_spans, ["the linked", "words"], "the space before the link stays outside it");
    }

    #[test]
    fn sections_lists_facts_and_long_words() {
        let d = doc(vec![
            section(1, "Title", vec![Block::Facts { facts: vec![Fact { label: "Born".into(), value: vec![text("1879")] }] }]),
            section(
                2,
                "Life and career",
                vec![Block::List {
                    ordered: true,
                    items: vec![
                        ListItem { depth: 0, content: vec![text("first item that wraps onto the next line")] },
                        ListItem { depth: 1, content: vec![text("nested")] },
                    ],
                }],
            ),
            section(3, "Deep", vec![Block::Paragraph { content: vec![text("Supercalifragilisticexpialidocious!")] }]),
        ]);
        let laid = layout(&d, 20);
        let lines = texts(&laid);
        assert!(lines.contains(&"Born: 1879".to_string()));
        let first = lines.iter().position(|l| l.starts_with("1. first")).unwrap();
        assert!(lines[first + 1].starts_with("   "), "continuation aligns after the marker: {lines:?}");
        assert!(lines.contains(&"  ◦ nested".to_string()));
        assert_eq!(laid.section_line("Life_and_career"), Some(laid.sections[1].line));
        assert_eq!(laid.section_at(laid.sections[2].line + 1).unwrap().heading, "Deep");
        assert!(lines.iter().all(|l| display_width(l) <= 20), "{lines:?}");
        assert!(lines.iter().any(|l| l == "Supercalifragilistic"));
    }

    #[test]
    fn strips_terminal_control_sequences() {
        let d = doc(vec![section(1, "T\u{1b}]52;c;evil\u{7}", vec![Block::Paragraph { content: vec![text("a\u{1b}[2Jb")] }])]);
        let laid = layout(&d, 40);
        assert!(texts(&laid).iter().all(|l| !l.chars().any(|c| c.is_control())));
        assert_eq!(laid.lines[3].text(), "a[2Jb");
    }

    #[test]
    fn line_breaks_inside_paragraphs() {
        let d = doc(vec![section(1, "T", vec![Block::Paragraph { content: vec![text("14 March 1879\nUlm")] }])]);
        assert_eq!(texts(&layout(&d, 40))[3..], ["14 March 1879", "Ulm"]);
    }
}
