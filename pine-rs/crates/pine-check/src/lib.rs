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
use pine_core::{Document, builtins};
use tree_sitter::Node;

mod logiclint;

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
    check_type_annotations(doc, &mut out);
    check_call_arguments(doc, &mut out);
    check_na_comparison(doc, &mut out);
    check_constant_condition(doc, &mut out);
    logiclint::check(doc, &mut out);
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
    // Identifier resolution is only reliable on a fully-parsed tree; on a file
    // with syntax errors the partial parse produces spurious bare identifiers.
    // Report the syntax error instead and skip undefined-id there.
    if doc.has_errors() {
        return;
    }
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
    // Don't resolve identifiers inside a parse error — they're unreliable and
    // produce spurious "undeclared" noise. The syntax error is reported instead.
    if node.is_error() {
        return;
    }
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
        // type definitions hold only the type name + field declarations, not
        // references (field access goes through `attribute` above).
        "type_definition_statement" => false,
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

/// Variable declarations whose initializer type clearly cannot be assigned to
/// the declared type (e.g. `int a = "hello"`). Conservative: only unambiguous
/// base-type violations are flagged; complex/generic types and unknown
/// inferences are skipped so false positives stay at zero.
fn check_type_annotations(doc: &Document, out: &mut Vec<Diagnostic>) {
    walk_type_checks(doc.root(), doc.text(), out);
}

fn walk_type_checks(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    if node.kind() == "variable_definition"
        && let (Some(ty), Some(init)) = (
            node.child_by_field_name("type"),
            node.child_by_field_name("initial_value"),
        )
        && let (Some(declared), Some(inferred)) = (base_type_name(ty, src), infer_type(init, src))
        && is_type_mismatch(&declared, &inferred)
    {
        out.push(Diagnostic {
            start_byte: init.start_byte(),
            end_byte: init.end_byte(),
            severity: Severity::Error,
            code: "type-mismatch",
            message: format!("Cannot assign `{inferred}` to `{declared}`"),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_type_checks(child, src, out);
    }
}

/// The base type name from a `base_type(identifier)` annotation; `None` for
/// generic/complex types (skipped).
fn base_type_name(ty: Node, src: &str) -> Option<String> {
    if ty.kind() != "base_type" {
        return None;
    }
    let id = ty.named_child(0)?;
    Some(src[id.start_byte()..id.end_byte()].to_string())
}

/// Infer the base type of a simple initializer expression; `None` (unknown) when
/// it can't be determined confidently.
fn infer_type(node: Node, src: &str) -> Option<String> {
    let t = match node.kind() {
        "integer" => "int",
        "float" => "float",
        "string" => "string",
        "true" | "false" => "bool",
        "identifier" => {
            let name = &src[node.start_byte()..node.end_byte()];
            return base_of(&builtins::variable(name)?.ty);
        }
        "call" => {
            let func = node.child_by_field_name("function")?;
            let fname = dotted(func, src)?;
            return base_of(&builtins::function(&fname)?.returns);
        }
        _ => return None,
    };
    Some(t.to_string())
}

pub(crate) fn dotted(node: Node, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(src[node.start_byte()..node.end_byte()].to_string()),
        "attribute" => {
            let obj = node.child_by_field_name("object")?;
            let attr = node.child_by_field_name("attribute")?;
            Some(format!(
                "{}.{}",
                dotted(obj, src)?,
                &src[attr.start_byte()..attr.end_byte()]
            ))
        }
        _ => None,
    }
}

/// Reduce a Pine type string (e.g. `"series float"`, `"input int"`,
/// `"array<float>"`) to its base; `None` for unions like `int/float`.
fn base_of(ty: &str) -> Option<String> {
    let last = ty.rsplit(' ').next().unwrap_or(ty);
    let base = last.split('<').next().unwrap_or(last);
    if base.is_empty() || base.contains('/') {
        return None;
    }
    Some(base.to_string())
}

/// Argument validation for calls to *builtin* functions: unknown named
/// arguments and too many positional arguments. Conservative: only builtins
/// (user functions skipped), and variadic/overloaded builtins are skipped since
/// their accepted arguments aren't a fixed set.
fn check_call_arguments(doc: &Document, out: &mut Vec<Diagnostic>) {
    if doc.has_errors() {
        return;
    }
    walk_calls(doc.root(), doc.text(), out);
}

fn walk_calls(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    if node.kind() == "call" {
        check_one_call(node, src, out);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, src, out);
    }
}

