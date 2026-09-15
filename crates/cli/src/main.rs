mod bench;
mod serve;
mod tui;

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ok_core::import::{ImportOptions, Progress, import};
use ok_core::{Library, Resolution, Target};

/// Offline knowledge: fast search and reading for ZIM files.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The ZIM file to use. Defaults to $OK_ZIM.
    #[arg(long, global = true, env = "OK_ZIM")]
    zim: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Build the indexes for a ZIM file (once per file).
    Import {
        /// Memory for the full-text indexer, in MB.
        #[arg(long, default_value_t = 512)]
        heap_mb: usize,
    },
    /// Open the terminal reader (the default).
    Tui,
    /// Print title suggestions for a prefix.
    Suggest {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Print full-text search results.
    Search {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Print an article by title or path.
    Show {
        title: Vec<String>,
        /// The structured document as JSON instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Time suggestions, article loads, link follows and searches.
    Bench {
        #[arg(long, default_value_t = 300)]
        samples: usize,
        #[arg(long)]
        json: bool,
        /// Also bench `serve`'s HTTP routes on an ephemeral loopback port.
        #[arg(long)]
        http: bool,
    },
    /// Serve the web UI.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let zim = cli.zim.context("no ZIM file given: pass --zim <file> or set OK_ZIM")?;
    match cli.command.unwrap_or(Command::Tui) {
        Command::Import { heap_mb } => run_import(&zim, heap_mb),
        Command::Tui => tui::run(open(&zim)?),
        Command::Suggest { query, limit } => {
            let library = open(&zim)?;
            let started = Instant::now();
            let hits = library.suggest(&query.join(" "), limit)?;
            let took = started.elapsed();
            for s in hits {
                let via = s.matched.map(|m| format!("  (from \"{m}\")")).unwrap_or_default();
                let section = s.fragment.map(|f| format!(" #{f}")).unwrap_or_default();
                println!("{:>6}  {}{section}{via}", s.inbound, s.title);
            }
            eprintln!("{:.2} ms", took.as_secs_f64() * 1000.0);
            Ok(())
        }
        Command::Search { query, limit } => {
            let library = open(&zim)?;
            let started = Instant::now();
            let hits = library.search(&query.join(" "), limit)?;
            let took = started.elapsed();
            for r in hits {
                println!("{:>7.2}  {}\n         {}", r.score, r.title, r.summary);
            }
            eprintln!("{:.2} ms", took.as_secs_f64() * 1000.0);
            Ok(())
        }
        Command::Show { title, json } => {
            let library = open(&zim)?;
            let wanted = title.join(" ");
            let target = match library.resolve_title(&wanted)? {
                Resolution::Found(t) => t,
                Resolution::NotFound { suggestions } => match suggestions.into_iter().next() {
                    Some(s) => {
                        eprintln!("no exact match for \"{wanted}\"; showing the closest title instead: \"{}\"", s.title);
                        Target { entry: s.article, fragment: s.fragment }
                    }
                    None => bail!("no article matches \"{wanted}\""),
                },
            };
            let started = Instant::now();
            let doc = library.article(target.entry)?;
            let took = started.elapsed();
            let mut out = std::io::stdout().lock();
            if json {
                serde_json::to_writer_pretty(&mut out, &doc)?;
                writeln!(out)?;
            } else {
                write!(out, "{}", doc.plain_text())?;
            }
            eprintln!("loaded and parsed in {:.2} ms", took.as_secs_f64() * 1000.0);
            Ok(())
        }
        Command::Bench { samples, json, http } => {
            let started = Instant::now();
            let library = std::sync::Arc::new(open(&zim)?);
            bench::run(library, started.elapsed(), samples, json, http)
        }
        Command::Serve { bind } => serve::run(open(&zim)?, bind),
    }
}

fn open(zim: &Path) -> Result<Library> {
    Library::open(zim).with_context(|| format!("opening {}", zim.display()))
}

fn run_import(zim: &Path, heap_mb: usize) -> Result<()> {
    let started = Instant::now();
    let report = import(zim, &ImportOptions { heap_bytes: heap_mb * 1024 * 1024 }, &|p| {
        let line = match &p {
            Progress::Scanned { candidates, redirects } => {
                format!("scanned: {candidates} HTML entries, {redirects} redirects")
            }
            Progress::Classified { articles, section_redirects } => {
                format!("classified: {articles} articles, {section_redirects} section redirects")
            }
            Progress::Parsed { done, total } => format!("parsed {done}/{total}"),
            Progress::Writing(what) => format!("writing {what}"),
        };
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[2K[{:>6.1}s] {line}", started.elapsed().as_secs_f64());
        if !matches!(p, Progress::Parsed { .. }) {
            let _ = writeln!(err);
        }
    })?;
    eprintln!();
    let m = &report.meta;
    println!("Imported \"{}\" into {}", m.title, report.index_dir.display());
    println!(
        "  {} articles, {} redirects, {} section redirects, {} title keys",
        m.articles, m.redirects, m.section_redirects, m.title_keys
    );
    println!("  {} internal links ({} to articles not in this file)", m.internal_links, m.missing_links);
    println!(
        "  scan {:.1}s, parse and index {:.1}s, finish {:.1}s, total {:.1}s",
        report.scan.as_secs_f64(),
        report.parse_and_index.as_secs_f64(),
        report.finish.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    println!("  index size {:.1} MB", report.index_bytes as f64 / 1_048_576.0);
    Ok(())
}
