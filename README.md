# offline-knowledge

A fast, fully offline reader for Wikipedia and other Kiwix ZIM collections, built to run as a small appliance: one binary in an Apple `container` today, a dedicated machine later. You type, titles appear as you type, and following a link takes a few milliseconds.

The first collection is Kiwix's selection of the best 50,000 English Wikipedia articles (`wikipedia_en_top_nopic`, 2.1 GB). The terminal reader comes first. A graphical interface and an MCP server for agents come later, over the same core.

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

## In a Container

```sh
container build -c 8 -m 8g -t offline-knowledge:dev .
container network create --internal offline      # once: a host-only network with no route out
container run -it --rm --network offline -v "$PWD/data:/data" offline-knowledge:dev
```

The image is Debian 13 slim plus the `ok` binary. On the `offline` network the container has no route to the internet, and everything works: the reader, `ok bench`, all of it. Import on the Mac or in the container, since the index lands next to the ZIM in `data/`.

## Keys

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

## Speed

`ok bench --samples 500` on the M3 MacBook Air (16 GB, macOS 26.5), release build, warm page cache, 2026-09-14:

| Operation | p50 | p99 | max |
| --- | --- | --- | --- |
| Open library | 10.8 ms | | |
| Title suggestions (1 to 6 characters) | 0.28 ms | 22 ms | 56 ms |
| Load and parse an article | 6.3 ms | 22 ms | 29 ms |
| Lay out at 100 columns | 0.60 ms | 2.2 ms | 9.0 ms |
| Follow a link (load, parse, layout) | 9.4 ms | 31 ms | 150 ms |
| Full-text search, a title word | 0.65 ms | 2.4 ms | 4.1 ms |
| Full-text search, a common word | 0.90 ms | 2.1 ms | 2.1 ms |

Inside Apple's `container` (1.3.1, default 4 CPUs and 1 GB, ZIM on a virtiofs bind mount, host cache warm), the same run stayed within a few milliseconds of native: suggestions p99 20 ms, article loads p99 21 ms, link follows p99 29 ms, searches p99 3.5 ms for a title word and 11 ms for a common one, and 29 ms to open the library.

The slow end of suggestions is one- and two-letter prefixes, which scan tens of thousands of titles. Precomputing those is the obvious next step if it ever shows in use.

## How It Works

`crates/zim` reads ZIM files without libzim. It memory-maps the file, parses directory entries in place, hands out uncompressed blobs as slices of the mapping, and decompresses zstd or xz clusters exactly as far as their offset tables say. Every size the file declares is checked and capped, because a ZIM file is input from the internet. A sampled comparison over 7,086 entries matches libzim byte for byte.

`crates/core` imports a file once. It finds the real articles, turns mwoffliner's meta-refresh pages (171,945 section redirects in this file) into a lookup table, parses every article, counts inbound links, and writes an FST of titles ranked by those counts plus a Tantivy full-text index. Articles reach every interface as a structured document of sections, paragraphs, lists, facts and resolved links, never as HTML.

`crates/cli` is the `ok` binary and the ratatui reader.

## Tests

```sh
scripts/fetch-test-data.sh   # small real ZIM files from openZIM's test suite, not committed
cargo test
```
