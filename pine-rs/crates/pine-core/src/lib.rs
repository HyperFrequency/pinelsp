//! `pine-core` — parsing and the document model for the Pine Script Rust toolchain.
//!
//! P0 scope (this phase): wrap tree-sitter parsing of Pine v6 source behind a
//! small [`Document`] type. The type system, symbol tables, type inference, and
//! true incremental reparse (tree-sitter `InputEdit` reuse) arrive in later
//! phases; for now a `Document` is a full parse of its source text.

pub mod builtins;

use tree_sitter::{Node, Parser, Tree};

/// A parsed Pine Script document: the source text plus its tree-sitter tree.
///
/// Construct with [`Document::parse`]. The grammar's root node kind is
/// `source_file`.
pub struct Document {
    text: String,
    tree: Tree,
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
        Some(Self { text, tree })
    }

    /// The source text this document was parsed from.
    pub fn text(&self) -> &str {
        &self.text
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
        assert!(total > 0, "expected at least one .pine fixture in {}", dir.display());
    }
}
