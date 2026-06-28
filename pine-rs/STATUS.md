# pine-rs — TypeScript → Rust port status

A Rust port of the Pine Script v6 toolchain (LSP + MCP + CLI), built as a
**superset** of the TypeScript server plus a full multi-language tree-sitter
grammar binding set. Branch: `feat/rust-server`.

## Proof (verified)

- **Workspace:** 7 crates — `tree-sitter-pine`, `pine-data-codegen`, `pine-core`,
  `pine-check`, `pine-lsp`, `pine-cli`, `pine-mcp`. `cargo test` → **162 passing**.
- **Builtins:** 457 functions / 90 variables / 237 constants / 28 keywords,
  embedded from the canonical TS pine-data.
- **LSP (14 providers, server-verified over stdio):** completion, hover,
  signature help, diagnostics, definition, references, rename (+prepare),
  document & workspace symbols, folding, inlay hints, semantic tokens,
  formatting, code actions. INCREMENTAL sync via tree-sitter `InputEdit`.
- **Checker — oracle parity 7/7, 0 false positives** vs TradingView
  `translate_light` (`scripts/differential.py`): undefined-identifier,
  type-mismatch, unknown-argument, too-many-arguments, na-comparison,
  missing-argument (`ta.*`/`math.*`, required-ness derived from param defaults),
  version, unused-variable. FP-hardened against a 42-fixture corpus.
- **Logic-lint (the zelosleone gap, now in Rust):** repainting `lookahead-bias`,
  `future-leak` (negative history), `strategy-no-orders` (Info), `strategy-no-exit`
  (Info), `ta-in-conditional` series-consistency (Warning), `constant-condition`
  literal-`true`/`false` branch (Warning), `self-assignment` `x := x` (Warning),
  `duplicate-parameter` (Error), `redundant-na` `na(na(x))` (Warning).
  All FP-scanned against the corpus.
- **MCP server:** 4 tools over stdio JSON-RPC (validate / lookup / list / format).
- **Bindings — 7/9 verified:** Rust, C, C++, Python, Go, Swift(build), WASM
  (via web-tree-sitter). Grammar: kvarenzn base + enum, ABI 15.
- **Production hardening:** server `tracing` logging to stderr (`PINE_LOG`/
  `RUST_LOG`; stdout stays pure JSON-RPC); panic isolation — handlers run under
  `catch_unwind` and return safe defaults so one bad request can't kill the
  server; import resolution cached by `(namespace, @source path, mtime)` (no
  per-keystroke re-read/re-parse); stdio LSP integration test (`tests/lsp_stdio.rs`,
  drives the real binary) + large-file perf guard. CI authored + locally green:
  `.github/workflows/rust-ci.yml` (fmt + clippy `-D warnings` + test matrix +
  `cargo deny`) and `rust-release.yml` (per-platform binaries + secret-gated
  registry publish). VS Code extension prefers a bundled Rust binary, falls back
  to the TS server (`pine.rustServerPath` overrides). MIT `LICENSE` + `CHANGELOG`
  + `deny.toml`; crates at 0.1.0 with publish metadata.

## Residual items (external ceilings / open-ended)

- **Release execution (needs a CI runner + credentials):** the Rust CI + release
  workflows are authored and locally validated (fmt/clippy/test/`cargo deny` all
  green here; YAML parses), but EXECUTING GitHub Actions, building the per-platform
  release binaries, and publishing to crates.io / npm / PyPI / NuGet require CI
  runners and registry secrets — not available in this environment.

- **Bindings 2/9 blocked by this environment:** Node-native (node-gyp 8.4.1 can't
  build the tree-sitter npm runtime on node 26 — WASM covers JS meanwhile); C#
  (no dotnet SDK). Swift `swift test` needs XCTest (Xcode), though `swift build`
  passes.
- **Checker full parity:** TS has ~40 checks; the highest-value ones are ported.
  `missing-argument` now fires for `ta.*`/`math.*` builtins (required-ness derived
  from absent parameter defaults in `pine-data-codegen`, oracle case caught, 0 FP);
  other namespaces' required-ness stays upstream-data-gated. Remaining
  (ternary/logical operand types, special-cases) tracked.
- **Grammar v6 completeness: 42/42 — all fixtures parse clean.** Fixed across the
  iterations: block comments, nested generics `<array<float>>`, enum integer
  values, leading-operator line continuation `?`/`:`/`.`, tuple-declaration RHS
  `[a,b] = expr`, indentation edge cases, and soft-keyword identifiers
  (`type`/`series`/`simple`/`const` in expression position). No residual.
- **Imports / multi-file IntelliSense** (`/// @source`): COMPLETE pipeline —
  parse → resolve → IntelliSense. `pine-core::imports`/`resolve_imports` parse and
  resolve local `@source` libs (path-traversal-safe); `pine-lsp` provides hover,
  bare-namespace completion, cross-file `alias.member` completion drawn from a
  library's `export`ed symbols, AND goto-definition that jumps into the library
  file at the exported declaration.

## Try it

```bash
cd pine-rs && cargo test                      # 162 tests
cargo run -p pine-cli -- some.pine            # lint
python3 scripts/differential.py              # oracle parity
# VS Code: set "pine.rustServerPath" to target/release/pine-lsp
```
