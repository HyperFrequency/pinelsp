# Changelog

All notable changes to the `pine-rs` workspace are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-06-28

First tagged release of the Rust toolchain — a TypeScript-to-Rust *superset*
port of the `pinelsp` TS monorepo. Built incrementally across iterations 1-5.

### Added

- **7-crate workspace**: `tree-sitter-pine` (grammar), `pine-core` (parsing +
  document model), `pine-data-codegen` (embedded builtins database),
  `pine-check` (semantic analysis), `pine-lsp` (language server), `pine-cli`
  (command-line linter), and `pine-mcp` (MCP server).
- **Builtins data-codegen**: the Pine v6 functions/variables/constants/keywords
  database is embedded at build time so the toolchain needs no runtime data
  files.
- **LSP features** (`pine-lsp`, tower-lsp): diagnostics, hover, completion,
  go-to-definition, document symbols, formatting, semantic tokens, folding,
  inlay hints, and code actions.
- **Checker + CLI oracle parity**: `pine-check` ports the TS
  `UnifiedPineValidator` to the tree-sitter CST (version directive,
  unused-variable, undeclared-identifier, type-mismatch, call-argument,
  na-comparison, constant-condition checks). CLI/oracle differential parity is
  6/7 against the TradingView pine-lint oracle.
- **Logic-lint**: heuristic best-practice warnings ported from the zelosleone
  analyzer (request.security lookahead, negative history access, self-assignment,
  duplicate parameters, stateful `ta.*` in conditionals, strategy-order checks).
- **True incremental parse**: `InputEdit`-based reparsing for low-latency edits.
- **MCP server** (`pine-mcp`): validate / lookup / list / format tools over the
  Model Context Protocol.
- **Multi-file imports**: `import` resolution and cross-file IntelliSense
  (ported from `imports.ts`).
- **Grammar**: 41/42 corpus fixtures passing (the residual fixture is a
  keywords-as-params case limited by the upstream tree-sitter parser).

### Notes

- Workspace MSRV is Rust 1.88 (edition 2024 plus stabilized let-chains).
- This is an independent community project, not affiliated with TradingView.
