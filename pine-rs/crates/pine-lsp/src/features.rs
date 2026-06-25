//! P2 LSP feature logic as pure functions over a parsed [`Document`], returning
//! `lsp_types` directly so they can be unit-tested without spinning up the async
//! Backend. Semantic diagnostics (the ported checker) arrive in P3; here
//! diagnostics are tree-sitter syntax errors only.

use std::collections::HashMap;

use pine_core::builtins;
use pine_core::symbols::{self, SymbolKind as DefKind};
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

/// Semantic diagnostics from `pine-check`, converted to LSP.
pub fn semantic_diagnostics(doc: &Document) -> Vec<Diagnostic> {
    pine_check::analyze(doc)
        .into_iter()
        .map(|d| Diagnostic {
            range: byte_range(doc, d.start_byte, d.end_byte),
            severity: Some(match d.severity {
                pine_check::Severity::Error => DiagnosticSeverity::ERROR,
                pine_check::Severity::Warning => DiagnosticSeverity::WARNING,
                pine_check::Severity::Info => DiagnosticSeverity::INFORMATION,
            }),
            code: Some(NumberOrString::String(d.code.to_string())),
            source: Some("pine-lsp".into()),
            message: d.message,
            ..Default::default()
        })
        .collect()
}

/// All diagnostics for a document: tree-sitter syntax errors + semantic checks.
pub fn all_diagnostics(doc: &Document) -> Vec<Diagnostic> {
    let mut diags = syntax_diagnostics(doc);
    diags.extend(semantic_diagnostics(doc));
    diags
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

/// Byte range -> LSP range via the document's UTF-16 line index.
fn byte_range(doc: &Document, start: usize, end: usize) -> Range {
    let (start_line, start_col) = doc.position_at(start);
    let (end_line, end_col) = doc.position_at(end);
    Range {
        start: Position::new(start_line, start_col),
        end: Position::new(end_line, end_col),
    }
}

/// Signature help for the builtin call enclosing the cursor.
pub fn signature_help(doc: &Document, pos: Position) -> Option<SignatureHelp> {
    let byte = doc.offset_at(pos.line, pos.character);
    let (name, active) = enclosing_call(doc, byte)?;
    let f = builtins::function(&name)?;
    let label = if f.syntax.is_empty() {
        f.name.clone()
    } else {
        f.syntax.clone()
    };
    let parameters: Vec<ParameterInformation> = f
        .parameters
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.name.clone()),
            documentation: (!p.description.is_empty())
                .then(|| Documentation::String(p.description.clone())),
        })
        .collect();
    let active_parameter = (!parameters.is_empty())
        .then(|| (active as u32).min(parameters.len() as u32 - 1));
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: (!f.description.is_empty())
                .then(|| Documentation::String(f.description.clone())),
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

fn enclosing_call(doc: &Document, byte: usize) -> Option<(String, usize)> {
    let mut node = doc.root().named_descendant_for_byte_range(byte, byte)?;
    while node.kind() != "call" {
        node = node.parent()?;
    }
    let func = node.child_by_field_name("function")?;
    let name = dotted_name(func, doc.text())?;
    let active = node
        .child_by_field_name("arguments")
        .map(|args| count_commas_before(args, byte))
        .unwrap_or(0);
    Some((name, active))
}

fn dotted_name(node: Node, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(src[node.start_byte()..node.end_byte()].to_string()),
        "attribute" => {
            let obj = node.child_by_field_name("object")?;
            let attr = node.child_by_field_name("attribute")?;
            Some(format!(
                "{}.{}",
                dotted_name(obj, src)?,
                &src[attr.start_byte()..attr.end_byte()]
            ))
        }
        _ => None,
    }
}

fn count_commas_before(arg_list: Node, byte: usize) -> usize {
    let mut cursor = arg_list.walk();
    arg_list
        .children(&mut cursor)
        .filter(|n| n.kind() == "," && n.start_byte() < byte)
        .count()
}

