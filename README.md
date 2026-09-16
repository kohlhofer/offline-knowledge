# offline-knowledge

A fast, fully offline reader for Wikipedia and other Kiwix ZIM collections, built to run as a small appliance: one binary in an Apple `container` today, a dedicated machine later. You type, titles appear as you type, and following a link takes a few milliseconds.

The first collection is Kiwix's selection of the best 50,000 English Wikipedia articles (`wikipedia_en_top_nopic`, 2.1 GB). The terminal reader, a web UI (`ok serve`) and an MCP server for agents (`ok mcp`) all read the same imported index through `ok-core`'s `Library`.

## Quick Start

Rust comes from Homebrew's keg-only `rustup`, so it needs to be on your PATH.

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
mkdir -p data && cd data
curl -LO https://download.kiwix.org/zim/wikipedia/wikipedia_en_top_nopic_2026-06.zim
echo "b2806831e14690cbcafeb1b6e7bd4439fd59b3e5fbeaeb300a5792dece510ee0  wikipedia_en_top_nopic_2026-06.zim" | shasum -a 256 -c -
cd ..
cargo build --release -p ok
export OK_ZIM=data/wikipedia_en_top_nopic_2026-06.zim
./target/release/ok import     # once, about 90 seconds, writes data/*.okx (154 MB)
./target/release/ok            # the reader
```

`ok suggest <prefix>`, `ok search <words>` and `ok show <title> [--json]` print to stdout, and `ok bench` times everything below.

```sh
./target/release/ok serve --bind 127.0.0.1:8080   # web UI, http://127.0.0.1:8080
./target/release/ok mcp                            # MCP server over stdio, for agents
```

## In a Container

```sh
container build -c 8 -m 8g -t offline-knowledge:dev .
container network create --internal offline      # once: a host-only network with no route out
container run -it --rm --network offline -v "$PWD/data:/data" offline-knowledge:dev
container run -it --rm --network offline -p 127.0.0.1:8080:8080 -v "$PWD/data:/data" offline-knowledge:dev serve --bind 0.0.0.0:8080
```

The image is Debian 13 slim plus the `ok` binary. On the `offline` network the container has no route to the internet, and the reader, `ok bench` and `ok serve` all work there. `ok mcp` talks stdio, so it runs the same way through `container exec -i <container> ok mcp` rather than a published port. So far the import has only run on the Mac; it writes the index next to the ZIM in `data/`, which the container then reads through the mount.

## Keys

Terminal reader:

| Where | Keys | Does |
| --- | --- | --- |
| Search | type | suggest titles |
| Search | ↑ ↓ Enter | open |
| Search | Tab | search the full text |
| Reading | j k, Space, PgUp PgDn, g G | scroll |
| Reading | Tab, Shift-Tab | select a link |
| Reading | Enter | follow it |
| Reading | Backspace, ← → | back, forward |
| Reading | o | outline |
| Reading | / | search |
| Anywhere | Ctrl-R, ?, Ctrl-C | random article, help, quit |

Web UI (`ok serve`), same shape with browser-native equivalents where they exist:

| Keys | Does |
| --- | --- |
| type in the search box | live suggestions; Enter searches all text |
| ↑ ↓ Enter, Tab | move/open a suggestion; native Tab/Enter on any link |
| o | outline dialog, type to filter, Esc clears then closes |
| n / N | select the next/previous link at or below the viewport top |
| r | random article |
| / | focus the search box |
| ? | help dialog |
| Space, PgUp/PgDn, Home/End, browser Back/Forward | native scroll and history |

## Speed

`ok bench --samples 500 --http` on the M3 MacBook Air (16 GB, macOS 26.5), release build, warm page cache, 2026-09-15, after the round-2 speed pass (a fused sanitize+escape `to_html`, a rendered-article cache, `spawn_blocking` for `/api/suggest`):

| Operation | p50 | p99 | max |
| --- | --- | --- | --- |
| Open library | 9.7 ms | | |
| Title suggestions (1 to 6 characters) | 0.20 ms | 12 ms | 16 ms |
| Load and parse an article | 4.1 ms | 12 ms | 13 ms |
| Lay out at 100 columns | 0.32 ms | 1.0 ms | 1.3 ms |
| Follow a link (load, parse, layout) | 5.7 ms | 17 ms | 26 ms |
| Full-text search, a title word | 0.21 ms | 1.4 ms | 3.1 ms |
| Full-text search, a common word | 0.50 ms | 3.9 ms | 3.9 ms |
| `GET /wiki/{path}` over HTTP, cache miss (resolve + load + parse + render) | 4.2 ms | 12 ms | 17 ms |

`Document::to_html` is measurable — the first pass's claim that it wasn't held only because the per-run allocations it was doing were cheap relative to the html5ever parse ahead of it, not because there was nothing to measure. Before this pass's fix (fusing sanitize and HTML-escaping into one pass over the output buffer, and dropping the per-run temporary `String`s and per-heading/link `format!` calls — see `core/html.rs`): 3.08 ms on Demographics of the United States (2.17 MB of source HTML, the largest article in this collection) and 1.13 ms on Albert Einstein. After: 0.46 ms and 0.34 ms (medians over 30 runs), an 85% and 70% reduction, verified byte-identical against the pre-fix renderer over 498 real articles. A revisit to an already-rendered article now costs about 0.2 ms instead of a full parse and render, via the LRU cache added in the same pass; the `GET /wiki/{path}` row above is the cache-miss cost a first visit pays.

`ok mcp`'s tools cost the same `Library` calls with no HTTP parsing at all. `read` on Albert Einstein (68 sections) now returns 6,176 bytes (about 1,530 tokens, chars÷4) — down from 8,580 bytes before this pass trimmed the default outline to top-level sections only (each noting how many subsections it hides). `search "general relativity"` returns 1,172 bytes for 8 hits; `read` on one section of Albert Einstein ("Life and career", the largest) returns 6,115 bytes; `links` on that same section returns 2,250 bytes for 136 unique articles.

The dependency cost of `axum` + `tokio` + `rmcp` (v1 used none of these; see `Cargo.toml`): the release binary is 16 MB (10 MB before them; 14 MB before this pass's `hyper-util` and `tower`'s `limit` feature, added for the header-read timeout and concurrency cap). A clean `cargo build --release -p ok` that has to compile the dependency graph for the first time takes about 75 s wall clock (roughly 35 s of that is axum/tokio/hyper/tower; rmcp/schemars/uuid add the rest) versus 23 s for `ok-core`/`ok` alone once every third-party dependency is already built.

Inside Apple's `container` (1.3.1, default 4 CPUs and 1 GB, ZIM on a virtiofs bind mount, host cache warm), an earlier run (before `serve`/`mcp` existed) stayed within a few milliseconds of native: suggestions p99 20 ms, article loads p99 21 ms, link follows p99 29 ms, searches p99 3.5 ms for a title word and 11 ms for a common one, and 29 ms to open the library. Not re-measured for the HTTP/MCP paths yet.

The slow end of suggestions is one- and two-letter prefixes, which scan tens of thousands of titles. Precomputing those is the obvious next step if it ever shows in use.

## How It Works

`crates/zim` reads ZIM files without libzim. It memory-maps the file, parses directory entries in place, hands out uncompressed blobs as slices of the mapping, and decompresses zstd or xz clusters exactly as far as their offset tables say. Every size the file declares is checked and capped, because a ZIM file is input from the internet. A sampled comparison over 7,086 entries matches libzim byte for byte.

`crates/core` imports a file once. It finds the real articles, turns mwoffliner's meta-refresh pages (171,945 section redirects in this file) into a lookup table, parses every article, counts inbound links, and writes an FST of titles ranked by those counts plus a Tantivy full-text index. Articles reach every interface as a structured document of sections, paragraphs, lists, facts and resolved links, never as HTML — except `ok-core::html`, which is the one place that turns that document back into HTML for `ok serve`, escaping every text run and allowlisting external link schemes, since ZIM content is untrusted.

`crates/cli` is the `ok` binary: the ratatui reader (default), `ok serve` (an `axum` web UI matching the reader's UX, heavy `Library` calls in `spawn_blocking`), and `ok mcp` (three tools — `search`, `read`, `links` — over stdio via the official `rmcp` SDK, identifiers always titles or paths, never entry indices). Only `serve`, `mcp` and `bench --http` construct a tokio runtime; every other subcommand, including the default TUI, pays no async startup cost.

## Tests

```sh
scripts/fetch-test-data.sh   # small real ZIM files from openZIM's test suite, not committed
cargo test
```

## License

The code is MIT, in `LICENSE`. No Wikipedia content ships in this repository. You
download a ZIM file from Kiwix yourself and the reader keeps it in `data/`, which
is not committed. The text inside that file stays under its own license, [CC BY-SA
4.0](https://creativecommons.org/licenses/by-sa/4.0/) for Wikipedia, and the reader
does not yet print a per-article source and license line. That belongs in every
frontend before anyone publishes a rendered article.
