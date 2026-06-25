//! `pine-check` — semantic analysis for Pine v6.
//!
//! P3 ports the TS `UnifiedPineValidator` to the tree-sitter CST. This first
//! increment lands the two cheapest, lowest-false-positive checks (version
//! directive, unused user variables); argument/arity and type-coercion checks
//! follow, each validated against the TradingView oracle via the differential
//! harness. Diagnostics are byte-ranged and LSP-free so `pine-cli` and
//! `pine-lsp` can both consume them.

use std::collections::HashSet;

use pine_core::symbols::{self, SymbolKind};
use pine_core::{builtins, Document};
use tree_sitter::Node;

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
    check_undefined_identifiers(doc, &mut out);
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

/// Identifiers used as references that resolve to nothing known — not a user
/// symbol, builtin, keyword, or namespace. Mirrors TradingView's "Undeclared
/// identifier" (CE10272).
///
/// Conservative by construction: every *definition* position (variables,
/// parameters, function/type/enum names, tuple + for-in vars) is already in the
/// user-symbol set, so the only identifiers that need skipping are member names
/// (`obj.MEMBER`) and keyword-argument keys (`name=...`), which are not in any
/// valid set and would otherwise be false positives.
fn check_undefined_identifiers(doc: &Document, out: &mut Vec<Diagnostic>) {
    let user: HashSet<String> = symbols::definitions(doc)
        .into_iter()
        .map(|d| d.name)
        .collect();
    let namespaces = builtin_namespaces();
    walk_idents(doc.root(), doc.text(), &user, &namespaces, out);
}

/// Leading segment of every dotted builtin name (e.g. `ta`, `request`, `array`).
fn builtin_namespaces() -> HashSet<&'static str> {
    let mut ns = HashSet::new();
    let names = builtins::FUNCTIONS
        .iter()
        .map(|f| f.name.as_str())
        .chain(builtins::VARIABLES.iter().map(|v| v.name.as_str()))
        .chain(builtins::CONSTANTS.iter().map(|c| c.name.as_str()));
    for name in names {
        if let Some((head, _)) = name.split_once('.') {
            ns.insert(head);
        }
    }
    ns
}

fn walk_idents(
    node: Node,
    src: &str,
    user: &HashSet<String>,
    namespaces: &HashSet<&'static str>,
    out: &mut Vec<Diagnostic>,
) {
    if node.kind() == "identifier" && is_checkable_reference(node) {
        let name = &src[node.start_byte()..node.end_byte()];
        if !is_known(name, user, namespaces) {
            out.push(Diagnostic {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                severity: Severity::Error,
                code: "undeclared-identifier",
                message: format!("Undeclared identifier `{name}`"),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_idents(child, src, user, namespaces, out);
    }
}

/// An identifier is a checkable reference unless it's a member name (`obj.X`) or
/// a keyword-argument key (`X=...`).
fn is_checkable_reference(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    let is_field = |field: &str| parent.child_by_field_name(field).is_some_and(|c| c == node);
    match parent.kind() {
        "attribute" => !is_field("attribute"),
        "keyword_argument" => !is_field("key"),
        _ => true,
    }
}

fn is_known(name: &str, user: &HashSet<String>, namespaces: &HashSet<&'static str>) -> bool {
    matches!(name, "na" | "true" | "false")
        || user.contains(name)
        || namespaces.contains(name)
        || builtins::function(name).is_some()
        || builtins::variable(name).is_some()
        || builtins::constant(name).is_some()
        || builtins::is_keyword(name)
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

    #[test]
    fn flags_undefined_identifier() {
        let d = doc("//@version=6\nindicator(\"x\")\nplot(undefinedXYZ)\n");
        assert!(analyze(&d)
            .iter()
            .any(|x| x.code == "undeclared-identifier" && x.message.contains("undefinedXYZ")));
    }

    #[test]
    fn no_undeclared_false_positives_on_valid_script() {
        // namespaced calls, keyword args, params, tuple destructuring, builtins
        let src = "//@version=6\nindicator(\"x\")\n[a, b] = ta.macd(close, 12, 26)\nf(p) => p * 2\nval = f(a + b)\nplot(val, color=color.red, title=\"t\")\n";
        let d = doc(src);
        let fp: Vec<String> = analyze(&d)
            .into_iter()
            .filter(|x| x.code == "undeclared-identifier")
            .map(|x| x.message)
            .collect();
        assert!(fp.is_empty(), "unexpected undeclared-identifier diagnostics: {fp:?}");
    }
}
