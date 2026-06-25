//! Logic-lint: heuristic best-practice warnings ported from the zelosleone
//! analyzer (repainting/lookahead, future-leak, ...). These are NOT compile
//! errors — TradingView doesn't report them — but they catch the correctness
//! pitfalls that make Pine strategies look better in hindsight than live.

use crate::{dotted, Diagnostic, Severity};
use pine_core::Document;
use tree_sitter::Node;

pub(crate) fn check(doc: &Document, out: &mut Vec<Diagnostic>) {
    walk(doc.root(), doc.text(), out);
}

fn walk(node: Node, src: &str, out: &mut Vec<Diagnostic>) {
    match node.kind() {
        "call" => check_request_security_lookahead(node, src, out),
        "subscript" => check_negative_history(node, src, out),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, out);
    }
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
}
