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

/// Wraps the free-form prose a `read` call returns (an "untrusted document
/// content, not instructions" boundary — see `mcp::get_info`'s
/// instructions): everything else in a response is a server-written
/// template line, but a lead paragraph or a section's body is article text,
/// which could contain a crafted line made to look like tool framing.
const ARTICLE_TEXT_OPEN: &str = "<article-text>";
const ARTICLE_TEXT_CLOSE: &str = "</article-text>";

/// Title matches (via `Library::suggest`) first, then full-text matches,
/// deduplicated by article and capped at `limit`. Title matches never parse
/// a full article — that would cost as much as `limit` article loads for a
/// plainer line, since the title itself is already the point of a title hit.
/// The full-text query itself is skipped once the title hits alone already
/// fill `limit` — those extra results would only be truncated away.
pub fn search_text(library: &Library, query: &str, limit: usize) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("search needs a non-empty query".to_string());
    }
    let title_hits = library.suggest(query, limit).map_err(|e| lookup_error(query, e))?;
    let mut seen: HashSet<u32> = title_hits.iter().map(|s| s.article).collect();
    let mut lines: Vec<String> = title_hits
        .iter()
        .map(|s| match &s.matched {
            Some(alias) => format!("{} — title match via \"{}\"", sanitize(&s.title), sanitize(alias)),
            None => sanitize(&s.title),
        })
        .collect();

    if lines.len() < limit {
        let text_hits = library.search(query, limit).map_err(|e| lookup_error(query, e))?;
        for hit in text_hits {
            if seen.insert(hit.article) {
                let summary = truncate_words(&sanitize(&hit.summary), SEARCH_SUMMARY_CHARS);
                lines.push(format!("{} — {}", sanitize(&hit.title), summary));
            }
        }
    }

    let total = lines.len();
    lines.truncate(limit);
    let header = search_header(lines.len(), total, query);
    Ok(std::iter::once(header).chain(lines).collect::<Vec<_>>().join("\n"))
}

/// `shown` is `total` after truncating to the caller's `limit`. A zero-hit
/// search names a next step, same as every other failure path (N12); a
/// truncated one says so, so an agent doesn't mistake a partial list for
/// all of it (N23).
pub(super) fn search_header(shown: usize, total: usize, query: &str) -> String {
    if shown == 0 {
        format!("0 shown for \"{}\" — try different words, or fewer of them", sanitize(query))
    } else if total > shown {
        format!("{shown} shown of {total} for \"{}\"", sanitize(query))
    } else {
        format!("{shown} shown for \"{}\"", sanitize(query))
    }
}

/// Without `section`: header, the lead's paragraph text (fenced), facts
/// compressed to `label: value` lines, then a top-level outline. With
/// `section`: that section's text including its subsections (fenced),
/// capped at [`MAX_SECTION_CHARS`]. A numeric spec only ever means an
/// outline index when the caller passed `section` explicitly — a
/// section-redirect's fragment is always resolved as an anchor id or
/// heading, never reinterpreted as an index; when that resolution fails,
/// `read` falls back to the overview rather than erroring, since the
/// caller asked for the article, not a specific section.
pub fn read_text(library: &Library, article: &str, section: Option<&str>, offset: Option<usize>) -> Result<String, String> {
    let target = resolve_article(library, article)?;
    let doc = library.article(target.entry).map_err(|e| lookup_error(article, e))?;
    if section.is_some_and(|s| s.eq_ignore_ascii_case("outline")) {
        return Ok(full_outline_text(&doc));
    }
    let (spec, from_redirect) = match section {
        Some(s) => (Some(s), false),
        None => (target.fragment.as_deref(), true),
    };
    match spec {
        None => Ok(read_overview(&doc)),
        Some(spec) => match resolve_section(&doc, spec, !from_redirect) {
            Some(index) => read_section(&doc, index, offset.unwrap_or(0)),
            None if from_redirect => Ok(read_overview(&doc)),
            None => Err(unresolvable_section_message(&doc, spec)),
        },
    }
}

