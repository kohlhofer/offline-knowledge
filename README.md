# offline-knowledge

Wikipedia on your own disk, read in milliseconds. A keystroke brings up titles in 0.15 ms, a link opens a rendered article in about 5 ms, and a collection's index opens in under 7. One process holds as many collections as you give it, one of them active at a time, and there are three ways in: a terminal reader, a web UI, and an MCP server for agents. All three go through `ok-core`'s `Library`, so the numbers below apply whichever one you use.

Every collection is a file you downloaded. Nothing is fetched while you read, no service answers the query, and the file stays the same until you replace it.

## Speed

`ok bench --samples 500 --http` on an M3 MacBook Air (16 GB, macOS 27.0), release build, warm page cache, load average 4.5, 2026-09-28, against Kiwix's 50,000-article English Wikipedia (`wikipedia_en_top_nopic`, 2.1 GB) as the only collection:

| Operation | p50 | p99 | max |
| --- | --- | --- | --- |
| Open the collection set | 0.04 ms | | |
| Open that collection's index | 6.3 ms | | |
| Title suggestions (1 to 6 characters) | 0.15 ms | 13 ms | 14 ms |
| Load and parse an article | 3.3 ms | 10 ms | 13 ms |
| Lay out at 100 columns | 0.34 ms | 1.2 ms | 1.4 ms |
| Follow a link (load, parse, layout) | 5.0 ms | 16 ms | 26 ms |
| Full-text search, a title word | 0.17 ms | 0.85 ms | 1.0 ms |
| Full-text search, a common word | 0.50 ms | 1.4 ms | 1.4 ms |
| `GET /{collection}/{path}` over HTTP, cache miss (resolve + load + parse + render) | 3.7 ms | 12 ms | 20 ms |

The first row is every collection's `meta.json`, which is all that loading a set reads; the second is the one collection a command actually uses. The terminal reader pays nothing beyond those two. Only `serve`, `mcp` and `bench --http` build a tokio runtime, so the reader and the one-shot commands (`ok suggest`, `ok search`, `ok show --json`) start, answer and exit. What a directory of collections costs at startup is under Collections below.

The web UI costs the reader's numbers plus HTTP. A first visit to an article is the `GET /{collection}/{path}` row; a revisit costs about 0.2 ms, served from an LRU of rendered articles, keyed by collection and entry. Rendering was called too cheap to measure in an earlier pass, and it isn't. Fusing sanitize and HTML-escaping into one pass over the output buffer, then dropping the per-run temporary `String`s and per-heading `format!` calls, took Demographics of the United States (2.17 MB of source HTML, the largest article here) from 3.08 ms to 0.46, and Albert Einstein from 1.13 ms to 0.34. The output is byte-identical over 498 real articles.

For `ok mcp` the budget is tokens. Same machine and date, one collection loaded: `read` on Albert Einstein, 68 sections, returns 6,158 bytes, about 1,530 tokens, because the default outline lists top-level sections and says how many subsections each one hides. `search "general relativity"` returns 1,637 bytes for 8 hits. `read` on the article's largest section returns 6,133 bytes, and `links` on that section returns 6,217 bytes covering 136 unique articles. Every identifier is a title or a path, so an agent can feed a result straight back in. With the three collections in `data/` loaded, each identifier carries its label too: that `links` call becomes 7,577 bytes, the `read` 6,168, and the handshake instructions grow from 551 bytes to 815.

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

## Collections

One process holds several ZIM files. Exactly one is active for any request, and every ranked list, meaning suggestions, full text and random, comes from that one collection.

```sh
export OK_ZIM=data                                # every *.zim directly inside, in filename order
./target/release/ok --zim data/ import            # once per file; a built index is left alone
./target/release/ok collections                   # loaded, failed and skipped, with reasons
./target/release/ok --collection wiktionary suggest merc
```

A label comes from the ZIM's `Name` metadata, the text before the first `_`, lowercased: `wikipedia_en_top` gives `wikipedia`, `archlinux_en_all` gives `archlinux`. That token is the URL segment, the MCP prefix and the row in the web switcher, and it stays put across editions of the same project. The ZIM's own `Title` stays the brand a reader sees. The first collection loaded is the default, so `--zim data/` alone defaults to whichever filename sorts first; `--collection <label>` or `OK_COLLECTION` picks another.

