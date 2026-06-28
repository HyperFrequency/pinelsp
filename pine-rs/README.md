# pine-rs

A Rust port of the Pine Script v6 toolchain — **language server, MCP server, and CLI
linter** — built as a *superset* of the original TypeScript implementation plus a
full multi-language tree-sitter grammar binding set.

See [`STATUS.md`](./STATUS.md) for the verified proof summary and the residual-item
ledger.

## Workspace

A Cargo workspace of seven crates, deliberately decoupling analysis from LSP
transport so the CLI and MCP server can reuse the checks without pulling in
`lsp_types`:

| Crate | Responsibility |
|---|---|
| `tree-sitter-pine` | The Pine v6 grammar (kvarenzn base + `enum`, ABI 15) and its C/C++/Rust/Node/Python/Swift/Go/WASM bindings. Exposes `language()`, `HIGHLIGHTS_QUERY`, `FOLDS_QUERY`, `LOCALS_QUERY`. |
| `pine-data-codegen` | Embeds the canonical builtins (457 functions / 90 variables / 237 constants / 28 keywords) from the TS `pine-data` as `&'static` tables. |
| `pine-core` | `Document` (incremental tree + UTF-16-aware `LineIndex`), symbol resolution, builtins access. No LSP types. |
| `pine-check` | The analyzer: type/argument/identifier checks (oracle-validated against TradingView) plus `logiclint` domain checks (lookahead bias, future-leak). |
| `pine-lsp` | A `tower-lsp` backend with 14 providers and INCREMENTAL sync via tree-sitter `InputEdit`. |
| `pine-mcp` | A stdio JSON-RPC MCP server exposing 4 tools (validate / lookup / list / format). |
| `pine-cli` | A `clap` CLI linter emitting human or `--json` diagnostics — also the oracle harness contract. |

## Build & test

```bash
cd pine-rs
cargo test                              # full workspace test suite
cargo run -p pine-cli -- some.pine      # lint a file (human output)
cargo run -p pine-cli -- --json some.pine   # machine-readable diagnostics
python3 scripts/differential.py         # checker parity vs TradingView translate_light
```

## Editor integration

The VS Code extension launches this server when `pine.rustServerPath` points at the
release binary; otherwise it falls back to the bundled TypeScript server (no
regression):

```bash
cargo build --release -p pine-lsp
# VS Code settings.json:
#   "pine.rustServerPath": "/absolute/path/to/pine-rs/target/release/pine-lsp"
```

## Grammar bindings

The grammar ships with bindings for C, C++, Rust, Python, Go, Swift, and WASM
(via `web-tree-sitter`, used by the Next.js demo under
`crates/tree-sitter-pine/examples/nextjs-demo/`). The external scanner
(`src/scanner.c`, indent/dedent/newline) is wired into every binding's build —
the omission that breaks most upstream kvarenzn forks. Node-native and C# bindings
are authored but blocked by this environment's toolchain (node-gyp on node 26;
no dotnet SDK).
