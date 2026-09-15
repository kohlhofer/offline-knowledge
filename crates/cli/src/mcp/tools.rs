//! Pure formatting: `&Library`/`&Document` in, plain text out. No rmcp
//! machinery here, so these are unit-testable without a running server.
//! `Ok` is the success text, `Err` is the `isError` text — `mcp/mod.rs`
//! wraps whichever came back into a `CallToolResult`.

use std::collections::HashSet;

use ok_core::document::{Block, Document, Link, inline_text, truncate_words};
use ok_core::text::sanitize;
use ok_core::{Library, Resolution, Suggestion};

const SEARCH_SUMMARY_CHARS: usize = 140;
const MAX_SECTION_CHARS: usize = 6000;
const MAX_FACTS: usize = 12;
const MAX_SUGGESTIONS: usize = 5;

/// Title matches (via `Library::suggest`) first, then full-text matches,
/// deduplicated by article and capped at `limit`. Title matches never parse
/// a full article — that would cost as much as `limit` article loads for a
/// plainer line, since the title itself is already the point of the hit.
pub fn search_text(library: &Library, query: &str, limit: usize) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("search needs a non-empty query".to_string());
    }
    let title_hits = library.suggest(query, limit).map_err(|e| e.to_string())?;
    let mut seen: HashSet<u32> = title_hits.iter().map(|s| s.article).collect();
    let mut lines: Vec<String> = title_hits
        .iter()
        .map(|s| match &s.matched {
            Some(alias) => format!("{} — title match via \"{}\"", sanitize(&s.title), sanitize(alias)),
            None => sanitize(&s.title),
        })
        .collect();

    let text_hits = library.search(query, limit).map_err(|e| e.to_string())?;
    for hit in text_hits {
        if seen.insert(hit.article) {
            let summary = truncate_words(&sanitize(&hit.summary), SEARCH_SUMMARY_CHARS);
            lines.push(format!("{} — {}", sanitize(&hit.title), summary));
        }
    }
    lines.truncate(limit);

    let header = format!("{} shown for \"{}\"", lines.len(), sanitize(query));
    Ok(std::iter::once(header).chain(lines).collect::<Vec<_>>().join("\n"))
}

/// Without `section`: header, the lead's paragraph text, facts compressed to
/// `label: value` lines, then an outline. With `section`: that section's
/// text including its subsections, capped at [`MAX_SECTION_CHARS`].
pub fn read_text(library: &Library, article: &str, section: Option<&str>, offset: Option<usize>) -> Result<String, String> {
    let target = resolve_article(library, article)?;
    let doc = library.article(target.entry).map_err(|e| e.to_string())?;
    // A section-redirect title with no explicit `section` opens that section directly.
    match section.or(target.fragment.as_deref()) {
        None => Ok(read_overview(&doc)),
        Some(spec) => match resolve_section(&doc, spec) {
            Some(index) => Ok(read_section(&doc, index, offset.unwrap_or(0))),
            None => Err(unresolvable_section_message(&doc, spec)),
        },
    }
}

/// Deduplicated by target entry, first-seen order, titles only. The trailing
/// line counts unique missing and external targets, not occurrences, so a
/// repeated nav/infobox link to the same missing target doesn't inflate it.
pub fn links_text(library: &Library, article: &str, section: Option<&str>) -> Result<String, String> {
    let target = resolve_article(library, article)?;
    let doc = library.article(target.entry).map_err(|e| e.to_string())?;
    let links: Vec<Link> = match section.or(target.fragment.as_deref()) {
        None => doc.links().cloned().collect(),
        Some(spec) => match resolve_section(&doc, spec) {
            Some(index) => doc.section_links(index).cloned().collect(),
            None => return Err(unresolvable_section_message(&doc, spec)),
        },
    };

    let mut seen_articles: HashSet<u32> = HashSet::new();
    let mut titles: Vec<String> = Vec::new();
    let mut missing: HashSet<String> = HashSet::new();
    let mut external: HashSet<String> = HashSet::new();
    for link in links {
        match link {
            Link::Article { entry, .. } => {
                if seen_articles.insert(entry) {
                    titles.push(sanitize(&library.title(entry).map_err(|e| e.to_string())?));
                }
            }
            Link::Missing { path } => {
                missing.insert(path);
            }
            Link::External { url } => {
                external.insert(url);
            }
            Link::Anchor { .. } => {}
        }
    }

    let header = format!("{} unique articles linked from \"{}\"", titles.len(), sanitize(&doc.title));
    let mut out = std::iter::once(header).chain(titles).collect::<Vec<_>>().join("\n");
    let mut counts = Vec::new();
    if !missing.is_empty() {
        counts.push(format!("{} not in this collection", missing.len()));
    }
    if !external.is_empty() {
        counts.push(format!("{} external", external.len()));
    }
    if !counts.is_empty() {
        out.push_str("\n+");
        out.push_str(&counts.join(", "));
    }
    Ok(out)
}

