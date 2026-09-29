mod bench;
mod mcp;
mod serve;
mod tui;

use std::fmt::Write as _;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use ok_core::import::{ImportOptions, Progress, import, index_dir_for};
use ok_core::{Collections, Library, Resolution, Target, text};

/// Offline knowledge: fast search and reading for ZIM files.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// A ZIM file, or a directory of them. Repeatable. Defaults to $OK_ZIM.
    #[arg(long, global = true, env = "OK_ZIM")]
    zim: Vec<PathBuf>,
    /// The collection to use, by label. Defaults to the first one loaded.
    #[arg(long, global = true, env = "OK_COLLECTION")]
    collection: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Build the indexes for every ZIM file given (once per file).
    Import {
        /// Memory for the full-text indexer, in MB.
        #[arg(long, default_value_t = 512)]
        heap_mb: usize,
        /// Rebuild an index that already exists.
        #[arg(long)]
        force: bool,
    },
    /// List the collections, and any files that could not be loaded.
    Collections,
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
    /// Run the MCP server over stdio.
    Mcp,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = zim_paths(&cli.zim)?;
    let wanted = cli.collection.as_deref();
    match cli.command.unwrap_or(Command::Tui) {
        Command::Import { heap_mb, force } => run_imports(&paths, heap_mb, force, &mut std::io::stdout().lock()),
        Command::Collections => {
            print!("{}", collections_report(&loaded(&paths, wanted)?)?);
            Ok(())
        }
        Command::Tui => tui::run(set(&paths, wanted)?),
        Command::Suggest { query, limit } => {
            let library = active(&paths, wanted)?;
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
            let library = active(&paths, wanted)?;
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
            let library = active(&paths, wanted)?;
            let asked = title.join(" ");
            let target = match library.resolve_title(&asked)? {
                Resolution::Found(t) => t,
                Resolution::NotFound { suggestions, .. } => match suggestions.into_iter().next() {
                    Some(s) => {
                        eprintln!("no exact match for \"{asked}\"; showing the closest title instead: \"{}\"", s.title);
                        Target { entry: s.article, fragment: s.fragment }
                    }
                    None => bail!("no article matches \"{asked}\""),
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
            let collections = set(&paths, wanted)?;
            let load = started.elapsed();
            let open = {
                let started = Instant::now();
                let default = collections.default();
                default.library().with_context(|| format!("opening {}", default.zim_path().display()))?;
                started.elapsed()
            };
            bench::run(collections, load, open, samples, json, http)
        }
        Command::Serve { bind } => serve::run(set(&paths, wanted)?, bind),
        Command::Mcp => mcp::run(active(&paths, wanted)?),
    }
}

/// Expands `--zim`: a file stands for itself, a directory for the `*.zim`
/// files directly inside it, sorted by filename so the load order — and so
/// which collection is the default — does not depend on the filesystem. An
/// `.okx` index directory, a subdirectory (`data/devdocs`) and anything
/// that is not a `.zim` file are passed over without a word; a file that
/// was named but cannot be loaded belongs to [`Collections::open`], which
/// says so.
fn zim_paths(given: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if given.is_empty() {
        bail!("no ZIM file given: pass --zim <file> or set OK_ZIM");
    }
    let mut paths = Vec::new();
    for path in given {
        if !path.is_dir() {
            paths.push(path.clone());
            continue;
        }
        let entries = std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?;
        let mut found: Vec<PathBuf> = entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let is_zim = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zim"));
                (is_zim && path.is_file()).then_some(path)
            })
            .collect();
        found.sort();
        paths.extend(found);
    }
    Ok(paths)
}

/// The loaded set, with every file that could not be loaded named on
/// stderr. Unconditional: a file dropped into the directory that goes
/// nowhere must not go nowhere silently.
fn loaded(paths: &[PathBuf], collection: Option<&str>) -> Result<Collections> {
    let collections = Collections::open(paths, collection)?;
    for skipped in collections.skipped() {
        eprintln!("skipped {}: {}", show_path(&skipped.path), skipped.reason);
    }
    Ok(collections)
}

/// The loaded set behind an `Arc`: what a frontend that holds all of them
/// at once needs.
fn set(paths: &[PathBuf], collection: Option<&str>) -> Result<Arc<Collections>> {
    Ok(Arc::new(loaded(paths, collection)?))
}