/// Top-level document symbols (functions, variables, types, enums).
pub fn document_symbols(doc: &Document) -> Vec<DocumentSymbol> {
    symbols::definitions(doc)
        .into_iter()
        .filter(|d| d.kind != DefKind::Parameter)
        .map(|d| {
            let range = byte_range(doc, d.start_byte, d.end_byte);
            #[allow(deprecated)]
            DocumentSymbol {
                name: d.name,
                detail: None,
                kind: to_symbol_kind(d.kind),
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            }
        })
        .collect()
}

fn to_symbol_kind(kind: DefKind) -> SymbolKind {
    match kind {
        DefKind::Function => SymbolKind::FUNCTION,
        DefKind::Variable | DefKind::Parameter => SymbolKind::VARIABLE,
        DefKind::Type => SymbolKind::STRUCT,
        DefKind::Enum => SymbolKind::ENUM,
    }
}

/// Go-to-definition for the user symbol under the cursor.
pub fn goto_definition(doc: &Document, pos: Position, uri: Url) -> Option<GotoDefinitionResponse> {
    let byte = doc.offset_at(pos.line, pos.character);
    let (name, _, _) = symbols::identifier_at(doc, byte)?;
    let def = symbols::definitions(doc).into_iter().find(|d| d.name == name)?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri,
        range: byte_range(doc, def.start_byte, def.end_byte),
    }))
}

/// All references to the user symbol under the cursor.
pub fn references(doc: &Document, pos: Position, uri: Url) -> Vec<Location> {
    let byte = doc.offset_at(pos.line, pos.character);
    let Some((name, _, _)) = symbols::identifier_at(doc, byte) else {
        return Vec::new();
    };
    symbols::references(doc, &name)
        .into_iter()
        .map(|(s, e)| Location {
            uri: uri.clone(),
            range: byte_range(doc, s, e),
        })
        .collect()
}

/// Rename the user symbol under the cursor (refuses builtins).
pub fn rename(doc: &Document, pos: Position, new_name: String, uri: Url) -> Option<WorkspaceEdit> {
    let byte = doc.offset_at(pos.line, pos.character);
    let (name, _, _) = symbols::identifier_at(doc, byte)?;
    if is_builtin(&name) {
        return None;
    }
    let edits: Vec<TextEdit> = symbols::references(doc, &name)
        .into_iter()
        .map(|(s, e)| TextEdit {
            range: byte_range(doc, s, e),
            new_text: new_name.clone(),
        })
        .collect();
    if edits.is_empty() {
        return None;
    }
    let mut changes = HashMap::new();
    changes.insert(uri, edits);
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// prepare-rename: allow only on user symbols (not builtins).
pub fn prepare_rename(doc: &Document, pos: Position) -> Option<PrepareRenameResponse> {
    let byte = doc.offset_at(pos.line, pos.character);
    let (name, s, e) = symbols::identifier_at(doc, byte)?;
    if is_builtin(&name) {
        return None;
    }
    Some(PrepareRenameResponse::Range(byte_range(doc, s, e)))
}

fn is_builtin(name: &str) -> bool {
    builtins::function(name).is_some()
        || builtins::variable(name).is_some()
        || builtins::constant(name).is_some()
}

const FOLDABLE_KINDS: &[&str] = &[
    "function_declaration_statement",
    "type_definition_statement",
    "enum_declaration",
    "if_statement",
    "for_statement",
    "for_in_statement",
    "switch_statement",
    "while_statement",
    "block",
];

/// Folding ranges for block-like constructs (functions, types, control flow).
pub fn folding_ranges(doc: &Document) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    collect_folds(doc.root(), &mut out);
    out
}

