# tree-sitter-pine — Next.js WASM demo

Parses Pine Script in the browser using the WASM grammar via `web-tree-sitter`
(no native build). The same `tree-sitter-pine.wasm` the language server loads.

```bash
npm install
# web-tree-sitter loads two wasm files from /public:
cp ../../tree-sitter-pine.wasm public/
cp node_modules/web-tree-sitter/tree-sitter.wasm public/
npm run dev   # open http://localhost:3000
```

The WASM/JS consumer path is verified headlessly in CI by
`tree-sitter build --wasm` + a `web-tree-sitter` Node smoke (see the bindings
section of the repo); this app is the interactive example.