/// The active collection's library, and nothing else opened: opening is
/// what costs.
fn active(paths: &[PathBuf], collection: Option<&str>) -> Result<Arc<Library>> {
    let collections = loaded(paths, collection)?;
    let active = collections.default();
    active.library().with_context(|| format!("opening {}", active.zim_path().display()))
}

/// `ok collections`: the one command that opens every collection on
/// purpose, because opening is the only thing that separates "imported and
/// still matching its ZIM" from "imported once, ZIM replaced since".
fn collections_report(collections: &Collections) -> Result<String> {
    let mut out = String::new();
    for (i, collection) in collections.iter().enumerate() {
        let label = collection.label()?;
        let default = if i == collections.default_index() { " (default)" } else { "" };
        let state = match collection.library() {
            Ok(library) => format!("{} articles", library.article_count()),
            Err(_) => format!("failed: {}", collection.failure().unwrap_or("could not be opened")),
        };
        writeln!(out, "{label}{default}  {}  {state}", text::sanitize_line(collection.title()))?;
    }
    for skipped in collections.skipped() {
        writeln!(out, "skipped {}: {}", show_path(&skipped.path), skipped.reason)?;
    }
    Ok(out)
}

/// A path on its way to a terminal line. Paths come from the command line
/// rather than from a ZIM, but they end up on the same lines as text that
/// does not.
fn show_path(path: &Path) -> String {
    text::sanitize_line(&path.display().to_string())
}

/// Imports every file given. One file's refusal is reported and the rest
/// still import — a directory of ZIMs is the normal case — and the run
/// exits non-zero when any of them failed.
fn run_imports(paths: &[PathBuf], heap_mb: usize, force: bool, out: &mut dyn Write) -> Result<()> {
    let mut failed = 0;
    for path in paths {
        if !force && index_dir_for(path).join("meta.json").exists() {
            writeln!(out, "{} is already imported; pass --force to rebuild it", show_path(path))?;
            continue;
        }
        if let Err(e) = run_import(path, heap_mb, out) {
            eprintln!("{}: {e:#}", show_path(path));
            failed += 1;
        }
    }
    ensure!(failed == 0, "{failed} of {} files could not be imported", paths.len());
    Ok(())
}

