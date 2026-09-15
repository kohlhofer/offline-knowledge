# offline-knowledge

An offline knowledge appliance: Wikipedia (and later other datasets) as Kiwix ZIM
files, imported once into fast indexes, read through a terminal UI. Rust from the
start. The plan lives in the engineering knowledge base:
`~/knowledge/engineering_kb/Outputs/2026-09-14_offline-knowledge-appliance-plan.md`.

## Layout

- `crates/zim` (`ok-zim`): read-only ZIM reader. Memory-mapped, bounded against
  hostile files, verified against libzim. `write.rs` is a test-only writer.
- `crates/core` (`ok-core`): import (title FST, section redirects, inbound links,
  Tantivy full text), the document model, `html.rs` (Document→HTML, the one place
  that renders ZIM content back into markup), `text::sanitize` (shared control-character
  stripping), and `Library`, the API every frontend uses.
- `crates/cli` (`ok`): the binary. `import`, `tui` (default), `suggest`, `search`,
  `show`, `bench` (`--http` benches `serve` on an ephemeral port), `serve` (the web UI,
  `axum`), `mcp` (three tools over stdio, `rmcp`).

Frontends stay thin. Anything a second frontend needs goes in `ok-core`, not in the
TUI — `resolve_title`, `Library::path`, `Section`/`Document`'s section-range methods
and `html.rs` all moved there for exactly this reason when `serve` and `mcp` were built.

## Commands

Rust comes from Homebrew's keg-only `rustup`, so put it on PATH first:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
scripts/fetch-test-data.sh            # once: small openZIM fixtures into testdata/
cargo test                            # all crates
cargo build --release -p ok
./target/release/ok --zim data/wikipedia_en_top_nopic_2026-06.zim import
./target/release/ok --zim data/wikipedia_en_top_nopic_2026-06.zim bench
```

## Rules

- Every source change comes with a test change, or a one-line reason it doesn't.
- Never rewrite or truncate a ZIM file in place while anything has it open: the
  reader memory-maps it. Replace files by download-to-temp, verify, rename.
- `data/`, `testdata/`, `logs/` and `target/` are not committed.
- Numbers in the README carry their machine. Re-run `ok bench` after anything that
  touches the read path and update them.
- Text from a ZIM file is untrusted: `ok-core::text::sanitize` strips control characters
  before text reaches the terminal (TUI), a response body (`serve`, escaped too) or MCP
  tool output (plain text). `serve`'s external-link scheme allowlist (http/https/mailto)
  is the same rule applied to hrefs, not just to the text around them.
