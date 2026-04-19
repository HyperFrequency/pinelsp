#!/usr/bin/env bash
# build-grammar.sh — regenerate tree-sitter-pine from grammar.js and
# produce the WASM artefact consumed by @pine-lsp/parser-ts.
#
# Requires: tree-sitter-cli (install via `pnpm add -g tree-sitter-cli`
# or `cargo install tree-sitter-cli`), and emscripten (auto-downloaded by
# tree-sitter on first WASM build).
#
# Not yet wired into package.json scripts. Invoke manually once the
# tree-sitter-pine package has been imported via git subtree.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GRAMMAR_DIR="$REPO_ROOT/packages/tree-sitter-pine"

if [ ! -f "$GRAMMAR_DIR/grammar.js" ]; then
  echo "ERROR: $GRAMMAR_DIR/grammar.js not found." >&2
  echo "Run \`git subtree add\` to import the grammar first. See" \
       "packages/tree-sitter-pine/README.md." >&2
  exit 1
fi

if ! command -v tree-sitter >/dev/null 2>&1; then
  echo "ERROR: tree-sitter CLI not found on PATH." >&2
  echo "Install: pnpm add -g tree-sitter-cli  OR  cargo install tree-sitter-cli" >&2
  exit 1
fi

cd "$GRAMMAR_DIR"

echo "[build-grammar] regenerating parser.c from grammar.js"
tree-sitter generate

echo "[build-grammar] building WASM"
tree-sitter build --wasm

echo "[build-grammar] output: $GRAMMAR_DIR/tree-sitter-pine.wasm"
ls -lh tree-sitter-pine.wasm
