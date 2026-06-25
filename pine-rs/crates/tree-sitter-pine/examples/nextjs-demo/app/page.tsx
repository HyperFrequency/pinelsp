"use client";

// Browser demo: load the Pine WASM grammar with web-tree-sitter and render the
// parse tree of a snippet. The grammar is consumed entirely client-side — the
// same .wasm the LSP loads — proving the WASM/JS binding works in a web app.
//
// Setup: copy ../../tree-sitter-pine.wasm and node_modules/web-tree-sitter/
// tree-sitter.wasm into ./public, then `npm run dev`.

import { useEffect, useState } from "react";
import { Parser, Language } from "web-tree-sitter";

const SAMPLE = `//@version=6
indicator("Demo")
fast = ta.sma(close, 14)
plot(fast, color = color.red)
`;

export default function Page() {
  const [sexp, setSexp] = useState("loading…");
  const [hasError, setHasError] = useState(false);

  useEffect(() => {
    (async () => {
      // web-tree-sitter loads its own runtime wasm from /public by default.
      await Parser.init();
      const pine = await Language.load("/tree-sitter-pine.wasm");
      const parser = new Parser();
      parser.setLanguage(pine);
      const tree = parser.parse(SAMPLE);
      setSexp(tree.rootNode.toString());
      setHasError(tree.rootNode.hasError);
    })();
  }, []);

  return (
    <main style={{ fontFamily: "monospace", padding: 24 }}>
      <h1>tree-sitter-pine — WASM demo</h1>
      <pre>{SAMPLE}</pre>
      <p>hasError: {String(hasError)}</p>
      <pre style={{ whiteSpace: "pre-wrap" }}>{sexp}</pre>
    </main>
  );
}