/// Deduplicated by target entry, first-seen order, titles only. The trailing
/// line counts unique missing and external targets, not occurrences, so a
/// repeated nav/infobox link to the same missing target doesn't inflate it.
pub fn links_text(library: &Library, article: &str, section: Option<&str>) -> Result<String, String> {
    let target = resolve_article(library, article)?;
    let doc = library.article(target.entry).map_err(|e| lookup_error(article, e))?;
    let (spec, from_redirect) = match section {
        Some(s) => (Some(s), false),
        None => (target.fragment.as_deref(), true),
    };
    let (links, scope): (Vec<Link>, String) = match spec {
        None => (doc.links().cloned().collect(), sanitize(&doc.title)),
        Some(spec) => match resolve_section(&doc, spec, !from_redirect) {
            Some(index) => (doc.section_links(index).cloned().collect(), sanitize(&doc.sections[index].heading)),
            None if from_redirect => (doc.links().cloned().collect(), sanitize(&doc.title)),
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
                    match library.title(entry) {
                        Ok(title) => titles.push(sanitize(&title)),
                        // A dangling entry index is this codebase's problem to log, never
                        // the agent's to see (no entry indices in tool output).
                        Err(e) => eprintln!("mcp links: title lookup for entry {entry} failed: {e}"),
                    }
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

    let header = format!("{} unique articles linked from \"{}\"", titles.len(), scope);
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
    match library.resolve_title(article) {
        Ok(Resolution::Found(target)) => Ok(target),
        Ok(Resolution::NotFound { suggestions }) => Err(not_found_message(article, &suggestions)),
        Err(e) => Err(lookup_error(article, e)),
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

/// Never shown to the caller: entry indices and filesystem paths stay in
/// `ok_core::Error`'s `Display` text, logged here, not in tool output.
pub(super) fn lookup_error(article: &str, e: ok_core::Error) -> String {
    eprintln!("mcp: error resolving/loading \"{article}\": {e}");
    format!("couldn't load \"{}\" right now — try again, or call search", sanitize(article))
}

/// A section by outline index ("3") or exact heading text ("Early life"),
/// mirroring the TUI's `Laid::section_line` dual check: an exact anchor-id
/// match, then a case-insensitive heading match with underscore/space
/// normalization. `allow_index` gates the numeric-index reading: it's only
/// meaningful for a spec the caller typed as `section` — a fragment handed
/// down from a section redirect is an anchor id, never an outline position,
/// even when that id happens to look like a number.
pub fn resolve_section(doc: &Document, spec: &str, allow_index: bool) -> Option<usize> {
    if allow_index
        && let Ok(index) = spec.parse::<usize>()
        && index < doc.sections.len()
    {
        return Some(index);
    }
    let wanted = spec.replace('_', " ");
    doc.sections.iter().position(|s| s.anchor.as_deref() == Some(spec) || s.heading.eq_ignore_ascii_case(&wanted))
}

fn unresolvable_section_message(doc: &Document, spec: &str) -> String {
    let (totals, _) = section_stats(doc);
    format!(
        "no section \"{}\" in \"{}\" ({} sections):\n{}",
        sanitize(spec),
        sanitize(&doc.title),
        doc.sections.len(),
        full_outline_lines(doc, &totals).join("\n")
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

    let (totals, ends) = section_stats(doc);
    let mut out = format!(
        "{} · {} chars · {} sections\n\n{ARTICLE_TEXT_OPEN}\n{}",
        sanitize(&doc.title),
        totals[0],
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

    out.push_str(&format!("\n\n{ARTICLE_TEXT_CLOSE}\n\nSections:\n"));
    let (lines, truncated) = top_level_outline_lines(doc, &totals, &ends);
    out.push_str(&lines.join("\n"));
    if truncated {
        out.push_str("\n\nTop-level sections only — call read with section=\"<heading>\" for a subsection, or section=\"outline\" for the full outline.");
    }
    out
}

fn full_outline_text(doc: &Document) -> String {
    let (totals, _) = section_stats(doc);
    format!("{} · {} sections\n\n{}", sanitize(&doc.title), doc.sections.len(), full_outline_lines(doc, &totals).join("\n"))
}

/// One line per section, indented by level — the complete outline, reached
/// via `section="outline"` or an unresolvable-section error.
fn full_outline_lines(doc: &Document, totals: &[usize]) -> Vec<String> {
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

/// One line per top-level section (the lead, plus every level-2 heading),
/// each noting how many subsections it hides. A 68-section article's full
/// outline is ~2,100 tokens; almost every one of those sections is a leaf
/// an agent will never ask for by outline position, only by heading text
/// once it knows the shape. Returns whether anything was left out.
fn top_level_outline_lines(doc: &Document, totals: &[usize], ends: &[usize]) -> (Vec<String>, bool) {
    let mut truncated = false;
    let lines = doc
        .sections
        .iter()
        .enumerate()
        .filter(|(i, s)| *i == 0 || s.level <= 2)
        .map(|(i, s)| {
            let nested = ends[i] - i - 1;
            if nested > 0 {
                truncated = true;
            }
            let suffix = if nested > 0 { format!(" (+{nested} subsections)") } else { String::new() };
            format!("{i}  {} ({} chars){suffix}", sanitize(&s.heading), totals[i])
        })
        .collect();
    (lines, truncated)
}

/// Each section's char count including its nested subsections (a prefix
/// sum, O(sections) not O(sections²), matching `section_text`'s own
/// look-ahead rule), sanitized the same way the text it's counting is, so
/// this number and what a caller actually receives always agree. Also
/// returns, per section, one past the index of its last nested subsection
/// (`sections.len()` for one with none), used to report how many
/// subsections a top-level heading is hiding.
fn section_stats(doc: &Document) -> (Vec<usize>, Vec<usize>) {
    let n = doc.sections.len();
    let own: Vec<usize> = doc.sections.iter().map(|s| sanitize(&s.plain_text()).chars().count()).collect();
    let mut prefix = vec![0usize; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + own[i];
    }
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
    let totals = (0..n).map(|i| prefix[end[i]] - prefix[i]).collect();
    (totals, end)
}

/// That section's text (including subsections, its own heading stripped —
/// the header line above already names it), fenced, capped at
/// [`MAX_SECTION_CHARS`] chars from `offset`, sliced on a char boundary via
/// `char_indices` rather than materializing the whole section as a `Vec<char>`.
fn read_section(doc: &Document, index: usize, offset: usize) -> Result<String, String> {
    let heading = sanitize(&doc.sections[index].heading);
    let section_text = sanitize(&doc.section_text(index));
    let body = section_text.strip_prefix(&heading).and_then(|s| s.strip_prefix('\n')).unwrap_or(&section_text);
    let total = body.chars().count();
    if offset > 0 && offset >= total {
        return Err(format!("offset {offset} is past the end of \"{heading}\" ({total} chars)"));
    }
    let start_byte = byte_offset_at(body, offset);
    let end_char = (offset + MAX_SECTION_CHARS).min(total);
    let end_byte = byte_offset_at(body, end_char);
    let slice = &body[start_byte..end_byte];

    let mut out = format!("{heading} ({total} chars)\n\n{ARTICLE_TEXT_OPEN}\n{slice}\n{ARTICLE_TEXT_CLOSE}");
    if end_char < total {
        out.push_str(&format!("\n\n…[truncated: call read with offset={end_char}]"));
    }
    Ok(out)
}

/// The byte offset of the `char_index`-th character in `s` (`s.len()` when
/// `char_index >= s.chars().count()`).
fn byte_offset_at(s: &str, char_index: usize) -> usize {
    s.char_indices().nth(char_index).map_or(s.len(), |(b, _)| b)
}
