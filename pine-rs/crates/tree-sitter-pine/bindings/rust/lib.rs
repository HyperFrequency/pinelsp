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

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::language())
            .expect("Error loading Pine grammar");
    }
}
