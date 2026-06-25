use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_pine() -> Language;
}

/// Returns the tree-sitter Language for this grammar.
pub fn language() -> Language {
    unsafe { tree_sitter_pine() }
}

/// The node-types.json content for this grammar.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// Tree-sitter highlight query (capture names map to editor/semantic-token types).
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// Tree-sitter fold query (foldable node kinds, captured as `@fold`).
pub const FOLDS_QUERY: &str = include_str!("../../queries/folds.scm");

/// Tree-sitter locals query (scopes/definitions/references).
pub const LOCALS_QUERY: &str = include_str!("../../queries/locals.scm");

#[cfg(test)]
mod tests {
    use tree_sitter::Query;

    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::language())
            .expect("Error loading Pine grammar");
    }

    #[test]
    fn fold_query_compiles() {
        Query::new(&super::language(), super::FOLDS_QUERY).expect("folds.scm must match grammar");
    }

    #[test]
    fn highlights_query_compiles() {
        Query::new(&super::language(), super::HIGHLIGHTS_QUERY)
            .expect("highlights.scm must match grammar");
    }
}
