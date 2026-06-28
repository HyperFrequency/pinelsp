//! Logic-lint: heuristic best-practice warnings ported from the zelosleone
//! analyzer (repainting/lookahead, future-leak, ...). These are NOT compile
//! errors — TradingView doesn't report them — but they catch the correctness
//! pitfalls that make Pine strategies look better in hindsight than live.

use crate::{dotted, Diagnostic, Severity};
use pine_core::Document;
use std::collections::HashSet;
use tree_sitter::Node;

pub(crate) fn check(doc: &Document, out: &mut Vec<Diagnostic>) {
    walk(doc.root(), doc.text(), out);
    check_strategy_orders(doc, out);
}

fn walk(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    match node.kind() {
        "call" => {
            check_request_security_lookahead(node, src, out);
            check_redundant_na(node, src, out);
        }
        "subscript" => check_negative_history(node, src, out),
        "reassignment" => check_self_assignment(node, src, out),
        "function_declaration_statement" => check_duplicate_params(node, src, out),
        "if_statement" | "for_statement" | "for_in_statement" | "while_statement" => {
            check_ta_stateful_in_conditional(node, src, out)
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, out);
    }
}

/// `ta.*` accumulator / moving-average / lookback functions whose internal state
/// depends on being evaluated on *every* bar. Calling them only inside an `if`/
/// loop body produces a different series than calling them unconditionally — the
/// classic Pine series-consistency pitfall. The cross/rising/falling family is
/// deliberately EXCLUDED: those are idiomatically (and safely) used as signal
/// conditions, so flagging them would be a major false-positive source.
const STATEFUL_TA: &[&str] = &[
    "ta.sma",
    "ta.ema",
    "ta.rma",
    "ta.wma",
    "ta.vwma",
    "ta.hma",
    "ta.alma",
    "ta.swma",
    "ta.linreg",
    "ta.rsi",
    "ta.atr",
    "ta.tr",
    "ta.cci",
    "ta.mfi",
    "ta.cmo",
    "ta.cog",
    "ta.mom",
    "ta.roc",
    "ta.stdev",
    "ta.dev",
    "ta.variance",
    "ta.highest",
    "ta.lowest",
    "ta.highestbars",
    "ta.lowestbars",
    "ta.barssince",
    "ta.valuewhen",
    "ta.cum",
    "ta.change",
    "ta.percentrank",
    "ta.median",
    "ta.mode",
    "ta.range",
    "ta.bb",
    "ta.bbw",
    "ta.kc",
    "ta.kcw",
    "ta.macd",
    "ta.dmi",
    "ta.sar",
    "ta.supertrend",
    "ta.wpr",
    "ta.correlation",
];

/// Scan the *immediate* body/consequence block of a conditional (`if`/`for`/
/// `while`) for direct stateful `ta.*` calls and warn on each. Only the body is
/// scanned — never the condition (calling a stateful `ta.*` inside the condition
/// is every-bar-safe), and the scan does NOT descend into nested
/// `if`/`for`/`while` blocks: each nested conditional's own visit owns its
/// direct calls, so a call is reported exactly once at its innermost enclosing
/// conditional.
fn check_ta_stateful_in_conditional(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    // if_statement exposes the body under `consequence`; loops under `body`.
    let body = node
        .child_by_field_name("consequence")
        .or_else(|| node.child_by_field_name("body"));
    let Some(body) = body else {
        return;
    };
    collect_direct_stateful_ta(body, src, out);
}

