//! `pine-check` — semantic analysis for Pine v6.
//!
//! P3 ports the TS `UnifiedPineValidator` to the tree-sitter CST. This first
//! increment lands the two cheapest, lowest-false-positive checks (version
//! directive, unused user variables); argument/arity and type-coercion checks
//! follow, each validated against the TradingView oracle via the differential
//! harness. Diagnostics are byte-ranged and LSP-free so `pine-cli` and
//! `pine-lsp` can both consume them.

use pine_core::symbols::{self, SymbolKind};
use pine_core::Document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub start_byte: usize,
    pub end_byte: usize,
    pub severity: Severity,
    /// Stable machine-readable code, e.g. `"unused-variable"`.
    pub code: &'static str,
    pub message: String,
}

/// Run all semantic checks over a parsed document, sorted by position.
pub fn analyze(doc: &Document) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_version_directive(doc, &mut out);
    check_unused_variables(doc, &mut out);
    out.sort_by_key(|d| (d.start_byte, d.end_byte));
    out
}

/// Pine requires a `//@version=N` directive; v6 tooling expects v6.
fn check_version_directive(doc: &Document, out: &mut Vec<Diagnostic>) {
    let text = doc.text();
    if text.contains("@version=") {
        return;
    }
    let end = text.find('\n').unwrap_or(text.len());
    out.push(Diagnostic {
        start_byte: 0,
        end_byte: end,
        severity: Severity::Warning,
        code: "missing-version",
        message: "Missing `//@version=6` directive".to_string(),
    });
}

/// Top-level user variables that are declared but never referenced.
///
/// Unlike the TS checker (which had a known bug flagging builtins as unused),
/// this only considers user definitions returned by `symbols::definitions`, so
/// builtins can never be flagged.
fn check_unused_variables(doc: &Document, out: &mut Vec<Diagnostic>) {
    for def in symbols::definitions(doc) {
        if def.kind != SymbolKind::Variable || def.name == "_" {
            continue;
        }
        // references() returns every same-name identifier including the def;
        // exactly one occurrence means it's never used.
        if symbols::references(doc, &def.name).len() <= 1 {
            out.push(Diagnostic {
                start_byte: def.start_byte,
                end_byte: def.end_byte,
                severity: Severity::Warning,
                code: "unused-variable",
                message: format!("Variable `{}` is declared but never used", def.name),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::parse(s).unwrap()
    }

    #[test]
    fn flags_missing_version() {
        let d = doc("x = 1\nplot(x)\n");
        assert!(analyze(&d).iter().any(|x| x.code == "missing-version"));
    }

    #[test]
    fn version_present_ok() {
        let d = doc("//@version=6\nx = 1\nplot(x)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "missing-version"));
    }

    #[test]
    fn flags_unused_variable() {
        let d = doc("//@version=6\nunused = 42\nplot(close)\n");
        assert!(analyze(&d)
            .iter()
            .any(|x| x.code == "unused-variable" && x.message.contains("unused")));
    }

    #[test]
    fn used_variable_not_flagged() {
        let d = doc("//@version=6\nlen = 14\nplot(ta.sma(close, len))\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "unused-variable"));
    }

    #[test]
    fn builtins_never_flagged_unused() {
        let d = doc("//@version=6\nplot(close)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "unused-variable"));
    }
}