fn run_import(zim: &Path, heap_mb: usize, out: &mut dyn Write) -> Result<()> {
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
    writeln!(out, "Imported \"{}\" into {}", m.title, report.index_dir.display())?;
    writeln!(
        out,
        "  {} articles, {} redirects, {} section redirects, {} title keys",
        m.articles, m.redirects, m.section_redirects, m.title_keys
    )?;
    writeln!(out, "  {} internal links ({} to articles not in this file)", m.internal_links, m.missing_links)?;
    writeln!(
        out,
        "  scan {:.1}s, parse and index {:.1}s, finish {:.1}s, total {:.1}s",
        report.scan.as_secs_f64(),
        report.parse_and_index.as_secs_f64(),
        report.finish.as_secs_f64(),
        started.elapsed().as_secs_f64()
    )?;
    writeln!(out, "  index size {:.1} MB", report.index_bytes as f64 / 1_048_576.0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ok_zim::write::ZimBuilder;

    use super::*;

    fn zim(dir: &Path, file: &str, name: &str, scraper: &str, articles: &[&str]) -> PathBuf {
        let mut builder = ZimBuilder::new();
        for article in articles {
            let html = format!(
                r#"<html><body><h1>{article}</h1><div id="mw-content-text"><div class="mw-parser-output"><p>Some prose about it.</p></div></div></body></html>"#
            );
            builder = builder.article(&article.replace(' ', "_"), article, &html);
        }
        let bytes = builder.metadata("Title", "Tiny").metadata("Name", name).metadata("Scraper", scraper).build();
        let path = dir.join(file);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// `--zim` repeats and takes each path whole: no value delimiter,
    /// because a path may contain a comma.
    #[test]
    fn zim_repeats_and_never_splits_a_path_on_a_comma() {
        let cli = Cli::try_parse_from(["ok", "--zim", "a,b.zim", "--zim", "c.zim", "suggest", "x"]).unwrap();
        assert_eq!(cli.zim, [PathBuf::from("a,b.zim"), PathBuf::from("c.zim")]);

        let cli = Cli::try_parse_from(["ok", "--zim", "d.zim", "--collection", "wiktionary"]).unwrap();
        assert_eq!(cli.collection.as_deref(), Some("wiktionary"));
    }

    /// A directory stands for the `*.zim` files directly inside it, in
    /// filename order, and everything else there — an `.okx` index, a
    /// subdirectory, a stray file — is passed over.
    #[test]
    fn zim_paths_expands_a_directory_in_filename_order_and_ignores_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        zim(dir.path(), "b.zim", "wiktionary_en_all", "mwoffliner 1.17.5", &["Mercury"]);
        zim(dir.path(), "a.zim", "wikipedia_en_top", "mwoffliner 1.17.5", &["Albert Einstein"]);
        std::fs::create_dir(dir.path().join("a.okx")).unwrap();
        std::fs::create_dir(dir.path().join("devdocs")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();

        let expanded = zim_paths(&[dir.path().to_path_buf()]).unwrap();
        let names: Vec<&str> = expanded.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["a.zim", "b.zim"], "sorted, so the default collection does not depend on the filesystem");

        let given = zim_paths(&[dir.path().join("b.zim"), dir.path().join("a.zim")]).unwrap();
        assert!(given[0].ends_with("b.zim") && given[1].ends_with("a.zim"), "two --zim flags keep the order given");

        assert!(zim_paths(&[]).is_err(), "no --zim at all is an error, not an empty set");
    }

    /// `ok --zim <dir> import` covers every file, reports the one it
    /// refuses while still importing the rest, and exits non-zero. A second
    /// run leaves the indexes alone unless asked to rebuild them.
    #[test]
    fn import_covers_every_file_skips_what_is_built_and_survives_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let good = zim(dir.path(), "a.zim", "wikipedia_en_top", "mwoffliner 1.17.5", &["Albert Einstein"]);
        let other = zim(dir.path(), "b.zim", "wiktionary_en_all", "mwoffliner 1.17.5", &["Mercury"]);
        let refused = zim(dir.path(), "c.zim", "stack_en_all", "sotoki 1.3", &["Question"]);
        let paths = zim_paths(&[dir.path().to_path_buf()]).unwrap();

        let err = run_imports(&paths, 20, false, &mut Vec::new()).err().unwrap().to_string();
        assert!(err.contains("1 of 3"), "the run exits non-zero saying how many failed: {err}");
        assert!(index_dir_for(&good).exists(), "a refusal must not stop the files around it");
        assert!(index_dir_for(&other).exists());
        assert!(!index_dir_for(&refused).exists());

        let built = std::fs::metadata(index_dir_for(&good).join("meta.json")).unwrap().modified().unwrap();
        let mut out = Vec::new();
        let _ = run_imports(&paths, 20, false, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("already imported") && text.contains("--force"), "a second run says why it did nothing: {text}");
        let after = std::fs::metadata(index_dir_for(&good).join("meta.json")).unwrap().modified().unwrap();
        assert_eq!(built, after, "an existing index is left alone");

        let mut out = Vec::new();
        let _ = run_imports(&paths, 20, true, &mut out);
        assert!(String::from_utf8(out).unwrap().contains("Imported"), "--force rebuilds");
    }

    /// `ok collections` opens every collection on purpose: that is what
    /// separates "still matching its ZIM" from "imported once, replaced
    /// since", and it names the files it could not load at all.
    #[test]
    fn collections_report_names_the_default_a_failed_collection_and_a_skipped_file() {
        let dir = tempfile::tempdir().unwrap();
        let broken = zim(dir.path(), "a.zim", "wikipedia_en_top", "mwoffliner 1.17.5", &["Albert Einstein"]);
        run_imports(std::slice::from_ref(&broken), 20, false, &mut Vec::new()).unwrap();
        // A different file at the same path: the index no longer describes it.
        zim(dir.path(), "a.zim", "wikipedia_en_top", "mwoffliner 1.17.5", &["Marie Curie", "Ulm", "Bern"]);
        zim(dir.path(), "b.zim", "wiktionary_en_all", "mwoffliner 1.17.5", &["Mercury"]);
        let paths = zim_paths(&[dir.path().to_path_buf()]).unwrap();

        let report = collections_report(&Collections::open(&paths, None).unwrap()).unwrap();
        assert!(report.contains("wikipedia (default)"), "{report}");
        assert!(report.contains("failed:"), "an index that no longer matches its ZIM is named failed: {report}");
        let skipped = report.lines().find(|l| l.starts_with("skipped ")).unwrap_or_default();
        assert!(skipped.contains("b.zim"), "{report}");
        assert!(skipped.contains("import"), "the reason names the command that fixes it: {report}");
    }
}