/// Recurse through `node` collecting stateful `ta.*` calls, but stop descending
/// at any nested conditional (its own visit will report its calls).
fn collect_direct_stateful_ta(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            if let Some(name) = dotted(func, src) {
                if STATEFUL_TA.contains(&name.as_str()) {
                    out.push(Diagnostic {
                        start_byte: node.start_byte(),
                        end_byte: node.end_byte(),
                        severity: Severity::Warning,
                        code: "ta-conditional",
                        message: format!(
                            "`{name}` is stateful and should be called on every bar; \
                             calling it only inside a conditional can make its series inconsistent"
                        ),
                    });
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // A nested conditional owns its own direct calls; don't double-report.
        if matches!(
            child.kind(),
            "if_statement" | "for_statement" | "for_in_statement" | "while_statement"
        ) {
            continue;
        }
        collect_direct_stateful_ta(child, src, out);
    }
}

/// `x := x` is a no-op: assigning a variable to itself with the plain `:=`
/// operator changes nothing. TradingView accepts the syntax (it is not a compile
/// error), so this is a Warning, not an Error. False-positive discipline:
///  - only the plain `:=` operator fires; compound ops (`+=`, `-=`, `*=`, `/=`,
///    `%=`) are semantically meaningful (`x += x` doubles `x`) and never fire;
///  - both sides must be a *bare* `identifier` with byte-equal text, so attribute
///    LHS like `foo.bar := foo.bar` (which could have property side effects) and
///    any non-identifier RHS (`x := x + 1`) are excluded. Net false-positives ~0.
fn check_self_assignment(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let Some(operator) = node.child_by_field_name("operator") else {
        return;
    };
    // Only the plain assignment is a no-op; compound assignments mutate `x`.
    if node_text(operator, src) != ":=" {
        return;
    }
    let (Some(variable), Some(value)) = (
        node.child_by_field_name("variable"),
        node.child_by_field_name("value"),
    ) else {
        return;
    };
    // Restrict to bare identifier <-> identifier to keep false-positives at zero.
    if variable.kind() != "identifier" || value.kind() != "identifier" {
        return;
    }
    let name = node_text(variable, src);
    if name != node_text(value, src) {
        return;
    }
    out.push(Diagnostic {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        severity: Severity::Warning,
        code: "self-assignment",
        message: format!("Self-assignment `{name} := {name}` has no effect"),
    });
}

/// Two parameters with the same name in a single function/method definition is
/// a genuine Pine error (TradingView rejects it), so this is Error severity.
/// The grammar surfaces each parameter as one `argument:` field that is always a
/// bare `(identifier)` — even with type annotations, qualifiers, and defaults
/// (those are separate sibling fields), so reading only `children_by_field_name
/// ("argument")` extracts the name unambiguously. The `HashSet` is local to this
/// call, so the same name reused in two *different* function definitions never
/// false-positives (each `function_declaration_statement` visit owns its own
/// set). Each duplicate is reported once, at the redeclaring occurrence (not the
/// first). Works for both `function:` and `method:` declarations since both use
/// the same `argument:` fields. Robust under parse errors: a malformed function
/// simply yields no/fewer `argument` fields and emits nothing.
fn check_duplicate_params(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let mut cursor = node.walk();
    let mut seen: HashSet<&str> = HashSet::new();
    for arg in node.children_by_field_name("argument", &mut cursor) {
        // The `argument` field is always a bare identifier; guard anyway so a
        // future grammar change can't cause a misnamed report.
        if arg.kind() != "identifier" {
            continue;
        }
        let name = node_text(arg, src);
        if !seen.insert(name) {
            out.push(Diagnostic {
                start_byte: arg.start_byte(),
                end_byte: arg.end_byte(),
                severity: Severity::Error,
                code: "duplicate-parameter",
                message: format!("Duplicate parameter `{name}` in function definition"),
            });
        }
    }
}

/// `na(na(x))` is redundant: `na()` already returns a bool, so wrapping it in
/// another `na()` is a pure no-op. TradingView accepts it (not a compile error),
/// so this is a Warning. `na` is a reserved builtin and cannot be shadowed by a
/// user function, so matching on the name `na` is unambiguous. False-positive
/// discipline: fires only when the OUTER call's function is exactly `na` AND the
/// FIRST POSITIONAL (named) child of the argument list is itself a `call` whose
/// function is exactly `na`. `nz(na(close))` does not match (outer is `nz`); a
/// keyword-argument first child (e.g. a hypothetical `na(x=...)`) is skipped
/// because only a bare positional `call` qualifies. The diagnostic spans the
/// outer call so the fix (drop one `na`) is clear.
fn check_redundant_na(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    if dotted(func, src).as_deref() != Some("na") {
        return;
    }
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    let Some(first) = args.named_children(&mut cursor).next() else {
        return;
    };
    // Must be a bare positional inner `call` named `na`. A keyword_argument or
    // any non-call first child does not qualify.
    if first.kind() != "call" {
        return;
    }
    let Some(inner_func) = first.child_by_field_name("function") else {
        return;
    };
    if dotted(inner_func, src).as_deref() != Some("na") {
        return;
    }
    out.push(Diagnostic {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        severity: Severity::Warning,
        code: "redundant-na",
        message: "`na(na(x))` is redundant; the inner `na()` already returns a bool".to_string(),
    });
}

/// A `strategy(...)` script that never places an order is almost certainly
/// incomplete. Document-wide advisory (Info): TradingView itself does NOT reject
/// such a script, and a library / partial template legitimately has no entries —
/// so per the false-positive-discipline rule this is non-blocking. A document-
/// wide scan (not scope-limited) means entries wrapped in user functions or
/// guarded by `if` are still found, keeping false positives near zero. Skipped
/// entirely when the file has any `import` (an order could come from an imported
/// library, which a single-document scan cannot see).
fn check_strategy_orders(doc: &Document, out: &mut Vec<Diagnostic>) {
    let src = doc.text();
    let mut scan = StrategyScan::default();
    scan_strategy(doc.root(), src, &mut scan);

    // An order (or exit) could come from an imported library that a
    // single-document scan cannot see, so both checks are suppressed when the
    // file has any `import`.
    if scan.has_import {
        return;
    }
    let Some(decl) = scan.strategy_decl else {
        return;
    };

    if !scan.has_order {
        out.push(Diagnostic {
            start_byte: decl.start_byte(),
            end_byte: decl.end_byte(),
            severity: Severity::Info,
            code: "strategy-no-orders",
            message: "`strategy()` script never places an order \
                      (no strategy.entry/order/exit/close/close_all call found)"
                .to_string(),
        });
        return;
    }

    // Entry placed but nothing ever closes the position. Fire only with EXACTLY
    // ONE entry call: two-or-more entries is the valid reverse-exit pattern
    // (a long is closed by opening an opposite short), which must not be flagged.
    // Info severity (advisory) per false-positive discipline: TradingView does
    // not reject this and residual cases (e.g. account-level closing) stay
    // non-blocking.
    if scan.entry_count == 1 && !scan.has_exit {
        out.push(Diagnostic {
            start_byte: decl.start_byte(),
            end_byte: decl.end_byte(),
            severity: Severity::Info,
            code: "strategy-no-exit",
            message: "`strategy.entry` has no matching exit \
                      (no strategy.exit/close/close_all call found); \
                      positions may never be closed"
                .to_string(),
        });
    }
}

const ORDER_CALLS: &[&str] = &[
    "strategy.entry",
    "strategy.order",
    "strategy.exit",
    "strategy.close",
    "strategy.close_all",
];

/// Calls that open a position. `strategy.order` is the low-level primitive and
/// `strategy.entry` the high-level one; both count as entries for the
/// reverse-exit suppression heuristic.
const ENTRY_CALLS: &[&str] = &["strategy.entry", "strategy.order"];

/// Calls that close / reduce a position.
const EXIT_CALLS: &[&str] = &["strategy.exit", "strategy.close", "strategy.close_all"];

#[derive(Default)]
struct StrategyScan<'a> {
    strategy_decl: Option<Node<'a>>,
    has_order: bool,
    has_import: bool,
    entry_count: usize,
    has_exit: bool,
}

