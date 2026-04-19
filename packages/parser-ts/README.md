# @pine-lsp/parser-ts

Tree-sitter-based Pine Script parser for Node/TypeScript consumers.
Loads the compiled WASM grammar from `packages/tree-sitter-pine/` via
`web-tree-sitter` and exposes an incremental-parse API.

## When to use this instead of `packages/core/src/parser`

Almost never, by default. Folknor's `core/parser` is the authoritative
parser — it's differentially tested against TradingView's pine-lint and
encodes years of Pine semantic quirks. Use it for:

- Diagnostics / validation
- Completion / hover / go-to-def
- Anything the LSP handler path already uses

Use `parser-ts` only for:

1. **Incremental reparse** — tree-sitter's `edit()` + `parse(oldTree)` is
   orders of magnitude faster than whole-file reparse on every keystroke.
   The LSP server can use this to accelerate diagnostics refresh on large
   files. Measure before adopting.
2. **External tooling** — if a caller already uses tree-sitter and wants
   a Pine AST without pulling in folknor's full semantic model.
3. **Editor-native highlighting** — editors that consume `highlights.scm`
   directly (Helix, Neovim, Zed) don't need this package at all; they
   load the `.wasm` / `.so` themselves. This package exists for the
   TypeScript/Node side of the ecosystem.

## API

```typescript
import { TreeSitterPineParser } from '@pine-lsp/parser-ts';

const parser = await TreeSitterPineParser.create();
const tree = parser.parse('//@version=6\nindicator("hi")');
// later, on edit:
const updated = parser.edit(tree, { startIndex, oldEndIndex, newEndIndex,
  startPosition, oldEndPosition, newEndPosition });
const newTree = parser.parse(sourceAfterEdit, updated);
```

See `ARCHITECTURE.md` in the workspace root for the combination design.