A file that cannot be loaded is named on stderr, skipped, and listed by `ok collections`: no index yet, an index this version cannot read, a ZIM another scraper wrote (`ok` reads mwoffliner's HTML only), or a label that is unusable or already taken. Everything else still serves. The process fails only when nothing loads.

In the web UI an article is `/{collection}/{path}`, while `/search`, `/api/suggest` and `/random` take `?c=<label>`. A path segment would make "search" unreachable as an article title, and article titles include it. `/wiki/{path}`, the URL earlier versions handed out, is a 302 to the collection labeled `wikipedia`, query string intact, so old links and bookmarks still land. `/` lists what is loaded once there is more than one, and the header carries a row of labels to switch with.

In the terminal reader Ctrl-T opens a picker of every collection with its label, title and article count. A switch clears the open article and the back and forward stacks, because an entry index means a different article in a different file.

Over MCP every tool takes an optional `collection`, and an identifier can carry its own label as `wikipedia/Albert Einstein`. The whole identifier sits inside one fence, so the token an agent copies stays whole. The handshake instructions list what is loaded.

A miss says whether another collection has that exact title, on the web 404, in the reader's status line and in an MCP error. The probe opens the other collections' title indexes and nothing else, and it is an existence check on an exact title with no ranking in it. Nothing else in the program crosses a collection.

Startup, same machine and date as the table above: one collection's `meta.json` is 0.04 ms, and the three files in `data/` cost 3.3 ms, because those three indexes predate the `Name` and `Scraper` fields in `meta.json` and each label is read from the ZIM instead. That read is 1.1 ms for the 26 MB file, 1.7 ms for the 34 MB one and 1.6 ms for the 2.1 GB one: the cost is decompressing the metadata cluster, and it does not track file size. An index written by this version records both fields and pays none of it. The Wikipedia index keeps paying, because an index lives at `<file>.okx` and `import` replaces it in place, so rebuilding it means 90 seconds with nothing reading that file.

## In a Container

```sh
container build -c 8 -m 8g -t offline-knowledge:dev .
container network create --internal offline      # once: a host-only network with no route out
container run -it --rm --network offline -v "$PWD/data:/data" offline-knowledge:dev
container run -it --rm --network offline -p 127.0.0.1:8080:8080 -v "$PWD/data:/data" offline-knowledge:dev serve --bind 0.0.0.0:8080
```

The image is Debian 13 slim plus the `ok` binary, with `OK_ZIM=/data`, so every imported ZIM on the mount is a collection and the first filename is the default. `ok mcp` talks stdio, so it runs through `container exec -i <container> ok mcp` rather than a published port. The import has only run on the Mac so far. It writes the index next to the ZIM in `data/`, which the container reads through the mount.

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
| Anywhere | Ctrl-T | pick a collection (when more than one is loaded) |

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

The collection switcher in the header is a row of ordinary links, one per loaded collection, so it works with JavaScript off like everything else here.

## How It Works

`crates/zim` reads ZIM files without libzim. It memory-maps the file, parses directory entries in place, hands out uncompressed blobs as slices of the mapping, and decompresses zstd or xz clusters exactly as far as their offset tables say. Every size the file declares is checked and capped, because a ZIM file is input from the internet. A sampled comparison over 7,086 entries matches libzim byte for byte.

`crates/core` imports a file once. It finds the real articles, turns mwoffliner's meta-refresh pages (171,945 section redirects in this file) into a lookup table, parses every article, counts inbound links, and writes the title FST and the Tantivy index. `collections` holds the set of imported files with each one's validated label, opening a `Library` on first use so a set costs nothing to load. Articles reach every interface as a structured document of sections, paragraphs, lists, facts and resolved links, never as HTML. The exception is `ok-core::html`, the one place that turns that document back into HTML for `ok serve`, escaping every text run and allowlisting external link schemes, since ZIM content is untrusted.

`crates/cli` is the `ok` binary: the ratatui reader (default), `ok serve` (an `axum` web UI matching the reader's UX, heavy `Library` calls in `spawn_blocking`), and `ok mcp` (three tools, `search`, `read` and `links`, over stdio via the official `rmcp` SDK).

## Tests

```sh
scripts/fetch-test-data.sh   # small real ZIM files from openZIM's test suite, not committed
cargo test
```

## License

The code is MIT, in `LICENSE`. No Wikipedia content ships in this repository. You download a ZIM file from Kiwix yourself and the reader keeps it in `data/`, which is not committed. The text inside that file stays under its own license, [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) for Wikipedia. The reader does not print a per-article source and license line yet, and it should before anyone publishes a rendered article.