fn scan_strategy<'a>(node: Node<'a>, src: &str, scan: &mut StrategyScan<'a>) {
    if node.kind() == "import" {
        scan.has_import = true;
    }
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            if let Some(name) = dotted(func, src) {
                if name == "strategy" && scan.strategy_decl.is_none() && is_top_level_call(node) {
                    scan.strategy_decl = Some(node);
                }
                if ORDER_CALLS.contains(&name.as_str()) {
                    scan.has_order = true;
                }
                if ENTRY_CALLS.contains(&name.as_str()) {
                    scan.entry_count += 1;
                }
                if EXIT_CALLS.contains(&name.as_str()) {
                    scan.has_exit = true;
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_strategy(child, src, scan);
    }
}

/// True when a call is a top-level declaration, i.e. its CST ancestry is
/// `source_file` -> `simple_statement` -> `call`. The `strategy()` declaration
/// must be at the top level; a same-named member call nested elsewhere
/// shouldn't be mistaken for the declaration.
fn is_top_level_call(call: Node) -> bool {
    call.parent()
        .filter(|p| p.kind() == "simple_statement")
        .and_then(|p| p.parent())
        .is_some_and(|gp| gp.kind() == "source_file")
}

/// `request.security(..., lookahead=barmerge.lookahead_on)` repaints — the
/// signature Pine lookahead-bias pitfall.
fn check_request_security_lookahead(call: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let Some(func) = call.child_by_field_name("function") else {
        return;
    };
    let Some(name) = dotted(func, src) else {
        return;
    };
    if name != "request.security" && name != "request.security_lower_tf" {
        return;
    }
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };
    // `lookahead` is the 5th positional parameter (index 4) or a named arg.
    let Some(value) = find_arg(args, "lookahead", 4, src) else {
        return;
    };
    let val = dotted(value, src).unwrap_or_else(|| node_text(value, src).to_string());
    if val == "barmerge.lookahead_on" {
        out.push(Diagnostic {
            start_byte: value.start_byte(),
            end_byte: value.end_byte(),
            severity: Severity::Warning,
            code: "lookahead-bias",
            message:
                "request.security lookahead set to barmerge.lookahead_on; this can introduce lookahead bias"
                    .to_string(),
        });
    }
}