fn check_one_call(call: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let Some(func) = call.child_by_field_name("function") else {
        return;
    };
    let Some(name) = dotted(func, src) else {
        return;
    };
    let Some(f) = builtins::function(&name) else {
        return; // only builtins; user functions need signature inference (later)
    };

    let variadic = f.flags.as_ref().is_some_and(|fl| fl.variadic);
    // Overloaded builtins encode alternatives with `unknown`/empty param types;
    // their accepted argument set isn't fixed, so skip them.
    let overloaded = f
        .parameters
        .iter()
        .any(|p| p.ty == "unknown" || p.ty.is_empty());
    if overloaded {
        return;
    }

    let param_names: HashSet<&str> = f.parameters.iter().map(|p| p.name.as_str()).collect();
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };

    let mut positional = 0usize;
    let mut named_keys: HashSet<&str> = HashSet::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        if child.kind() == "keyword_argument" {
            if let Some(key) = child.child_by_field_name("key") {
                let key_name = &src[key.start_byte()..key.end_byte()];
                named_keys.insert(key_name);
                if !param_names.contains(key_name) {
                    out.push(Diagnostic {
                        start_byte: key.start_byte(),
                        end_byte: key.end_byte(),
                        severity: Severity::Error,
                        code: "unknown-argument",
                        message: format!("Unknown argument `{key_name}` for `{name}`"),
                    });
                }
            }
        } else {
            positional += 1;
        }
    }

    if !variadic && positional > f.parameters.len() {
        out.push(Diagnostic {
            start_byte: call.start_byte(),
            end_byte: call.end_byte(),
            severity: Severity::Error,
            code: "too-many-arguments",
            message: format!(
                "Too many arguments for `{name}`: expected {}, got {positional}",
                f.parameters.len()
            ),
        });
    }

    // Missing required arguments: a required param with no default, not covered
    // positionally or by name. Skipped for variadics (their arity isn't fixed).
    if !variadic {
        for (i, param) in f.parameters.iter().enumerate() {
            if param.required
                && param.default.is_none()
                && positional <= i
                && !named_keys.contains(param.name.as_str())
            {
                out.push(Diagnostic {
                    start_byte: call.start_byte(),
                    end_byte: call.end_byte(),
                    severity: Severity::Error,
                    code: "missing-argument",
                    message: format!("Missing required argument `{}` for `{name}`", param.name),
                });
            }
        }
    }
}

/// `x == na` / `x != na` always evaluate to na/false — a pitfall; use `na(x)`.
/// A warning (TradingView accepts the syntax), mirroring the TS checker.
fn check_na_comparison(doc: &Document, out: &mut Vec<Diagnostic>) {
    walk_na(doc.root(), doc.text(), out);
}

