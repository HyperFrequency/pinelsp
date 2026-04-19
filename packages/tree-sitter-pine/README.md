# @pine-lsp/tree-sitter-pine

Tree-sitter grammar for Pine Script v6. Imported from
`zelosleone/pinescript-vsc-server-rust/crates/tree-sitter-pine-rs/` via
`git subtree` to preserve upstream history.

## Contents (after subtree import)

- `grammar.js` — grammar definition source.
- `src/parser.c`, `src/scanner.c` — generated C parser (from grammar.js).
- `queries/{highlights,folds,locals,tags}.scm` — tree-sitter queries
  consumed by editors and code-search tools.
- `bindings/rust/` — Rust bindings (upstream, unchanged).
- `bindings/node/` — Node bindings (added by this fork).
- `tree-sitter-pine.wasm` — compiled WASM, built by
  `scripts/build-grammar.sh`.

## Pull updates from upstream

```bash
# From repo root
git subtree pull \
  --prefix=packages/tree-sitter-pine \
  https://github.com/zelosleone/pinescript-vsc-server-rust \
  master --squash
```

Then regenerate + rebuild:

```bash
pnpm run build:grammar
```

## Build outputs

- `tree-sitter-pine.wasm` — consumed by `@pine-lsp/parser-ts` via
  `web-tree-sitter`.
- Native `.node` addon (if Node bindings used directly) — via
  `node-gyp` during `pnpm install`.

## License

MIT (matches both upstream and this fork).