fn collect_folds(node: Node, out: &mut Vec<FoldingRange>) {
    let start = node.start_position();
    let end = node.end_position();
    if FOLDABLE_KINDS.contains(&node.kind()) && end.row > start.row {
        out.push(FoldingRange {
            start_line: start.row as u32,
            start_character: None,
            end_line: end.row as u32,
            end_character: None,
            kind: Some(FoldingRangeKind::Region),
            collapsed_text: None,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_folds(child, out);
    }
}

/// Inlay hints: parameter-name labels on the positional arguments of builtin
/// calls (e.g. `ta.sma(source: close, length: 14)`).
pub fn inlay_hints(doc: &Document) -> Vec<InlayHint> {
    let mut out = Vec::new();
    collect_inlays(doc.root(), doc, &mut out);
    out
}

fn collect_inlays(node: Node, doc: &Document, out: &mut Vec<InlayHint>) {
    if node.kind() == "call" {
        inlay_for_call(node, doc, out);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inlays(child, doc, out);
    }
}

fn inlay_for_call(call: Node, doc: &Document, out: &mut Vec<InlayHint>) {
    let Some(func) = call.child_by_field_name("function") else {
        return;
    };
    let Some(name) = dotted_name(func, doc.text()) else {
        return;
    };
    let Some(f) = builtins::function(&name) else {
        return; // builtins only; user-fn params need signature inference (later)
    };
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    let mut positional = 0usize;
    for child in args.named_children(&mut cursor) {
        if child.kind() == "keyword_argument" {
            continue; // already named at the call site
        }
        if let Some(param) = f.parameters.get(positional) {
            let (line, character) = doc.position_at(child.start_byte());
            out.push(InlayHint {
                position: Position::new(line, character),
                label: InlayHintLabel::String(format!("{}:", param.name)),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
        positional += 1;
    }
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

    #[test]
    fn signature_help_for_builtin_call() {
        let d = doc("//@version=6\nx = ta.sma(close, 14)\n");
        // cursor on the second arg "14" (line 1, char 18)
        let sh = signature_help(&d, Position::new(1, 18)).expect("sig help for ta.sma");
        assert_eq!(sh.signatures.len(), 1);
        assert!(!sh.signatures[0].label.is_empty());
        assert_eq!(sh.active_parameter, Some(1), "after one comma → param index 1");
    }

    #[test]
    fn document_symbols_lists_user_defs() {
        let d = doc("//@version=6\nlen = 14\nf(a) =>\n    a\n");
        let names: Vec<String> = document_symbols(&d).into_iter().map(|s| s.name).collect();
        assert!(names.iter().any(|n| n == "len"));
        assert!(names.iter().any(|n| n == "f"));
        assert!(!names.iter().any(|n| n == "a"), "params excluded from doc symbols");
    }

    #[test]
    fn definition_references_rename_roundtrip() {
        let src = "//@version=6\nlen = 14\nz = len + 1\n";
        let d = doc(src);
        let uri = Url::parse("file:///t.pine").unwrap();
        let use_byte = src.rfind("len").unwrap();
        let (ul, uc) = d.position_at(use_byte);
        let pos = Position::new(ul, uc);

        assert!(goto_definition(&d, pos, uri.clone()).is_some());
        assert_eq!(references(&d, pos, uri.clone()).len(), 2);

        let edit = rename(&d, pos, "length".into(), uri.clone()).unwrap();
        let n = edit.changes.unwrap().values().next().unwrap().len();
        assert_eq!(n, 2, "rename edits both occurrences");
    }

    #[test]
    fn no_rename_on_builtin() {
        let d = doc("//@version=6\nplot(close)\n");
        assert!(prepare_rename(&d, Position::new(1, 5)).is_none(), "close is builtin");
    }

    #[test]
    fn folding_for_multiline_function() {
        let d = doc("//@version=6\nf(x) =>\n    a = x + 1\n    a * 2\nplot(f(close))\n");
        assert!(!folding_ranges(&d).is_empty(), "function body should fold");
    }

    #[test]
    fn inlay_hints_label_builtin_args() {
        let d = doc("//@version=6\nplot(ta.sma(close, 14))\n");
        let hints = inlay_hints(&d);
        assert!(!hints.is_empty(), "expected parameter inlay hints");
        for h in &hints {
            match &h.label {
                InlayHintLabel::String(s) => assert!(s.ends_with(':'), "label `{s}`"),
                _ => panic!("expected string label"),
            }
        }
    }
}
