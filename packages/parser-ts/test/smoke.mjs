// End-to-end smoke test: load the compiled Pine grammar WASM via
// web-tree-sitter, parse a real Pine script, verify the tree.
// Verifies the tree-sitter-pine → web-tree-sitter → AST path works.

import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Parser, Language } from 'web-tree-sitter';

const here = path.dirname(fileURLToPath(import.meta.url));
const wasmPath = path.resolve(here, '..', '..', 'tree-sitter-pine', 'tree-sitter-pine.wasm');
const sampleFile = path.resolve(here, 'fixtures', 'mtf-ema-cross.pine');

async function main() {
  console.log(`[smoke] Parser.init()`);
  await Parser.init();

  console.log(`[smoke] loading WASM: ${wasmPath}`);
  const pine = await Language.load(wasmPath);

  const parser = new Parser();
  parser.setLanguage(pine);

  console.log(`[smoke] reading sample: ${sampleFile}`);
  const source = fs.readFileSync(sampleFile, 'utf8');

  console.log(`[smoke] parsing ${source.length} bytes`);
  const tree = parser.parse(source);
  const root = tree.rootNode;
  console.log(`[smoke] root type=${root.type} namedChildCount=${root.namedChildCount}`);

  if (root.type !== 'source_file') throw new Error(`expected source_file, got ${root.type}`);
  if (root.namedChildCount === 0) throw new Error('no named children at root');

  // Print first few named children
  for (let i = 0; i < Math.min(3, root.namedChildCount); i++) {
    const child = root.namedChild(i);
    console.log(
      `[smoke]   child[${i}] type=${child.type} range=[${child.startIndex}..${child.endIndex}]`,
    );
  }

  console.log(`[smoke] applying trivial edit + incremental reparse`);
  const lines = source.split('\n').length - 1;
  tree.edit({
    startIndex: source.length,
    oldEndIndex: source.length,
    newEndIndex: source.length + 1,
    startPosition: { row: lines, column: 0 },
    oldEndPosition: { row: lines, column: 0 },
    newEndPosition: { row: lines + 1, column: 0 },
  });
  const newTree = parser.parse(source + '\n', tree);
  console.log(
    `[smoke] reparsed: root type=${newTree.rootNode.type} namedChildCount=${newTree.rootNode.namedChildCount}`,
  );

  console.log(`[smoke] OK`);
}

main().catch((err) => {
  console.error(`[smoke] FAILED:`, err);
  process.exit(1);
});
