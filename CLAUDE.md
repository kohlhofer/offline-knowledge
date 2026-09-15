# offline-knowledge

An offline knowledge appliance: Wikipedia (and later other datasets) as Kiwix ZIM
files, imported once into fast indexes, read through a terminal UI. Rust from the
start. The plan lives in the engineering knowledge base:
`~/knowledge/engineering_kb/Outputs/2026-09-14_offline-knowledge-appliance-plan.md`.

## Layout

- `crates/zim` (`ok-zim`): read-only ZIM reader. Memory-mapped, bounded against
  hostile files, verified against libzim. `write.rs` is a test-only writer.
- `crates/core` (`ok-core`): import (title FST, section redirects, inbound links,
  Tantivy full text), the document model, and `Library`, the API every frontend uses.
- `crates/cli` (`ok`): the binary. `import`, `tui` (default), `suggest`, `search`,
  `show`, `bench`.

Frontends stay thin. Anything a second frontend (HTTP, MCP) would need goes in
`ok-core`, not in the TUI.

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
- Text from a ZIM file is untrusted: the TUI strips control characters before it
  reaches the terminal, and any future MCP surface must present article text as data.
