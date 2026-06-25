//! Builtins facade over the embedded `pine-data-codegen` database.
//!
//! Downstream crates (pine-check, pine-lsp, pine-mcp, pine-cli) depend on
//! `pine-core`, so the builtins lookups are re-exported here in one place rather
//! than each crate depending on `pine-data-codegen` directly.

pub use pine_data_codegen::{
    constant, function, is_keyword, variable, BuiltinConstant, BuiltinFunction, BuiltinVariable,
    FunctionFlags, Keywords, Param, CONSTANTS, FUNCTIONS, KEYWORDS, VARIABLES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_resolves_builtins() {
        assert!(function("ta.ema").is_some());
        assert!(variable("close").is_some());
        assert_eq!(FUNCTIONS.len(), 457);
    }
}