fn walk_na(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    if node.kind() == "comparison_operation" {
        let mut op_cursor = node.walk();
        let is_eq = node
            .children(&mut op_cursor)
            .any(|c| matches!(c.kind(), "==" | "!="));
        let is_na = |field: &str| {
            node.child_by_field_name(field).is_some_and(|n| {
                n.kind() == "identifier" && &src[n.start_byte()..n.end_byte()] == "na"
            })
        };
        if is_eq && (is_na("left") || is_na("right")) {
            // TradingView treats this as an error, not just a lint.
            out.push(Diagnostic {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                severity: Severity::Error,
                code: "na-comparison",
                message: "Cannot compare to `na` directly; use the `na()` function instead"
                    .to_string(),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_na(child, src, out);
    }
}

/// A bare boolean literal `true`/`false` used as the condition of an `if`
/// statement or a ternary `?:` makes the branch unconditional or dead. A
/// Warning (TradingView accepts the syntax). Restricted strictly to bare
/// literal `true`/`false` nodes — no constant folding of expressions — so it
/// can never fire on valid dynamic Pine. `while true` is deliberately NOT
/// flagged: it is a legitimate loop-with-break idiom in Pine.
fn check_constant_condition(doc: &Document, out: &mut Vec<Diagnostic>) {
    // The condition field is only reliable on a fully-parsed tree.
    if doc.has_errors() {
        return;
    }
    walk_constant_condition(doc.root(), out);
}

fn walk_constant_condition(node: Node, out: &mut Vec<Diagnostic>) {
    if matches!(node.kind(), "if_statement" | "conditional_expression")
        && let Some(cond) = node.child_by_field_name("condition")
    {
        let message = match cond.kind() {
            "true" => Some("Condition is always `true`, the branch is unconditional"),
            "false" => Some("Condition is always `false`, the branch is dead code"),
            _ => None,
        };
        if let Some(message) = message {
            out.push(Diagnostic {
                start_byte: cond.start_byte(),
                end_byte: cond.end_byte(),
                severity: Severity::Warning,
                code: "constant-condition",
                message: message.to_string(),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_constant_condition(child, out);
    }
}

fn is_type_mismatch(declared: &str, inferred: &str) -> bool {
    if declared == inferred {
        return false;
    }
    let numeric = |t: &str| t == "int" || t == "float";
    if numeric(declared) && numeric(inferred) {
        return false; // int <-> float coerces
    }
    matches!(
        (declared, inferred),
        ("int" | "float", "string")
            | ("bool", "string")
            | ("string", "bool")
            | ("int" | "float", "bool")
            | ("bool", "int" | "float")
    )
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
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "unused-variable" && x.message.contains("unused"))
        );
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
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "undeclared-identifier" && x.message.contains("undefinedXYZ"))
        );
    }

    #[test]
    fn flags_type_mismatch_string_to_int() {
        let d = doc("//@version=6\nint a = \"hello\"\nplot(a)\n");
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "type-mismatch" && x.message.contains("string"))
        );
    }

    #[test]
    fn int_float_coercion_not_flagged() {
        let d = doc("//@version=6\nfloat x = 1\nplot(x)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "type-mismatch"));
    }

    #[test]
    fn builtin_typed_initializer_ok() {
        // close is `series float`; assigning to `float` must not be flagged.
        let d = doc("//@version=6\nfloat c = close\nplot(c)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "type-mismatch"));
    }

    #[test]
    fn flags_unknown_named_argument() {
        let d = doc("//@version=6\nplot(close, notarealparam=1)\n");
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "unknown-argument" && x.message.contains("notarealparam"))
        );
    }

    #[test]
    fn valid_named_argument_not_flagged() {
        let d = doc("//@version=6\nplot(close, title=\"t\")\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "unknown-argument"));
    }

    #[test]
    fn flags_na_comparison() {
        let d = doc("//@version=6\nb = close == na\nplot(close)\n");
        assert!(analyze(&d).iter().any(|x| x.code == "na-comparison"));
    }

    #[test]
    fn na_function_call_not_flagged() {
        // na(x) is the correct form — must not be flagged.
        let d = doc("//@version=6\nb = na(close)\nplot(close)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "na-comparison"));
    }

    #[test]
    fn complete_call_has_no_missing_argument() {
        let d = doc("//@version=6\nplot(close)\nx = ta.sma(close, 14)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "missing-argument"));
    }

    #[test]
    fn switch_assignment_target_not_flagged() {
        // `t = switch ...` binds t via variable_definition_statement.
        let d = doc("//@version=6\nt = switch\n    close > open => 1\n    => 0\nplot(t)\n");
        assert!(
            !analyze(&d)
                .iter()
                .any(|x| x.code == "undeclared-identifier"),
            "switch-assigned var should be a definition"
        );
    }

    #[test]
    fn for_counter_not_flagged() {
        let d = doc("//@version=6\nsum = 0.0\nfor i = 0 to 5\n    sum := sum + i\nplot(sum)\n");
        let undeclared: Vec<_> = analyze(&d)
            .into_iter()
            .filter(|x| x.code == "undeclared-identifier")
            .map(|x| x.message)
            .collect();
        assert!(
            undeclared.is_empty(),
            "for-counter `i` should be defined: {undeclared:?}"
        );
    }

    #[test]
    fn undefined_id_skipped_on_syntax_error() {
        // Broken parse → undefined-id suppressed (syntax error reported instead).
        let d = doc("//@version=6\nx = (1 +\n");
        assert!(
            !analyze(&d)
                .iter()
                .any(|x| x.code == "undeclared-identifier")
        );
    }

    #[test]
    fn flags_constant_condition_if_true() {
        let d = doc("//@version=6\nindicator(\"x\")\nif true\n    plot(close)\n");
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "constant-condition" && x.message.contains("always `true`"))
        );
    }

    #[test]
    fn flags_constant_condition_ternary_false() {
        let d = doc("//@version=6\nx = false ? 1 : 2\nplot(x)\n");
        assert!(
            analyze(&d)
                .iter()
                .any(|x| x.code == "constant-condition" && x.message.contains("always `false`"))
        );
    }

    #[test]
    fn dynamic_bool_variable_condition_not_flagged() {
        let d = doc("//@version=6\nflag = close > open\nif flag\n    plot(close)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "constant-condition"));
    }

    #[test]
    fn dynamic_ternary_condition_not_flagged() {
        let d = doc("//@version=6\nx = close > open ? 1 : 2\nplot(x)\n");
        assert!(!analyze(&d).iter().any(|x| x.code == "constant-condition"));
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
        assert!(
            fp.is_empty(),
            "unexpected undeclared-identifier diagnostics: {fp:?}"
        );
    }
}
