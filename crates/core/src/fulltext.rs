//! Full-text search with Tantivy, built once at import.

use std::path::Path;
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, SegmentReader, doc};

use crate::Result;

const TOKENIZER: &str = "en_stem";

#[derive(Clone, Copy)]
struct Fields {
    entry: Field,
    title: Field,
    alt_titles: Field,
    body: Field,
    summary: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let with_positions = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer(TOKENIZER).set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    // Body terms without positions keep the index small; phrase queries are
    // turned into plain conjunctions before parsing.
    let freqs_only = TextOptions::default()
        .set_indexing_options(TextFieldIndexing::default().set_tokenizer(TOKENIZER).set_index_option(IndexRecordOption::WithFreqs));
    let fields = Fields {
        entry: b.add_u64_field("entry", INDEXED | STORED | FAST),
        title: b.add_text_field("title", with_positions.clone()),
        alt_titles: b.add_text_field("alt_titles", with_positions),
        body: b.add_text_field("body", freqs_only),
        summary: b.add_text_field("summary", STORED),
    };
    (b.build(), fields)
}

pub struct FullTextWriter {
    writer: IndexWriter,
    fields: Fields,
}

impl FullTextWriter {
    pub fn create(dir: &Path, heap_bytes: usize) -> Result<FullTextWriter> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = schema();
        let index = Index::create_in_dir(dir, schema)?;
        let writer = index.writer(heap_bytes)?;
        Ok(FullTextWriter { writer, fields })
    }

    /// Thread-safe: call from many threads at once.
    pub fn add(&self, entry: u32, title: &str, alt_titles: &str, body: &str, summary: &str) -> Result<()> {
        let f = self.fields;
        self.writer.add_document(doc!(
            f.entry => u64::from(entry),
            f.title => title,
            f.alt_titles => alt_titles,
            f.body => body,
            f.summary => summary,
        ))?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub entry: u32,
    pub score: f32,
    pub summary: String,
}

pub struct FullText {
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
    inbound: Arc<[u32]>,
}

impl FullText {
    pub fn open(dir: &Path, inbound: Arc<[u32]>) -> Result<FullText> {
        let index = Index::open_in_dir(dir)?;
        let (_, fields) = schema();
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let mut parser = QueryParser::for_index(&index, vec![fields.title, fields.alt_titles, fields.body]);
        parser.set_field_boost(fields.title, 4.0);
        parser.set_field_boost(fields.alt_titles, 3.0);
        parser.set_conjunction_by_default();
        Ok(FullText { reader, parser, fields, inbound })
    }

    /// Articles matching every word of `query`, or any word when none match
    /// them all. Relevance is BM25, nudged up by inbound links.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let cleaned = clean_query(query);
        if cleaned.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let hits = self.run(&cleaned, limit)?;
        if !hits.is_empty() || !cleaned.contains(' ') {
            return Ok(hits);
        }
        self.run(&cleaned.split(' ').collect::<Vec<_>>().join(" OR "), limit)
    }

    fn run(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let (parsed, _errors) = self.parser.parse_query_lenient(query);
        let inbound = Arc::clone(&self.inbound);
        let collector = TopDocs::with_limit(limit).tweak_score(move |segment: &SegmentReader| {
            let entries = segment.fast_fields().u64("entry").expect("entry is a fast field").first_or_default_col(0);
            let inbound = Arc::clone(&inbound);
            move |doc, score| {
                let entry = entries.get_val(doc) as usize;
                let links = inbound.get(entry).copied().unwrap_or(0) as f32;
                score * (1.0 + 0.15 * links.ln_1p())
            }
        });
        let searcher = self.reader.searcher();
        let top: Vec<(f32, DocAddress)> = searcher.search(&parsed, &collector)?;
        top.into_iter()
            .map(|(score, address)| {
                let doc: TantivyDocument = searcher.doc(address)?;
                let entry = doc.get_first(self.fields.entry).and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as u32;
                let summary = doc.get_first(self.fields.summary).and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(SearchHit { entry, score, summary })
            })
            .collect()
    }
}

/// Drops query syntax a reader would not mean: quotes, field prefixes,
/// boosts and boolean operators all become plain words.
fn clean_query(query: &str) -> String {
    query
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT") && !w.chars().all(|c| c == '-' || c == '\''))
        .map(|w| w.trim_matches(|c| c == '-' || c == '\''))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_cleaning_removes_syntax() {
        assert_eq!(clean_query(r#""general relativity" title:foo^2 AND -x"#), "general relativity title foo 2 x");
        assert_eq!(clean_query("  "), "");
        assert_eq!(clean_query("Newton's laws"), "Newton's laws");
    }

    #[test]
    fn indexes_and_ranks_with_inbound_links() {
        let dir = tempfile::tempdir().unwrap();
        let writer = FullTextWriter::create(dir.path(), 15_000_000).unwrap();
        writer.add(1, "Albert Einstein", "Einstein", "physicist who developed relativity", "Physicist.").unwrap();
        writer.add(2, "Relativity (band)", "", "a band named after relativity", "Band.").unwrap();
        writer.add(3, "Theory of relativity", "", "general and special relativity theories", "Theory.").unwrap();
        writer.add(4, "Isaac Newton", "", "laws of motion and gravity", "Newton.").unwrap();
        writer.finish().unwrap();

        let inbound: Arc<[u32]> = vec![0, 5, 0, 500, 50].into();
        let ft = FullText::open(dir.path(), inbound).unwrap();

        let hits = ft.search("relativity", 10).unwrap();
        let entries: Vec<u32> = hits.iter().map(|h| h.entry).collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], 3, "title match plus inbound links ranks first: {hits:?}");
        assert_eq!(hits[0].summary, "Theory.");

        assert_eq!(ft.search("einstein", 5).unwrap()[0].entry, 1, "redirect titles are searchable");
        assert_eq!(ft.search("\"special relativity\"", 5).unwrap()[0].entry, 3, "quotes do not break search");
        let fallback = ft.search("gravity relativity", 5).unwrap();
        assert!(fallback.len() >= 2, "no document has both words, so any word matches: {fallback:?}");
        assert!(ft.search("zzzz", 5).unwrap().is_empty());
    }
}
