//! `ok bench`: the latency budget, measured against a real file.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
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

pub fn run(library: Arc<Library>, open: Duration, samples: usize, as_json: bool, http: bool) -> Result<()> {
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

    let mut rows = vec![
        ("open library", vec![open]),
        ("suggest (1-6 chars)", suggest),
        ("load + parse article", load),
        ("layout at 100 cols", render),
        ("follow link (load+parse+layout)", follow),
        ("search, title word", search_rare),
        ("search, common word", search_common),
    ];
    if http {
        rows.push(("GET /wiki/{path} (server)", http_bench(Arc::clone(&library), samples)?));
    }
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

/// Starts `serve`'s router on an ephemeral loopback port, times `samples`
/// real `GET /wiki/{path}` round trips against it, then shuts it down. The
/// only place `ok bench` needs a tokio runtime at all.
fn http_bench(library: Arc<Library>, samples: usize) -> Result<Vec<Duration>> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let app = crate::serve::router(Arc::clone(&library));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut rng = SplitMix(0xf00d_5eed);
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let entry = library.random_article(rng.next()).expect("library has articles");
            let path = library.path(entry)?;
            let uri = ok_core::html::wiki_href(&path, None);
            let started = Instant::now();
            let response = http_get(addr, &uri).await?;
            ensure!(
                response.status == 200,
                "GET {uri} returned status {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            );
            timings.push(started.elapsed());
        }
        server.abort();
        Ok(timings)
    })
}

pub(crate) struct HttpGetResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

/// A minimal HTTP/1.1 GET client: request line, `Host`, `Connection: close`
/// (so the server closes the connection when done, letting a plain
/// `read_to_end` stand in for real content-length/chunked parsing), then
/// everything up to the blank line is the head. Enough for our own tiny
/// server, not a general client.
pub(crate) async fn http_get(addr: SocketAddr, path: &str) -> Result<HttpGetResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    Ok(parse_http_response(&raw))
}

fn parse_http_response(raw: &[u8]) -> HttpGetResponse {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(raw.len());
    let status = std::str::from_utf8(&raw[..head_end])
        .ok()
        .and_then(|h| h.lines().next())
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    HttpGetResponse { status, body: raw[head_end..].to_vec() }
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

    /// Every sample the loop draws is the one article in this library, and
    /// it has no links: the follow-a-link sample must skip it, not index
    /// into an empty `Vec` (`run` guards this with `if !links.is_empty()`).
    #[test]
    fn follow_link_sampling_skips_an_article_with_no_links_without_panicking() {
        let bytes = ok_zim::write::ZimBuilder::new()
            .article(
                "Lonely",
                "Lonely",
                r#"<html><body><h1>Lonely</h1><div id="mw-content-text"><div class="mw-parser-output"><p>No links here.</p></div></div></body></html>"#,
            )
            .metadata("Title", "T")
            .build();
        let dir = tempfile::tempdir().unwrap();
        let zim = dir.path().join("t.zim");
        std::fs::write(&zim, bytes).unwrap();
        ok_core::import::import(&zim, &ok_core::import::ImportOptions { heap_bytes: 20_000_000 }, &|_| {}).unwrap();
        let library = Arc::new(ok_core::Library::open(&zim).unwrap());
        run(library, Duration::ZERO, 5, true, false).unwrap();
    }
}
