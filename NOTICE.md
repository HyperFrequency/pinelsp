# NOTICE

This repository is a fork of [`folknor/pine-tools`](https://github.com/folknor/pine-tools),
which itself continues work originally by Jaroslav Pantsjoha
([`jpantsjoha/pinescript-vscode-extension`](https://github.com/jpantsjoha/pinescript-vscode-extension)).

Full upstream attribution is preserved in `LICENSE`.

## Third-party components vendored into this fork

### `packages/tree-sitter-pine/` — tree-sitter grammar for Pine Script v6

Imported from
[`zelosleone/pinescript-vsc-server-rust`](https://github.com/zelosleone/pinescript-vsc-server-rust),
specifically the `crates/tree-sitter-pine-rs/` directory. MIT-licensed upstream;
the original crate metadata (`Cargo.toml`, `bindings/rust/`, `grammar.js`,
`src/parser.c`, `src/scanner.c`, `queries/*.scm`) is preserved unchanged so
upstream updates can be re-synced.

The compiled WASM (`tree-sitter-pine.wasm`, 167KB) is built from
`grammar.js` via `tree-sitter generate && tree-sitter build --wasm` and
consumed by `@pine-lsp/parser-ts` via `web-tree-sitter`.

Upstream: <https://github.com/zelosleone/pinescript-vsc-server-rust>

## Changes from `folknor/pine-tools`

This fork adds:

- `packages/tree-sitter-pine/` — the vendored grammar (above).
- `packages/parser-ts/` — a `web-tree-sitter`-based parser wrapper
  that loads the WASM and exposes an incremental-parse API.
- `scripts/build-grammar.sh` — regenerates and rebuilds the WASM.

Nothing in `packages/core/`, `packages/language-service/`, or
`packages/lsp/` is modified. The tree-sitter path is **additive** —
folknor's semantic parser remains authoritative for completion, hover,
diagnostics, and validation. The tree-sitter backend exists for
incremental reparse (future LSP optimization), editor-native
integration (Helix/Neovim/Zed can consume the WASM and `queries/*.scm`
directly), and external tooling that wants a concrete syntax tree.

See `packages/parser-ts/README.md` for guidance on when to use which
parser, and `pine-lsp/ARCHITECTURE.md` in the originating workspace
for the combination design doc.