/// A negative history offset (`close[-1]`) references future bars. The grammar
/// is `subscript{series, offset}`; a negative offset parses as a `unary_operation`.
fn check_negative_history(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    let Some(offset) = node.child_by_field_name("offset") else {
        return;
    };
    if offset.kind() == "unary_operation" && node_text(offset, src).trim_start().starts_with('-') {
        out.push(Diagnostic {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            severity: Severity::Warning,
            code: "future-leak",
            message: "Negative history reference reads future bars (lookahead)".to_string(),
        });
    }
}

/// The value node of a named arg `name`, else the positional arg at `pos_index`.
fn find_arg<'a>(args: Node<'a>, name: &str, pos_index: usize, src: &str) -> Option<Node<'a>> {
    let mut cursor = args.walk();
    let mut positional = 0usize;
    let mut found = None;
    for child in args.named_children(&mut cursor) {
        if child.kind() == "keyword_argument" {
            if let Some(key) = child.child_by_field_name("key") {
                if &src[key.start_byte()..key.end_byte()] == name {
                    return child.child_by_field_name("value");
                }
            }
        } else {
            if positional == pos_index {
                found = Some(child);
            }
            positional += 1;
        }
    }
    found
}

fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    &src[node.start_byte()..node.end_byte()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::parse(s).unwrap()
    }

    #[test]
    fn flags_lookahead_on() {
        let d = doc("//@version=6\nindicator(\"x\")\nx = request.security(syminfo.tickerid, \"D\", close, lookahead=barmerge.lookahead_on)\nplot(x)\n");
        assert!(crate::analyze(&d).iter().any(|x| x.code == "lookahead-bias"));
    }

    #[test]
    fn no_warning_when_lookahead_off() {
        let d = doc("//@version=6\nindicator(\"x\")\nx = request.security(syminfo.tickerid, \"D\", close, lookahead=barmerge.lookahead_off)\nplot(x)\n");
        assert!(!crate::analyze(&d).iter().any(|x| x.code == "lookahead-bias"));
    }

    #[test]
    fn flags_negative_history() {
        let d = doc("//@version=6\nindicator(\"x\")\nplot(close[-1])\n");
        assert!(crate::analyze(&d).iter().any(|x| x.code == "future-leak"));
    }

    // --- strategy-without-entry (Info, document-wide) ---

    #[test]
    fn flags_strategy_without_orders() {
        let d = doc("//@version=6\nstrategy(\"S\")\nplot(close)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|x| x.code == "strategy-no-orders" && x.severity == Severity::Info));
    }

    #[test]
    fn flags_strategy_with_signal_var_but_no_order() {
        let d = doc("//@version=6\nstrategy(\"S\")\nlongCond = ta.crossover(close, open)\nplot(close)\n");
        assert!(crate::analyze(&d).iter().any(|x| x.code == "strategy-no-orders"));
    }

    #[test]
    fn strategy_with_guarded_entry_not_flagged() {
        // entry present, even though guarded by an `if`
        let d = doc("//@version=6\nstrategy(\"S\")\nif close > open\n    strategy.entry(\"L\", strategy.long)\n");
        assert!(!crate::analyze(&d).iter().any(|x| x.code == "strategy-no-orders"));
    }

    #[test]
    fn indicator_never_flagged_strategy_no_orders() {
        let d = doc("//@version=6\nindicator(\"x\")\nplot(close)\n");
        assert!(!crate::analyze(&d).iter().any(|x| x.code == "strategy-no-orders"));
    }

    // --- ta-stateful-in-conditional (Warning) ---

    #[test]
    fn flags_ta_rsi_in_if_body() {
        let d = doc("//@version=6\nindicator(\"x\")\nif close > open\n    myrsi = ta.rsi(close, 14)\n    plot(myrsi)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|x| x.code == "ta-conditional" && x.message.contains("ta.rsi")));
    }

    #[test]
    fn flags_ta_sma_in_for_body() {
        let d = doc("//@version=6\nindicator(\"x\")\nfor i = 0 to 5\n    s = ta.sma(close, 14)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|x| x.code == "ta-conditional" && x.message.contains("ta.sma")));
    }

    #[test]
    fn ta_rsi_unconditional_not_flagged() {
        let d = doc("//@version=6\nindicator(\"x\")\nmyrsi = ta.rsi(close, 14)\nplot(close > open ? myrsi : na)\n");
        assert!(!crate::analyze(&d).iter().any(|x| x.code == "ta-conditional"));
    }

    #[test]
    fn ta_crossover_in_condition_not_flagged() {
        // ta.crossover is excluded from the allowlist AND it sits in the
        // condition, not the body.
        let d = doc("//@version=6\nindicator(\"x\")\nif ta.crossover(close, open)\n    plot(close)\n");
        assert!(!crate::analyze(&d).iter().any(|x| x.code == "ta-conditional"));
    }

    #[test]
    fn nested_conditional_ta_reported_once() {
        // ta.rsi sits in an inner `if` nested inside an outer `if`; it must be
        // reported exactly once (owned by the inner conditional's visit).
        let d = doc(
            "//@version=6\nindicator(\"x\")\nif close > open\n    if high > low\n        r = ta.rsi(close, 14)\n        plot(r)\n",
        );
        let hits = crate::analyze(&d)
            .iter()
            .filter(|x| x.code == "ta-conditional")
            .count();
        assert_eq!(hits, 1, "ta.rsi in nested if should be reported exactly once");
    }

    // --- self-assignment (Warning) ---

    #[test]
    fn flags_self_assignment() {
        let d = doc("//@version=6\nindicator(\"x\")\nx = 1\nx := x\nplot(x)\n");
        assert!(crate::analyze(&d).iter().any(|diag| diag.code == "self-assignment"
            && diag.severity == Severity::Warning
            && diag.message.contains("x := x")));
    }

    #[test]
    fn flags_self_assignment_other_name() {
        let d = doc("//@version=6\nindicator(\"x\")\nlen = 14\nlen := len\nplot(close)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "self-assignment"));
    }

    #[test]
    fn compound_self_assignment_not_flagged() {
        // `x += x` doubles x — semantically meaningful, not a no-op.
        let d = doc("//@version=6\nindicator(\"x\")\nx = 1\nx += x\nplot(x)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "self-assignment"));
    }

    #[test]
    fn self_assignment_with_expr_value_not_flagged() {
        // RHS is a math_operation, not a bare identifier.
        let d = doc("//@version=6\nindicator(\"x\")\nx = 1\nx := x + 1\nplot(x)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "self-assignment"));
    }

    // --- strategy-no-exit (Info) ---

    #[test]
    fn flags_strategy_no_exit_guarded_entry() {
        let d = doc(
            "//@version=6\nstrategy(\"S\")\nif close > open\n    strategy.entry(\"L\", strategy.long)\n",
        );
        assert!(crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit" && diag.severity == Severity::Info));
    }

    #[test]
    fn flags_strategy_no_exit_top_level_entry() {
        let d =
            doc("//@version=6\nstrategy(\"S\")\nstrategy.entry(\"L\", strategy.long)\nplot(close)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit" && diag.severity == Severity::Info));
    }

    #[test]
    fn strategy_with_exit_not_flagged_no_exit() {
        let d = doc("//@version=6\nstrategy(\"S\")\nstrategy.entry(\"L\", strategy.long)\nstrategy.exit(\"X\", \"L\", stop=low)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit"));
    }

    #[test]
    fn two_entries_reverse_exit_not_flagged() {
        // Two entries = reverse-exit pattern (close long by opening short).
        let d = doc("//@version=6\nstrategy(\"S\")\nstrategy.entry(\"L\", strategy.long)\nstrategy.entry(\"S\", strategy.short)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit"));
    }

    #[test]
    fn indicator_never_flagged_strategy_no_exit() {
        let d = doc("//@version=6\nindicator(\"x\")\nplot(close)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit"));
    }

    #[test]
    fn strategy_with_import_not_flagged_no_exit() {
        // Exit could live in the imported library; suppressed by import guard.
        let d = doc("//@version=6\nimport Foo/Bar/1 as lib\nstrategy(\"S\")\nstrategy.entry(\"L\", strategy.long)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "strategy-no-exit"));
    }

    // --- duplicate-parameter (Error) ---

    #[test]
    fn flags_duplicate_parameter() {
        let d = doc("//@version=6\nf(a, b, a) =>\n    a + b\nplot(f(1,2,3))\n");
        assert!(crate::analyze(&d).iter().any(|diag| diag.code == "duplicate-parameter"
            && diag.severity == Severity::Error
            && diag.message.contains('a')));
    }

    #[test]
    fn flags_duplicate_parameter_typed_defaulted_qualified() {
        // type/qualifier/default siblings must not hide the bare identifier name.
        let d = doc("//@version=6\nf(int a, float b = 1.0, simple int a) =>\n    a + b\nplot(f(1))\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "duplicate-parameter" && diag.message.contains('a')));
    }

    #[test]
    fn distinct_parameters_not_flagged() {
        let d = doc("//@version=6\nf(a, b, c) =>\n    a + b + c\nplot(f(1,2,3))\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "duplicate-parameter"));
    }

    #[test]
    fn same_name_in_two_functions_not_flagged() {
        // Same param name across two separate definitions is valid; the HashSet
        // is per-function, so this must not false-positive.
        let d = doc("//@version=6\nf(a) =>\n    a\ng(a) =>\n    a\nplot(f(1)+g(2))\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "duplicate-parameter"));
    }

    // --- redundant-na (Warning) ---

    #[test]
    fn flags_redundant_na() {
        let d = doc("//@version=6\nx = na(na(close))\nplot(x ? 1 : 0)\n");
        assert!(crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "redundant-na" && diag.severity == Severity::Warning));
    }

    #[test]
    fn single_na_not_flagged() {
        let d = doc("//@version=6\nx = na(close)\nplot(x ? 1 : 0)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "redundant-na"));
    }

    #[test]
    fn nz_wrapping_na_not_flagged() {
        // Outer fn is `nz`, not `na` — legitimate nesting, must not fire.
        let d = doc("//@version=6\nx = nz(na(close))\nplot(x ? 1 : 0)\n");
        assert!(!crate::analyze(&d)
            .iter()
            .any(|diag| diag.code == "redundant-na"));
    }
}
