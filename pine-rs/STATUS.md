# pine-rs — TypeScript → Rust port status

A Rust port of the Pine Script v6 toolchain (LSP + MCP + CLI), built as a
**superset** of the TypeScript server plus a full multi-language tree-sitter
grammar binding set. Branch: `feat/rust-server`.

## Proof (verified)

- **Workspace:** 7 crates — `tree-sitter-pine`, `pine-data-codegen`, `pine-core`,
  `pine-check`, `pine-lsp`, `pine-cli`, `pine-mcp`. `cargo test` → **107 passing**.
- **Builtins:** 457 functions / 90 variables / 237 constants / 28 keywords,
  embedded from the canonical TS pine-data.
- **LSP (14 providers, server-verified over stdio):** completion, hover,
  signature help, diagnostics, definition, references, rename (+prepare),
  document & workspace symbols, folding, inlay hints, semantic tokens,
  formatting, code actions. INCREMENTAL sync via tree-sitter `InputEdit`.
- **Checker — oracle parity 6/7, 0 false positives** vs TradingView
  `translate_light` (`scripts/differential.py`): undefined-identifier,
  type-mismatch, unknown-argument, too-many-arguments, na-comparison,
  missing-argument (data-gated), version, unused-variable. FP-hardened against a
  42-fixture corpus.
- **Logic-lint (the zelosleone gap, now in Rust):** repainting `lookahead-bias`,
  `future-leak` (negative history), `strategy-no-orders` (Info), `strategy-no-exit`
  (Info), `ta-in-conditional` series-consistency (Warning), `constant-condition`
  literal-`true`/`false` branch (Warning), `self-assignment` `x := x` (Warning).
  All FP-scanned against the corpus.
- **MCP server:** 4 tools over stdio JSON-RPC (validate / lookup / list / format).
- **Bindings — 7/9 verified:** Rust, C, C++, Python, Go, Swift(build), WASM
  (via web-tree-sitter). Grammar: kvarenzn base + enum, ABI 15.

## Residual items (external ceilings / open-ended)

- **Bindings 2/9 blocked by this environment:** Node-native (node-gyp 8.4.1 can't
  build the tree-sitter npm runtime on node 26 — WASM covers JS meanwhile); C#
  (no dotnet SDK). Swift `swift test` needs XCTest (Xcode), though `swift build`
  passes.
- **Checker full parity:** TS has ~40 checks; the highest-value ones are ported.
  `missing-argument` is correct but gated by pine-data's `required` flags (only
  28/457 functions mark any param required). Remaining (ternary/logical operand
  types, special-cases) tracked.
- **Grammar v6 completeness:** 34/42 syntax fixtures parse clean (block comments,
  nested generics `<array<float>>`, and enum integer values now fixed). The
  residual 8 are the hard cases: leading-operator line-continuation (`?`/`:`/`.`/
  `and`/`or` at line start) and tuple-vs-subscript (`[a,b] =` after a statement) —
  both require external-scanner (`scanner.c`) changes on the universal line-break /
  expression-statement path, which is high regression risk. Deferred to a
  dedicated scanner increment gated on the full 42-fixture corpus.
- **Imports / multi-file IntelliSense** (`/// @source`): two slices landed —
  `pine-core::imports` parses `import user/lib/v as alias` + `/// @source`
  directives into a typed `ImportTable`, and `pine-lsp` surfaces imported
  namespaces in hover + completion. Cross-file resolution/loading remains.

## Try it

```bash
cd pine-rs && cargo test                      # 59 tests
cargo run -p pine-cli -- some.pine            # lint
python3 scripts/differential.py              # oracle parity
# VS Code: set "pine.rustServerPath" to target/release/pine-lsp
```
