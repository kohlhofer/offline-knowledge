//! `ok bench`: the latency budget, measured against a real file.

use std::time::{Duration, Instant};

use anyhow::Result;
use ok_core::Library;
use ok_core::document::Link;
use serde_json::json;

use crate::tui::layout::layout;

const SUGGEST_LIMIT: usize = 10;
const SEARCH_LIMIT: usize = 20;
const LAYOUT_WIDTH: u16 = 100;

/// Words that appear in a large share of articles, to time the slow end of search.
const COMMON_QUERIES: &[&str] =
    &["history", "united states", "war", "music", "world", "city", "science", "film", "government", "language"];

pub fn run(library: &Library, open: Duration, samples: usize, as_json: bool) -> Result<()> {
    let mut rng = SplitMix(0x5eed);
    let pick = |rng: &mut SplitMix| library.random_article(rng.next()).expect("library has articles");

    let mut suggest = Vec::new();
    let mut load = Vec::new();
    let mut render = Vec::new();
    let mut follow = Vec::new();
    let mut search_rare = Vec::new();
    let mut search_common = Vec::new();
    let mut largest = (0usize, String::new());

    for i in 0..samples {
        let entry = pick(&mut rng);
        let title = library.title(entry)?;

        let chars: Vec<char> = title.chars().collect();
        let prefix: String = chars.iter().take(1 + i % 6).collect();
        let (hits, t) = time(|| library.suggest(&prefix, SUGGEST_LIMIT));
        hits?;
        suggest.push(t);

        let (doc, t) = time(|| library.article(entry));
        let doc = doc?;
        load.push(t);
        let (lines, t) = time(|| layout(&doc, LAYOUT_WIDTH));
        render.push(t);
        if lines.lines.len() > largest.0 {
            largest = (lines.lines.len(), doc.title.clone());
        }

        let links: Vec<u32> = doc
            .links()
            .filter_map(|l| match l {
                Link::Article { entry, .. } => Some(*entry),
                _ => None,
            })
            .collect();
        if !links.is_empty() {
            let target = links[(rng.next() % links.len() as u64) as usize];
            let (res, t) = time(|| library.article(target).map(|d| layout(&d, LAYOUT_WIDTH)));
            res?;
            follow.push(t);
        }

        if let Some(word) = title.split_whitespace().find(|w| w.chars().count() >= 5 && w.chars().all(char::is_alphabetic)) {
            let (hits, t) = time(|| library.search(word, SEARCH_LIMIT));
            hits?;
            search_rare.push(t);
        }
        if i < COMMON_QUERIES.len() * 5 {
            let q = COMMON_QUERIES[i % COMMON_QUERIES.len()];
            let (hits, t) = time(|| library.search(q, SEARCH_LIMIT));
            hits?;
            search_common.push(t);
        }
    }

    let rows = [
        ("open library", vec![open]),
        ("suggest (1-6 chars)", suggest),
        ("load + parse article", load),
        ("layout at 100 cols", render),
        ("follow link (load+parse+layout)", follow),
        ("search, title word", search_rare),
        ("search, common word", search_common),
    ];
    if as_json {
        let out: Vec<_> = rows
            .iter()
            .map(|(name, d)| {
                let s = Stats::of(d);
                json!({"op": name, "n": s.n, "p50_ms": ms(s.p50), "p90_ms": ms(s.p90), "p99_ms": ms(s.p99), "max_ms": ms(s.max)})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("{:<34} {:>5} {:>9} {:>9} {:>9} {:>9}", "operation", "n", "p50 ms", "p90 ms", "p99 ms", "max ms");
        for (name, d) in &rows {
            let s = Stats::of(d);
            println!(
                "{:<34} {:>5} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                name,
                s.n,
                ms(s.p50),
                ms(s.p90),
                ms(s.p99),
                ms(s.max)
            );
        }
        println!("longest article sampled: {} ({} lines at {} columns)", largest.1, largest.0, LAYOUT_WIDTH);
    }
    Ok(())
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

struct Stats {
    n: usize,
    p50: Duration,
    p90: Duration,
    p99: Duration,
    max: Duration,
}

impl Stats {
    fn of(samples: &[Duration]) -> Stats {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let at = |p: f64| {
            if sorted.is_empty() {
                return Duration::ZERO;
            }
            let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
            sorted[rank - 1]
        };
        Stats { n: sorted.len(), p50: at(0.50), p90: at(0.90), p99: at(0.99), max: sorted.last().copied().unwrap_or_default() }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let d: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let s = Stats::of(&d);
        assert_eq!((s.n, s.p50, s.p90, s.p99, s.max), (100, Duration::from_millis(50), Duration::from_millis(90), Duration::from_millis(99), Duration::from_millis(100)));
        assert_eq!(Stats::of(&[]).p99, Duration::ZERO);
        assert_eq!(Stats::of(&[Duration::from_millis(7)]).p50, Duration::from_millis(7));
    }
}