fn resolve_article(library: &Library, article: &str) -> Result<ok_core::Target, String> {
    match library.resolve_title(article).map_err(|e| e.to_string())? {
        Resolution::Found(target) => Ok(target),
        Resolution::NotFound { suggestions } => Err(not_found_message(article, &suggestions)),
    }
}

fn not_found_message(article: &str, suggestions: &[Suggestion]) -> String {
    let article = sanitize(article);
    if suggestions.is_empty() {
        return format!("no article titled \"{article}\" — call search");
    }
    let names: Vec<String> = suggestions.iter().take(MAX_SUGGESTIONS).map(|s| format!("\"{}\"", sanitize(&s.title))).collect();
    format!("no article titled \"{article}\" — try: {}, or call search", names.join(", "))
}

/// A section by outline index ("3") or exact heading text ("Early life"),
/// mirroring the TUI's `Laid::section_line` dual check: an exact anchor-id
/// match, then a case-insensitive heading match with underscore/space
/// normalization.
pub fn resolve_section(doc: &Document, spec: &str) -> Option<usize> {
    if let Ok(index) = spec.parse::<usize>()
        && index < doc.sections.len()
    {
        return Some(index);
    }
    let wanted = spec.replace('_', " ");
    doc.sections.iter().position(|s| s.anchor.as_deref() == Some(spec) || s.heading.eq_ignore_ascii_case(&wanted))
}

fn unresolvable_section_message(doc: &Document, spec: &str) -> String {
    format!(
        "no section \"{}\" in \"{}\" ({} sections):\n{}",
        sanitize(spec),
        sanitize(&doc.title),
        doc.sections.len(),
        outline_lines(doc).join("\n")
    )
}

fn read_overview(doc: &Document) -> String {
    let lead = &doc.sections[0];
    let paragraphs: Vec<String> = lead
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { content } => Some(sanitize(&inline_text(content))),
            _ => None,
        })
        .collect();

    let mut out = format!(
        "{} · {} chars · {} sections\n\n{}",
        sanitize(&doc.title),
        doc.plain_text().chars().count(),
        doc.sections.len(),
        paragraphs.join("\n\n")
    );

    let facts: Vec<(&str, String)> = lead
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Facts { facts } => Some(facts),
            _ => None,
        })
        .flatten()
        .map(|f| (f.label.as_str(), sanitize(&inline_text(&f.value))))
        .collect();
    if !facts.is_empty() {
        out.push_str("\n\n");
        let lines: Vec<String> = facts
            .iter()
            .take(MAX_FACTS)
            .map(|(label, value)| if label.is_empty() { value.clone() } else { format!("{}: {value}", sanitize(label)) })
            .collect();
        out.push_str(&lines.join("\n"));
        if facts.len() > MAX_FACTS {
            out.push_str(&format!("\n+{} more", facts.len() - MAX_FACTS));
        }
    }

    out.push_str("\n\nSections:\n");
    out.push_str(&outline_lines(doc).join("\n"));
    out
}

fn outline_lines(doc: &Document) -> Vec<String> {
    let totals = section_totals(doc);
    doc.sections
        .iter()
        .zip(totals)
        .enumerate()
        .map(|(i, (s, total))| {
            let indent = "  ".repeat(s.level.saturating_sub(2) as usize);
            format!("{i}  {indent}{} ({total} chars)", sanitize(&s.heading))
        })
        .collect()
}

/// Each section's char count including its nested subsections — the same
/// quantity `Document::section_text(i).chars().count()` would give, computed
/// for every section in one O(sections) pass via a prefix sum instead of
/// O(sections²) repeated concatenation.
fn section_totals(doc: &Document) -> Vec<usize> {
    let n = doc.sections.len();
    let own: Vec<usize> = doc.sections.iter().map(|s| s.plain_text().chars().count()).collect();
    let mut prefix = vec![0usize; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + own[i];
    }
    // end[i]: one past the last section nested under i (level > sections[i].level).
    let mut end = vec![n; n];
    let mut open: Vec<usize> = Vec::new();
    for (i, section) in doc.sections.iter().enumerate() {
        while let Some(&top) = open.last() {
            if doc.sections[top].level < section.level {
                break;
            }
            end[open.pop().expect("just peeked")] = i;
        }
        open.push(i);
    }
    (0..n).map(|i| prefix[end[i]] - prefix[i]).collect()
}

/// That section's text (including subsections), capped at
/// [`MAX_SECTION_CHARS]` chars from `offset`, sliced on a char boundary.
fn read_section(doc: &Document, index: usize, offset: usize) -> String {
    let heading = sanitize(&doc.sections[index].heading);
    let full: Vec<char> = sanitize(&doc.section_text(index)).chars().collect();
    let total = full.len();
    let start = offset.min(total);
    let end = (start + MAX_SECTION_CHARS).min(total);
    let body: String = full[start..end].iter().collect();

    let mut out = format!("{heading} ({total} chars)\n\n{body}");
    if end < total {
        out.push_str(&format!("\n…[truncated: call read with offset={end}]"));
    }
    out
}

