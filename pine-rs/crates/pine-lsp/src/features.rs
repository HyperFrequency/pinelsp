//! P2 LSP feature logic as pure functions over a parsed [`Document`], returning
//! `lsp_types` directly so they can be unit-tested without spinning up the async
//! Backend. Semantic diagnostics (the ported checker) arrive in P3; here
//! diagnostics are tree-sitter syntax errors only.

use pine_core::builtins;
use pine_core::Document;
use tower_lsp::lsp_types::*;
use tree_sitter::Node;

/// Diagnostics from tree-sitter ERROR / MISSING nodes.
pub fn syntax_diagnostics(doc: &Document) -> Vec<Diagnostic> {
    let mut nodes = Vec::new();
    collect_errors(doc.root(), &mut nodes);
    nodes
        .into_iter()
        .map(|node| {
            let (start_line, start_col) = doc.position_at(node.start_byte());
            let (end_line, end_col) = doc.position_at(node.end_byte());
            let message = if node.is_missing() {
                format!("Syntax error: missing `{}`", node.kind())
            } else {
                "Syntax error".to_string()
            };
            Diagnostic {
                range: Range {
                    start: Position::new(start_line, start_col),
                    end: Position::new(end_line, end_col),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("pine-lsp".into()),
                message,
                ..Default::default()
            }
        })
        .collect()
}

/// Collect ERROR/MISSING nodes, pruning subtrees that parsed cleanly.
fn collect_errors<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    if node.is_error() || node.is_missing() {
        out.push(node);
        return;
    }
    if !node.has_error() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, out);
    }
}

/// Hover for the builtin (function / variable / constant) under the cursor.
pub fn hover_at(doc: &Document, pos: Position) -> Option<Hover> {
    let byte = doc.offset_at(pos.line, pos.character);
    let word = word_at(doc.text(), byte)?;
    let value = builtin_doc(word)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

fn builtin_doc(word: &str) -> Option<String> {
    if let Some(f) = builtins::function(word) {
        let head = if f.syntax.is_empty() { &f.name } else { &f.syntax };
        let mut s = format!("```pine\n{head}\n```");
        if !f.description.is_empty() {
            s.push_str("\n\n");
            s.push_str(&f.description);
        }
        return Some(s);
    }
    if let Some(v) = builtins::variable(word) {
        let mut s = format!("```pine\n{}: {}\n```", v.name, v.ty);
        if !v.description.is_empty() {
            s.push_str("\n\n");
            s.push_str(&v.description);
        }
        return Some(s);
    }
    if let Some(c) = builtins::constant(word) {
        let mut s = format!("```pine\n{}: {}\n```", c.name, c.ty);
        if let Some(d) = &c.description {
            if !d.is_empty() {
                s.push_str("\n\n");
                s.push_str(d);
            }
        }
        return Some(s);
    }
    None
}

/// Completions: members of `namespace.` after a dot, otherwise all top-level
/// builtins + keywords.
pub fn completions_at(doc: &Document, pos: Position) -> Vec<CompletionItem> {
    let byte = doc.offset_at(pos.line, pos.character);
    let run = trailing_run(doc.text(), byte);
    match run.rfind('.') {
        Some(dot) => namespace_members(&run[..dot]),
        None => top_level_items(),
    }
}

fn namespace_members(ns: &str) -> Vec<CompletionItem> {
    let prefix = format!("{ns}.");
    let mut items = Vec::new();
    let direct = |full: &str| -> Option<String> {
        full.strip_prefix(&prefix)
            .filter(|m| !m.contains('.'))
            .map(|m| m.to_string())
    };
    for f in builtins::FUNCTIONS.iter() {
        if let Some(member) = direct(&f.name) {
            items.push(item(member, CompletionItemKind::FUNCTION, fn_detail(f)));
        }
    }
    for v in builtins::VARIABLES.iter() {
        if let Some(member) = direct(&v.name) {
            items.push(item(member, CompletionItemKind::VARIABLE, v.ty.clone()));
        }
    }
    for c in builtins::CONSTANTS.iter() {
        if let Some(member) = direct(&c.name) {
            items.push(item(member, CompletionItemKind::CONSTANT, c.ty.clone()));
        }
    }
    items
}

fn top_level_items() -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for f in builtins::FUNCTIONS.iter() {
        items.push(item(f.name.clone(), CompletionItemKind::FUNCTION, fn_detail(f)));
    }
    for v in builtins::VARIABLES.iter() {
        items.push(item(v.name.clone(), CompletionItemKind::VARIABLE, v.ty.clone()));
    }
    for c in builtins::CONSTANTS.iter() {
        items.push(item(c.name.clone(), CompletionItemKind::CONSTANT, c.ty.clone()));
    }
    for k in builtins::KEYWORDS.all.iter() {
        items.push(CompletionItem {
            label: k.clone(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }
    items
}

fn item(label: String, kind: CompletionItemKind, detail: String) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail: (!detail.is_empty()).then_some(detail),
        ..Default::default()
    }
}

fn fn_detail(f: &builtins::BuiltinFunction) -> String {
    if f.syntax.is_empty() {
        f.returns.clone()
    } else {
        f.syntax.clone()
    }
}

/// The word (possibly dotted, e.g. `ta.sma`) surrounding `byte`. ASCII word
/// chars + `.`; trims stray leading/trailing dots.
fn word_at(text: &str, byte: usize) -> Option<&str> {
    if byte > text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let mut start = byte;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = byte;
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    let word = text.get(start..end)?.trim_matches('.');
    (!word.is_empty()).then_some(word)
}

/// The run of word chars (incl. `.`) immediately *before* `byte` — used to
/// detect a `namespace.` member-access context.
fn trailing_run(text: &str, byte: usize) -> &str {
    let byte = byte.min(text.len());
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let mut start = byte;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    &text[start..byte]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::parse(s).unwrap()
    }

    #[test]
    fn syntax_errors_reported() {
        let d = doc("//@version=6\nx = (1 + \n");
        let diags = syntax_diagnostics(&d);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn clean_source_has_no_diags() {
        let d = doc("//@version=6\nindicator(\"x\")\nplot(close)\n");
        assert!(syntax_diagnostics(&d).is_empty());
    }

    #[test]
    fn hover_on_builtin_variable() {
        let d = doc("//@version=6\nplot(close)\n");
        // "close" begins at line 1, char 5
        let h = hover_at(&d, Position::new(1, 5));
        assert!(h.is_some(), "expected hover on `close`");
    }

    #[test]
    fn completion_after_dot_lists_namespace_members() {
        let d = doc("//@version=6\nx = ta.\n");
        // cursor right after "ta." → line 1, char 7
        let items = completions_at(&d, Position::new(1, 7));
        assert!(items.iter().any(|i| i.label == "sma"), "expected ta.sma");
        assert!(items.iter().all(|i| !i.label.contains('.')), "members are bare");
    }

    #[test]
    fn completion_top_level_includes_builtins_and_keywords() {
        let d = doc("//@version=6\n\n");
        let items = completions_at(&d, Position::new(1, 0));
        assert!(items.iter().any(|i| i.label == "close"));
        assert!(items.iter().any(|i| i.label == "if"));
        assert!(items.len() > 400);
    }
}
