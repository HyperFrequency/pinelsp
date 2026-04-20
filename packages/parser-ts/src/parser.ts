// TreeSitterPineParser — thin wrapper over web-tree-sitter for Pine Script v6.
// Loads the compiled grammar (packages/tree-sitter-pine/tree-sitter-pine.wasm)
// and exposes an incremental-parse API.

import { type Edit, Language, Parser, type Tree } from "web-tree-sitter";

export interface TreeSitterPineParserOptions {
	/** Absolute path to `tree-sitter-pine.wasm`. Required — callers
	 *  resolve via `require.resolve("@pine-lsp/tree-sitter-pine/tree-sitter-pine.wasm")`
	 *  under CJS or `import.meta.resolve(...)` under ESM. A default
	 *  computed from module location would require `import.meta.url`,
	 *  which is incompatible with this monorepo's CommonJS tsc target. */
	wasmPath: string;
}

export class TreeSitterPineParser {
	private readonly parser: Parser;

	private constructor(parser: Parser) {
		this.parser = parser;
	}

	static async create(
		options: TreeSitterPineParserOptions,
	): Promise<TreeSitterPineParser> {
		await Parser.init();
		const parser = new Parser();
		const language = await Language.load(options.wasmPath);
		parser.setLanguage(language);
		return new TreeSitterPineParser(parser);
	}

	/** Parse source into a concrete syntax tree. If `previous` is provided
	 *  (after its `.edit()` descriptor has been applied), tree-sitter
	 *  performs an incremental reparse — much faster for small edits. */
	parse(source: string, previous?: Tree): Tree {
		const tree = this.parser.parse(source, previous);
		if (!tree) {
			throw new Error(
				"tree-sitter parse returned null — grammar load likely failed",
			);
		}
		return tree;
	}

	/** Apply an edit descriptor to a tree so tree-sitter knows which bytes
	 *  changed. Call this before `parse(source, editedTree)`. */
	edit(tree: Tree, edit: Edit): void {
		tree.edit(edit);
	}
}

export type { Edit, Tree } from "web-tree-sitter";
