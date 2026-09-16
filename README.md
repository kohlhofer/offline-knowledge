# offline-knowledge

Wikipedia on your own disk, read in milliseconds. A keystroke brings up titles in 0.2 ms, a link opens a rendered article in about 5 ms, and the library itself opens in 10. One index, three ways into it: a terminal reader, a web UI, and an MCP server for agents. All three go through `ok-core`'s `Library`, so the numbers below apply whichever one you use.

The collection is a file you downloaded. Nothing is fetched while you read, no service answers the query, and the file stays the same until you replace it.

## Speed

`ok bench --samples 500 --http` on an M3 MacBook Air (16 GB, macOS 26.5), release build, warm page cache, 2026-09-15, against Kiwix's 50,000-article English Wikipedia (`wikipedia_en_top_nopic`, 2.1 GB):

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

The terminal reader pays nothing beyond that first row. Only `serve`, `mcp` and `bench --http` build a tokio runtime, so the reader and the one-shot commands (`ok suggest`, `ok search`, `ok show --json`) start, answer and exit.

The web UI costs the reader's numbers plus HTTP. A first visit to an article is the `GET /wiki/{path}` row; a revisit costs about 0.2 ms, served from an LRU of rendered articles. Rendering was called too cheap to measure in an earlier pass, and it isn't. Fusing sanitize and HTML-escaping into one pass over the output buffer, then dropping the per-run temporary `String`s and per-heading `format!` calls, took Demographics of the United States (2.17 MB of source HTML, the largest article here) from 3.08 ms to 0.46, and Albert Einstein from 1.13 ms to 0.34. The output is byte-identical over 498 real articles.

For `ok mcp` the budget is tokens. `read` on Albert Einstein, 68 sections, returns 6,176 bytes, about 1,530 tokens, because the default outline lists top-level sections and says how many subsections each one hides. `search "general relativity"` returns 1,172 bytes for 8 hits. `read` on the article's largest section returns 6,115 bytes, and `links` on that section returns 2,250 bytes covering 136 unique articles. Every identifier is a title or a path, so an agent can feed a result straight back in.

The import does the work once. Each ZIM becomes an FST of titles ranked by inbound links plus a Tantivy full-text index, about 90 seconds and 154 MB for these 2.1 GB. Every read after that is a memory-mapped lookup with nothing to warm up.

The slow end of suggestions is one- and two-letter prefixes, which scan tens of thousands of titles. Precomputing those is the obvious next step if it ever shows in use.

Inside Apple's `container` (1.3.1, default 4 CPUs and 1 GB, ZIM on a virtiofs bind mount, host cache warm), an earlier run before `serve` and `mcp` existed stayed within a few milliseconds of native: suggestions p99 20 ms, article loads p99 21 ms, link follows p99 29 ms, searches p99 3.5 ms for a title word and 11 ms for a common one, 29 ms to open the library. The HTTP and MCP paths have not been re-measured there.

The release binary is 16 MB with `axum`, `tokio` and `rmcp` in it, against 10 MB for the reader alone. A clean `cargo build --release -p ok` takes about 75 s from an empty dependency graph, 23 s once third-party crates are built.

## Independent Access

The ZIM file is the only dependency. Give the binary a file and it reads; it listens on a port when you run `ok serve` and tell it where to bind. In Apple's `container` on an `--internal` network, which has no route out, the reader, `ok bench` and `ok serve` all work.

A machine with no egress still has the encyclopedia, and an agent can consult it without the question leaving the host. Kiwix publishes collections at every size, a few hundred megabytes of Simple English up to the full encyclopedia at around 100 GB, and the same binary reads any of them.

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

`ok suggest <prefix>`, `ok search <words>` and `ok show <title> [--json]` print to stdout, and `ok bench` reproduces the table above.

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

The image is Debian 13 slim plus the `ok` binary. `ok mcp` talks stdio, so it runs through `container exec -i <container> ok mcp` rather than a published port. The import has only run on the Mac so far. It writes the index next to the ZIM in `data/`, which the container reads through the mount.

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

## How It Works

`crates/zim` reads ZIM files without libzim. It memory-maps the file, parses directory entries in place, hands out uncompressed blobs as slices of the mapping, and decompresses zstd or xz clusters exactly as far as their offset tables say. Every size the file declares is checked and capped, because a ZIM file is input from the internet. A sampled comparison over 7,086 entries matches libzim byte for byte.

`crates/core` imports a file once. It finds the real articles, turns mwoffliner's meta-refresh pages (171,945 section redirects in this file) into a lookup table, parses every article, counts inbound links, and writes the title FST and the Tantivy index. Articles reach every interface as a structured document of sections, paragraphs, lists, facts and resolved links, never as HTML. The exception is `ok-core::html`, the one place that turns that document back into HTML for `ok serve`, escaping every text run and allowlisting external link schemes, since ZIM content is untrusted.

`crates/cli` is the `ok` binary: the ratatui reader (default), `ok serve` (an `axum` web UI matching the reader's UX, heavy `Library` calls in `spawn_blocking`), and `ok mcp` (three tools, `search`, `read` and `links`, over stdio via the official `rmcp` SDK).

## Tests

```sh
scripts/fetch-test-data.sh   # small real ZIM files from openZIM's test suite, not committed
cargo test
```

## License

The code is MIT, in `LICENSE`. No Wikipedia content ships in this repository. You download a ZIM file from Kiwix yourself and the reader keeps it in `data/`, which is not committed. The text inside that file stays under its own license, [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) for Wikipedia. The reader does not print a per-article source and license line yet, and it should before anyone publishes a rendered article.
