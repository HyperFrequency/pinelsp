//! `pine-core` — parsing and the document model for the Pine Script Rust toolchain.
//!
//! P0 scope (this phase): wrap tree-sitter parsing of Pine v6 source behind a
//! small [`Document`] type. The type system, symbol tables, type inference, and
//! true incremental reparse (tree-sitter `InputEdit` reuse) arrive in later
//! phases; for now a `Document` is a full parse of its source text.

pub mod builtins;
pub mod imports;
pub mod symbols;
pub mod text;

use text::LineIndex;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

fn point((row, column): (usize, usize)) -> Point {
    Point { row, column }
}

/// A parsed Pine Script document: the source text plus its tree-sitter tree.
///
/// Construct with [`Document::parse`]. The grammar's root node kind is
/// `source_file`.
pub struct Document {
    text: String,
    tree: Tree,
    line_index: LineIndex,
}

impl Document {
    /// Parse `text` as Pine source.
    ///
    /// Returns `None` only if the grammar fails to load or tree-sitter returns
    /// no tree. With the bundled Pine grammar and UTF-8 input neither happens in
    /// practice, but the fallible signature keeps the failure explicit rather
    /// than panicking inside a language server.
    pub fn parse(text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_pine::language()).ok()?;
        let tree = parser.parse(text.as_str(), None)?;
        let line_index = LineIndex::new(&text);
        Some(Self {
            text,
            tree,
            line_index,
        })
    }

    /// The source text this document was parsed from.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// UTF-16-aware byte/position mapping for this document's text.
    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// LSP position (0-based line, UTF-16 char) -> byte offset.
    pub fn offset_at(&self, line: u32, character_utf16: u32) -> usize {
        self.line_index.offset_at(&self.text, line, character_utf16)
    }

    /// Byte offset -> LSP position (0-based line, UTF-16 char).
    pub fn position_at(&self, offset: usize) -> (u32, u32) {
        self.line_index.position_at(&self.text, offset)
    }

    /// Apply a single edit — replace the byte range `start_byte..old_end_byte`
    /// with `new_text` — and **incrementally** reparse, reusing the previous
    /// tree (tree-sitter reuses unchanged subtrees). Points are byte-based, as
    /// `InputEdit` requires.
    pub fn apply_edit(&mut self, start_byte: usize, old_end_byte: usize, new_text: &str) {
        let start_position = point(self.line_index.byte_to_point(start_byte));
        let old_end_position = point(self.line_index.byte_to_point(old_end_byte));
        let new_end_byte = start_byte + new_text.len();

        self.text.replace_range(start_byte..old_end_byte, new_text);
        let new_index = LineIndex::new(&self.text);
        let new_end_position = point(new_index.byte_to_point(new_end_byte));

        self.tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        });

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_pine::language())
            .expect("Pine grammar loads");
        // Reuse the edited tree for incremental reparse.
        if let Some(tree) = parser.parse(self.text.as_str(), Some(&self.tree)) {
            self.tree = tree;
        }
        self.line_index = new_index;
    }

    /// The underlying tree-sitter syntax tree.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The root node (kind `source_file`).
    pub fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    /// True if the parse tree contains any ERROR or MISSING nodes.
    pub fn has_errors(&self) -> bool {
        self.root().has_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Minimal v6 scripts that must parse with zero error/missing nodes. These
    /// pin the parser's happy path so later grammar changes can't silently
    /// regress basic parsing.
    const VALID_SNIPPETS: &[&str] = &[
        "//@version=6\nindicator(\"Test\")\nplot(close)\n",
        "//@version=6\nstrategy(\"S\")\nfast = ta.sma(close, 14)\nplot(fast)\n",
        "//@version=6\nindicator(\"Z\")\nint n = 5\nplot(n > 0 ? high : low)\n",
    ];

    #[test]
    fn grammar_loads() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_pine::language())
            .expect("Pine grammar should load");
    }

    #[test]
    fn incremental_edit_matches_full_parse() {
        let mut doc = Document::parse("//@version=6\nx = 1\nplot(x)\n").unwrap();
        // Replace the `1` in `x = 1` with `42`.
        let one = doc.text().find("= 1").unwrap() + 2;
        doc.apply_edit(one, one + 1, "42");
        assert_eq!(doc.text(), "//@version=6\nx = 42\nplot(x)\n");
        // The incrementally-reparsed tree must equal a from-scratch parse.
        let fresh = Document::parse(doc.text()).unwrap();
        assert_eq!(
            doc.root().to_sexp(),
            fresh.root().to_sexp(),
            "incremental tree must match full parse"
        );
        assert!(!doc.has_errors());
    }

    #[test]
    fn incremental_edit_multibyte() {
        // Edit after a multibyte char must keep byte offsets correct.
        let mut doc = Document::parse("//@version=6\ns = \"😀\"\ny = 1\n").unwrap();
        let one = doc.text().rfind("= 1").unwrap() + 2;
        doc.apply_edit(one, one + 1, "2");
        let fresh = Document::parse(doc.text()).unwrap();
        assert_eq!(doc.root().to_sexp(), fresh.root().to_sexp());
    }

    #[test]
    fn root_is_source_file() {
        let doc = Document::parse(VALID_SNIPPETS[0]).expect("parse");
        assert_eq!(doc.root().kind(), "source_file");
    }

    #[test]
    fn valid_snippets_parse_without_errors() {
        for src in VALID_SNIPPETS {
            let doc = Document::parse(*src).expect("parse returns a tree");
            assert!(
                !doc.has_errors(),
                "expected clean parse but tree had errors:\n{src}\n--- sexp ---\n{}",
                doc.root().to_sexp()
            );
        }
    }

    /// Corpus smoke test (the P0 proof: "parse fixture corpus, no panic"). Walks
    /// the TS project's syntax fixtures if present and asserts every one yields a
    /// tree without panicking, reporting how many parse error-free. Skips
    /// gracefully when the fixtures aren't on disk so the crate isn't hard-wired
    /// to the monorepo layout.
    #[test]
    fn fixture_corpus_parses() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../packages/core/test/fixtures/syntax");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skip: fixtures not found at {}", dir.display());
            return;
        };

        let mut total = 0usize;
        let mut error_free = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pine") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read fixture");
            let doc = Document::parse(src).expect("parse returns a tree");
            total += 1;
            if !doc.has_errors() {
                error_free += 1;
            }
        }

        eprintln!("fixture corpus: {error_free}/{total} parsed error-free");
        assert!(
            total > 0,
            "expected at least one .pine fixture in {}",
            dir.display()
        );
    }
}
