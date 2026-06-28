//! P2 LSP feature logic as pure functions over a parsed [`Document`], returning
//! `lsp_types` directly so they can be unit-tested without spinning up the async
//! Backend. Semantic diagnostics (the ported checker) arrive in P3; here
//! diagnostics are tree-sitter syntax errors only.

use std::collections::HashMap;

use pine_core::Document;
use pine_core::builtins;
use pine_core::imports::{
    ExportKind, ExportedSymbol, ImportEntry, ImportResolution, import_table, resolve_imports,
};
use pine_core::symbols::{self, SymbolKind as DefKind};
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
///
/// Note: a missing `/// @source` directive is deliberately NOT diagnosed.
/// `@source` is a local-library convenience; published imports (the common case,
/// e.g. `import TradingView/ta/7`) have no local file and legitimately omit it,
/// so flagging its absence would be a false-positive on valid v6. The hover over
/// an import alias surfaces the missing-source note contextually instead.
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

/// Hover for the builtin (function / variable / constant) under the cursor, or —
/// only when no builtin matches — for an import alias.
///
/// `builtin_doc` is tried FIRST so importing a name that collides with a builtin
/// (e.g. `import .. as math`) never shadows the builtin's existing hover; the
/// import fallback is purely additive (adds hover where there was none).
pub fn hover_at(doc: &Document, pos: Position) -> Option<Hover> {
    let byte = doc.offset_at(pos.line, pos.character);
    let word = word_at(doc.text(), byte)?;
    let value = builtin_doc(word).or_else(|| import_alias_hover(doc, word))?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

/// Hover markdown for an import whose effective namespace (alias, or lib name
/// when aliasless) equals `word`. `None` when no import matches `word`.
fn import_alias_hover(doc: &Document, word: &str) -> Option<String> {
    let table = import_table(doc);
    let entry = table
        .entries()
        .iter()
        .find(|entry| entry.effective_namespace() == word)?;
    // Render the `as` clause only when the import actually declared one — an
    // aliasless `import User/Lib/1` must not fabricate `as Lib`.
    let mut markdown = match &entry.alias {
        Some(alias) => format!(
            "```pine\nimport {}/{}/{} as {}\n```",
            entry.user, entry.lib, entry.version, alias
        ),
        None => format!(
            "```pine\nimport {}/{}/{}\n```",
            entry.user, entry.lib, entry.version
        ),
    };
    match &entry.source {
        Some(path) => {
            markdown.push_str("\n\nSource: ");
            markdown.push_str(path);
        }
        None => {
            markdown.push_str(
                "\n\nNo `/// @source` directive set; cross-file IntelliSense unavailable.",
            );
        }
    }
    Some(markdown)
}

fn builtin_doc(word: &str) -> Option<String> {
    if let Some(f) = builtins::function(word) {
        let head = if f.syntax.is_empty() {
            &f.name
        } else {
            &f.syntax
        };
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
///
/// `base_dir` is the directory of the open document (derived from its file
/// URL by the Backend), used only to resolve `/// @source` library imports for
/// cross-file member completion. Passing `None` (e.g. an `untitled:`/in-memory
/// doc, or any non-`file:` URL) reproduces the builtin-only behavior exactly:
/// no filesystem access and no cross-file members. Cross-file resolution fires
/// strictly on the post-`.` branch, so top-level completion stays fs-free.
// Uncached fallback retained as the documented public API and exercised by the
// unit tests (the running server uses `completions_at_cached`). In a bin-only
// build with no test cfg nothing calls it, so silence dead_code there.
#[cfg_attr(not(test), allow(dead_code))]
pub fn completions_at(
    doc: &Document,
    pos: Position,
    base_dir: Option<&std::path::Path>,
) -> Vec<CompletionItem> {
    let byte = doc.offset_at(pos.line, pos.character);
    let run = trailing_run(doc.text(), byte);
    match run.rfind('.') {
        Some(dot) => {
            let namespace = &run[..dot];
            // Builtin namespace members (e.g. `ta.`, `math.`) run first and
            // unconditionally, so this branch never regresses builtins.
            let mut items = namespace_members(namespace);
            // Additively append exported symbols of an imported library whose
            // effective namespace equals `namespace`. Skipped entirely when no
            // directory is known (graceful degrade for in-memory docs).
            if let Some(dir) = base_dir {
                append_imported_members(doc, namespace, dir, &mut items);
            }
            items
        }
        None => top_level_items(doc),
    }
}

/// Cached-path twin of [`completions_at`]: identical behavior, but cross-file
/// member completion uses the backend's pre-resolved import projection
/// (`resolved`) instead of re-reading/re-parsing libs per keystroke. The
/// top-level (no-`.`) and builtin-namespace branches are unchanged.
pub fn completions_at_cached(
    doc: &Document,
    pos: Position,
    resolved: &[(String, ImportResolution)],
) -> Vec<CompletionItem> {
    let byte = doc.offset_at(pos.line, pos.character);
    let run = trailing_run(doc.text(), byte);
    match run.rfind('.') {
        Some(dot) => {
            let namespace = &run[..dot];
            let mut items = namespace_members(namespace);
            append_imported_members_cached(namespace, resolved, &mut items);
            items
        }
        None => top_level_items(doc),
    }
}

/// Resolve `namespace`'s imported library (if any) and append its exported
/// symbols to `items`. No-op unless an import entry's effective namespace
/// matches `namespace` AND its `/// @source` resolved successfully. Uses
/// `effective_namespace` (not `by_alias`) so aliasless imports — whose
/// namespace falls back to the lib name — also resolve.
// Only reached via the uncached `completions_at` fallback (tests). See note there.
#[cfg_attr(not(test), allow(dead_code))]
fn append_imported_members(
    doc: &Document,
    namespace: &str,
    base_dir: &std::path::Path,
    items: &mut Vec<CompletionItem>,
) {
    let table = import_table(doc);
    // Cheap guard: only touch the filesystem when an import actually claims
    // this namespace. Published/unresolved imports still go through
    // resolve_imports below but yield Unresolved -> nothing appended.
    if !table
        .entries()
        .iter()
        .any(|entry| entry.effective_namespace() == namespace)
    {
        return;
    }
    // resolve_imports enforces the path-safety contract (refuses absolute /
    // escaping `@source` paths, canonicalizes under base_dir); we never bypass
    // it. Missing/escaping/parse-failed sources yield no members, not a panic.
    let resolved = resolve_imports(&table, base_dir);
    let Some(matched) = resolved
        .entries()
        .iter()
        .find(|resolved| resolved.entry.effective_namespace() == namespace)
    else {
        return;
    };
    if let ImportResolution::Resolved { symbols, .. } = &matched.resolution {
        for symbol in symbols {
            items.push(exported_item(symbol));
        }
    }
}

/// Cached-path twin of [`append_imported_members`]: given a pre-resolved
/// `(effective_namespace, ImportResolution)` projection (built + cached in the
/// backend so we don't re-read/re-parse libs per keystroke), append the matching
/// namespace's exported members. Pure in-memory work — no filesystem access.
///
/// Semantics intentionally mirror the uncached path: only `Resolved` entries
/// contribute members; `Unresolved`/`NotFound`/`ParseFailed` append nothing.
pub(crate) fn append_imported_members_cached(
    namespace: &str,
    resolved: &[(String, ImportResolution)],
    items: &mut Vec<CompletionItem>,
) {
    let Some((_, resolution)) = resolved.iter().find(|(ns, _)| ns == namespace) else {
        return;
    };
    if let ImportResolution::Resolved { symbols, .. } = resolution {
        for symbol in symbols {
            items.push(exported_item(symbol));
        }
    }
}

/// Map an [`ExportedSymbol`] to a bare-member [`CompletionItem`] (label is the
/// symbol name with no namespace prefix, consistent with builtin
/// `namespace_members`).
pub(crate) fn exported_item(symbol: &ExportedSymbol) -> CompletionItem {
    let kind = match symbol.kind {
        ExportKind::Function | ExportKind::Method => CompletionItemKind::FUNCTION,
        ExportKind::Type => CompletionItemKind::STRUCT,
        ExportKind::Enum => CompletionItemKind::ENUM,
    };
    item(symbol.name.clone(), kind, exported_detail(symbol))
}

/// A synthesized one-line signature for an exported fn/method
/// (`name(type a, type b=…)`); empty for types and enums (which carry no
/// params), mirroring how `fn_detail` renders builtin signatures.
fn exported_detail(symbol: &ExportedSymbol) -> String {
    match symbol.kind {
        ExportKind::Function | ExportKind::Method => {
            let params = symbol
                .params
                .iter()
                .map(|param| {
                    let typed = match &param.type_name {
                        Some(type_name) => format!("{type_name} {}", param.name),
                        None => param.name.clone(),
                    };
                    if param.has_default {
                        format!("{typed}=…")
                    } else {
                        typed
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", symbol.name, params)
        }
        ExportKind::Type | ExportKind::Enum => String::new(),
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

fn top_level_items(doc: &Document) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for f in builtins::FUNCTIONS.iter() {
        items.push(item(
            f.name.clone(),
            CompletionItemKind::FUNCTION,
            fn_detail(f),
        ));
    }
    for v in builtins::VARIABLES.iter() {
        items.push(item(
            v.name.clone(),
            CompletionItemKind::VARIABLE,
            v.ty.clone(),
        ));
    }
    for c in builtins::CONSTANTS.iter() {
        items.push(item(
            c.name.clone(),
            CompletionItemKind::CONSTANT,
            c.ty.clone(),
        ));
    }
    for k in builtins::KEYWORDS.all.iter() {
        items.push(CompletionItem {
            label: k.clone(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }
    // Additive: one MODULE item per import, keyed on the effective namespace
    // (alias, or lib name when aliasless). Skip any alias that collides with a
    // builtin namespace head (e.g. `ta`) so we never double-list it or disturb
    // the existing `ta.` member-completion path.
    for entry in import_table(doc).entries() {
        let namespace = entry.effective_namespace();
        if is_builtin_namespace_head(namespace) {
            continue;
        }
        items.push(import_completion(entry));
    }
    items
}

fn import_completion(entry: &ImportEntry) -> CompletionItem {
    CompletionItem {
        label: entry.effective_namespace().to_string(),
        kind: Some(CompletionItemKind::MODULE),
        detail: Some(format!(
            "import {}/{}/{}",
            entry.user, entry.lib, entry.version
        )),
        ..Default::default()
    }
}

/// True when `name` is the head namespace of some builtin (i.e. there exists a
/// builtin `name.<member>`). Used to avoid shadowing builtin namespaces like
/// `ta`/`math` with import-alias completion items.
fn is_builtin_namespace_head(name: &str) -> bool {
    let prefix = format!("{name}.");
    builtins::FUNCTIONS
        .iter()
        .any(|f| f.name.starts_with(&prefix))
        || builtins::VARIABLES
            .iter()
            .any(|v| v.name.starts_with(&prefix))
        || builtins::CONSTANTS
            .iter()
            .any(|c| c.name.starts_with(&prefix))
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
    let active_parameter =
        (!parameters.is_empty()).then(|| (active as u32).min(parameters.len() as u32 - 1));
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
///
/// Same-file resolution runs FIRST and is unchanged: a free identifier that
/// matches a top-level definition in this document jumps within the file and
/// ignores `base_dir`. Only when the cursor is on the MEMBER side of an
/// `alias.member` access (and no same-file definition matched) do we fall back
/// to cross-file resolution: resolve the alias's imported `/// @source` lib and
/// jump to the exported symbol's name in that lib file.
///
/// `base_dir` mirrors `completions_at`: it is the open document's directory,
/// used only to resolve local `/// @source` paths under the existing
/// path-safety contract. `None` (in-memory/`untitled:` docs) disables the
/// cross-file fallback entirely — behavior is then identical to same-file goto.
// Uncached fallback retained as the documented public API and exercised by the
// unit tests (the running server uses `goto_definition_cached`). Silenced in
// the bin-only build where no test cfg calls it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn goto_definition(
    doc: &Document,
    pos: Position,
    uri: Url,
    base_dir: Option<&std::path::Path>,
) -> Option<GotoDefinitionResponse> {
    let byte = doc.offset_at(pos.line, pos.character);

    // Same-file path first (unchanged behavior). Note: `identifier_at` returns
    // the identifier text regardless of whether it is a member; the same-file
    // definition lookup simply finds nothing for a cross-file member, so we fall
    // through to the import path below.
    if let Some((name, _, _)) = symbols::identifier_at(doc, byte) {
        if let Some(def) = symbols::definitions(doc)
            .into_iter()
            .find(|d| d.name == name)
        {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri,
                range: byte_range(doc, def.start_byte, def.end_byte),
            }));
        }
    }

    // Cross-file fallback: cursor on the member of `alias.member`.
    goto_imported_member(doc, byte, base_dir)
}

/// Cached-path twin of [`goto_definition`]: same-file resolution is identical;
/// the cross-file member fallback uses the backend's pre-resolved import
/// projection (`resolved`) instead of calling `resolve_imports` per request.
pub fn goto_definition_cached(
    doc: &Document,
    pos: Position,
    uri: Url,
    resolved: &[(String, ImportResolution)],
) -> Option<GotoDefinitionResponse> {
    let byte = doc.offset_at(pos.line, pos.character);

    if let Some((name, _, _)) = symbols::identifier_at(doc, byte)
        && let Some(def) = symbols::definitions(doc)
            .into_iter()
            .find(|d| d.name == name)
    {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri,
            range: byte_range(doc, def.start_byte, def.end_byte),
        }));
    }

    goto_imported_member_cached(doc, byte, resolved)
}

/// Resolve go-to-definition into an imported `/// @source` library when the
/// cursor sits on the MEMBER side of an `alias.member` access. Returns `None`
/// (graceful degrade, never panics) for any of: `base_dir` absent, cursor not on
/// an attribute member, the object is not an imported namespace, the import is
/// Unresolved/NotFound/ParseFailed, the member is not exported, the lib re-read
/// fails, or the path cannot be turned into a file URL.
// Only reached via the uncached `goto_definition` fallback (tests). See note there.
#[cfg_attr(not(test), allow(dead_code))]
fn goto_imported_member(
    doc: &Document,
    byte: usize,
    base_dir: Option<&std::path::Path>,
) -> Option<GotoDefinitionResponse> {
    let base_dir = base_dir?;

    // Find the `attribute` node enclosing the cursor and confirm the cursor is
    // on its `attribute` (member) child — never the `object` side. This mirrors
    // symbols.rs::collect_refs's `is_member` check, so normal alias-object goto
    // is unaffected.
    let leaf = doc.root().named_descendant_for_byte_range(byte, byte)?;
    let attribute = enclosing_attribute(leaf)?;
    let object_node = attribute.child_by_field_name("object")?;
    let member_node = attribute.child_by_field_name("attribute")?;
    // The cursor's leaf must be the member identifier, not the object.
    if member_node != leaf {
        return None;
    }
    let src = doc.text();
    let object = &src[object_node.start_byte()..object_node.end_byte()];
    let member = &src[member_node.start_byte()..member_node.end_byte()];

    // Resolve the import whose effective namespace is `object` under the
    // path-safety contract (we never bypass resolve_imports).
    let table = import_table(doc);
    if !table
        .entries()
        .iter()
        .any(|entry| entry.effective_namespace() == object)
    {
        return None;
    }
    let resolved = resolve_imports(&table, base_dir);
    let matched = resolved
        .entries()
        .iter()
        .find(|resolved| resolved.entry.effective_namespace() == object)?;
    let ImportResolution::Resolved { path, symbols } = &matched.resolution else {
        return None;
    };
    let symbol = symbols.iter().find(|symbol| symbol.name == member)?;

    // The name byte offsets are in the LIB's coordinate space, so we MUST use
    // the lib's own LineIndex (re-parse it) to convert them — using the main
    // doc's index would give wrong rows. Re-read from the canonical `path`
    // (never a raw @source string), preserving the path-safety contract.
    let lib_contents = std::fs::read_to_string(path).ok()?;
    let lib_doc = Document::parse(lib_contents)?;
    let (start_line, start_col) = lib_doc.position_at(symbol.name_byte_start);
    let (end_line, end_col) = lib_doc.position_at(symbol.name_byte_end);
    let lib_uri = Url::from_file_path(path).ok()?;

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: lib_uri,
        range: Range {
            start: Position::new(start_line, start_col),
            end: Position::new(end_line, end_col),
        },
    }))
}

/// Cached-path twin of [`goto_imported_member`]: resolves the member of an
/// `alias.member` access using a pre-resolved `(effective_namespace,
/// ImportResolution)` projection (cached in the backend) instead of calling
/// `resolve_imports` here. The single unavoidable filesystem read is the lib
/// re-read needed to map the symbol's lib-coordinate byte offsets to a line/col
/// range (the cached `ImportResolution::Resolved.path` is the canonical,
/// path-safety-checked target, so re-reading it preserves the safety contract).
/// Returns `None` for the same graceful-degrade cases as the uncached path.
pub(crate) fn goto_imported_member_cached(
    doc: &Document,
    byte: usize,
    resolved: &[(String, ImportResolution)],
) -> Option<GotoDefinitionResponse> {
    let leaf = doc.root().named_descendant_for_byte_range(byte, byte)?;
    let attribute = enclosing_attribute(leaf)?;
    let object_node = attribute.child_by_field_name("object")?;
    let member_node = attribute.child_by_field_name("attribute")?;
    if member_node != leaf {
        return None;
    }
    let src = doc.text();
    let object = &src[object_node.start_byte()..object_node.end_byte()];
    let member = &src[member_node.start_byte()..member_node.end_byte()];

    let (_, resolution) = resolved.iter().find(|(ns, _)| ns == object)?;
    let ImportResolution::Resolved { path, symbols } = resolution else {
        return None;
    };
    let symbol = symbols.iter().find(|symbol| symbol.name == member)?;

    // Byte offsets are in the LIB's coordinate space, so convert them with the
    // lib's own LineIndex (re-parse it) — see goto_imported_member.
    let lib_contents = std::fs::read_to_string(path).ok()?;
    let lib_doc = Document::parse(lib_contents)?;
    let (start_line, start_col) = lib_doc.position_at(symbol.name_byte_start);
    let (end_line, end_col) = lib_doc.position_at(symbol.name_byte_end);
    let lib_uri = Url::from_file_path(path).ok()?;

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: lib_uri,
        range: Range {
            start: Position::new(start_line, start_col),
            end: Position::new(end_line, end_col),
        },
    }))
}

/// Walk up from `node` to the nearest enclosing `attribute` CST node, if any.
/// The leaf identifier under an `alias.member` cursor is the immediate child of
/// the `attribute`, so this is at most a one-step walk in practice; the loop is
/// defensive against deeper nesting and stops at the document root.
fn enclosing_attribute(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        if current.kind() == "attribute" {
            return Some(current);
        }
        current = current.parent()?;
    }
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

// Semantic-token legend indices. MUST match `semantic_token_legend()` order.
const T_FUNCTION: u32 = 1;
const T_VARIABLE: u32 = 2;
const T_PARAMETER: u32 = 3;
const T_TYPE: u32 = 4;
const T_STRING: u32 = 5;
const T_NUMBER: u32 = 6;
const T_COMMENT: u32 = 7;
const T_NAMESPACE: u32 = 8;
const T_PROPERTY: u32 = 9;
const M_DEFAULT_LIBRARY: u32 = 1 << 0;
const M_DECLARATION: u32 = 1 << 1;

/// The legend advertised by the server; index order is load-bearing (see the
/// `T_*` constants above).
pub fn semantic_token_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::PROPERTY,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DEFAULT_LIBRARY,
            SemanticTokenModifier::DECLARATION,
        ],
    }
}

/// Full-document semantic tokens (delta-encoded per the LSP spec).
pub fn semantic_tokens(doc: &Document) -> SemanticTokens {
    let user = user_kind_map(doc);
    let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new(); // line, char, len, type, mods
    collect_tokens(doc.root(), doc, &user, &mut raw);
    raw.sort_by_key(|t| (t.0, t.1));

    let mut data = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_char) = (0u32, 0u32);
    for (line, character, length, token_type, token_modifiers_bitset) in raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            character - prev_char
        } else {
            character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset,
        });
        prev_line = line;
        prev_char = character;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

fn user_kind_map(doc: &Document) -> std::collections::HashMap<String, DefKind> {
    let mut map = std::collections::HashMap::new();
    for d in symbols::definitions(doc) {
        map.entry(d.name).or_insert(d.kind);
    }
    map
}

fn collect_tokens(
    node: Node,
    doc: &Document,
    user: &std::collections::HashMap<String, DefKind>,
    out: &mut Vec<(u32, u32, u32, u32, u32)>,
) {
    let classified = match node.kind() {
        "comment" => Some((T_COMMENT, 0)),
        "string" => Some((T_STRING, 0)),
        "integer" | "float" => Some((T_NUMBER, 0)),
        "identifier" => classify_identifier(node, doc.text(), user),
        _ => None,
    };
    if let Some((token_type, mods)) = classified {
        let (line, character) = doc.position_at(node.start_byte());
        let length = utf16_len(&doc.text()[node.start_byte()..node.end_byte()]);
        out.push((line, character, length, token_type, mods));
        return; // emitted nodes are atomic — don't recurse (keeps tokens non-overlapping)
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, doc, user, out);
    }
}

fn classify_identifier(
    node: Node,
    src: &str,
    user: &std::collections::HashMap<String, DefKind>,
) -> Option<(u32, u32)> {
    let name = &src[node.start_byte()..node.end_byte()];
    let parent = node.parent();
    let is_field = |field: &str| {
        parent.is_some_and(|p| {
            p.kind() == "attribute" && p.child_by_field_name(field).is_some_and(|c| c == node)
        })
    };
    if is_field("attribute") {
        return Some((T_PROPERTY, 0)); // member name in `obj.member`
    }
    if builtins::function(name).is_some() {
        return Some((T_FUNCTION, M_DEFAULT_LIBRARY));
    }
    if builtins::variable(name).is_some() || builtins::constant(name).is_some() {
        return Some((T_VARIABLE, M_DEFAULT_LIBRARY));
    }
    if let Some(kind) = user.get(name) {
        return Some(match kind {
            DefKind::Function => (T_FUNCTION, M_DECLARATION),
            DefKind::Parameter => (T_PARAMETER, 0),
            DefKind::Type | DefKind::Enum => (T_TYPE, 0),
            DefKind::Variable => (T_VARIABLE, 0),
        });
    }
    if is_field("object") {
        return Some((T_NAMESPACE, 0)); // e.g. `ta` in `ta.sma`
    }
    None
}

fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// Document formatting — deliberately conservative because Pine's indentation is
/// significant: trim trailing whitespace per line, drop trailing blank lines,
/// and guarantee exactly one final newline. No reindentation or operator
/// spacing (those risk changing semantics). Returns `None` when already clean.
pub fn format_document(doc: &Document) -> Option<Vec<TextEdit>> {
    let src = doc.text();
    let formatted = format_text(src);
    if formatted == src {
        return None;
    }
    let (end_line, end_char) = doc.position_at(src.len());
    Some(vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(end_line, end_char),
        },
        new_text: formatted,
    }])
}

fn format_text(src: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Quick-fix code actions for diagnostics in range. Currently: add a missing
/// `//@version=6` directive.
pub fn code_actions(diagnostics: &[Diagnostic], uri: Url) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for diag in diagnostics {
        if diag.code == Some(NumberOrString::String("missing-version".to_string())) {
            let mut changes = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(0, 0),
                    },
                    new_text: "//@version=6\n".to_string(),
                }],
            );
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add `//@version=6`".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                ..Default::default()
            }));
        }
    }
    actions
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
        let items = completions_at(&d, Position::new(1, 7), None);
        assert!(items.iter().any(|i| i.label == "sma"), "expected ta.sma");
        assert!(
            items.iter().all(|i| !i.label.contains('.')),
            "members are bare"
        );
    }

    #[test]
    fn completion_top_level_includes_builtins_and_keywords() {
        let d = doc("//@version=6\n\n");
        let items = completions_at(&d, Position::new(1, 0), None);
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
        assert_eq!(
            sh.active_parameter,
            Some(1),
            "after one comma → param index 1"
        );
    }

    #[test]
    fn document_symbols_lists_user_defs() {
        let d = doc("//@version=6\nlen = 14\nf(a) =>\n    a\n");
        let names: Vec<String> = document_symbols(&d).into_iter().map(|s| s.name).collect();
        assert!(names.iter().any(|n| n == "len"));
        assert!(names.iter().any(|n| n == "f"));
        assert!(
            !names.iter().any(|n| n == "a"),
            "params excluded from doc symbols"
        );
    }

    #[test]
    fn definition_references_rename_roundtrip() {
        let src = "//@version=6\nlen = 14\nz = len + 1\n";
        let d = doc(src);
        let uri = Url::parse("file:///t.pine").unwrap();
        let use_byte = src.rfind("len").unwrap();
        let (ul, uc) = d.position_at(use_byte);
        let pos = Position::new(ul, uc);

        assert!(goto_definition(&d, pos, uri.clone(), None).is_some());
        assert_eq!(references(&d, pos, uri.clone()).len(), 2);

        let edit = rename(&d, pos, "length".into(), uri.clone()).unwrap();
        let n = edit.changes.unwrap().values().next().unwrap().len();
        assert_eq!(n, 2, "rename edits both occurrences");
    }

    #[test]
    fn no_rename_on_builtin() {
        let d = doc("//@version=6\nplot(close)\n");
        assert!(
            prepare_rename(&d, Position::new(1, 5)).is_none(),
            "close is builtin"
        );
    }

    #[test]
    fn folding_for_multiline_function() {
        let d = doc("//@version=6\nf(x) =>\n    a = x + 1\n    a * 2\nplot(f(close))\n");
        assert!(!folding_ranges(&d).is_empty(), "function body should fold");
    }

    #[test]
    fn format_trims_and_normalizes() {
        let d = doc("//@version=6  \nplot(close)   \n\n\n");
        let edits = format_document(&d).expect("should reformat");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "//@version=6\nplot(close)\n");
    }

    #[test]
    fn format_noop_when_clean() {
        let d = doc("//@version=6\nplot(close)\n");
        assert!(format_document(&d).is_none());
    }

    #[test]
    fn code_action_adds_version() {
        let uri = Url::parse("file:///t.pine").unwrap();
        let diag = Diagnostic {
            range: Range::default(),
            code: Some(NumberOrString::String("missing-version".to_string())),
            message: String::new(),
            ..Default::default()
        };
        let actions = code_actions(&[diag], uri);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn semantic_tokens_classify_builtins_and_literals() {
        let d = doc("//@version=6\nx = ta.sma(close, 14)\nplot(x)\n");
        let toks = semantic_tokens(&d);
        assert!(!toks.data.is_empty());
        let types: Vec<u32> = toks.data.iter().map(|t| t.token_type).collect();
        assert!(types.contains(&T_FUNCTION), "plot → function");
        assert!(types.contains(&T_NUMBER), "14 → number");
        assert!(types.contains(&T_NAMESPACE), "ta → namespace");
        assert!(types.contains(&T_COMMENT), "//@version → comment");
        // close is a builtin variable → defaultLibrary modifier present somewhere
        assert!(
            toks.data
                .iter()
                .any(|t| t.token_modifiers_bitset & M_DEFAULT_LIBRARY != 0)
        );
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

    // ---- imports: hover / completion / diagnostics ----------------------------

    /// Position of the first byte of `needle` in `src`, as an LSP `Position`.
    fn pos_of(doc: &Document, src: &str, needle: &str) -> Position {
        let byte = src.find(needle).expect("needle in src");
        let (line, character) = doc.position_at(byte);
        Position::new(line, character)
    }

    fn hover_markdown(h: &Hover) -> &str {
        match &h.contents {
            HoverContents::Markup(MarkupContent { value, .. }) => value,
            _ => panic!("expected markup hover"),
        }
    }

    #[test]
    fn hover_on_import_alias_shows_library_path() {
        let src = "//@version=6\n/// @source ./libs/a.pine\nimport User/MyLib/1 as myLib\n";
        let d = doc(src);
        let h = hover_at(&d, pos_of(&d, src, "myLib")).expect("hover on alias");
        let md = hover_markdown(&h);
        assert!(md.contains("User/MyLib/1"), "markdown: {md}");
        assert!(md.contains("./libs/a.pine"), "markdown: {md}");
    }

    #[test]
    fn hover_on_aliasless_import_uses_lib_name() {
        let src = "//@version=6\nimport TV/Strategy/2\n";
        let d = doc(src);
        // Hover on the lib name `Strategy` (the effective namespace).
        let h = hover_at(&d, pos_of(&d, src, "Strategy")).expect("hover on lib name");
        let md = hover_markdown(&h);
        assert!(md.contains("TV/Strategy/2"), "markdown: {md}");
        assert!(
            !md.contains(" as "),
            "aliasless import must not fabricate `as`: {md}"
        );
    }

    #[test]
    fn hover_on_import_without_source_notes_missing() {
        let src = "//@version=6\nimport User/MyLib/1 as myLib\n";
        let d = doc(src);
        let h = hover_at(&d, pos_of(&d, src, "myLib")).expect("hover on alias");
        let md = hover_markdown(&h);
        assert!(
            md.contains("@source"),
            "should mention missing @source: {md}"
        );
    }

    #[test]
    fn hover_on_builtin_still_wins() {
        let src = "//@version=6\nplot(close)\n";
        let d = doc(src);
        // `close` is a builtin variable; alias fallback must not change this.
        let h = hover_at(&d, pos_of(&d, src, "close")).expect("builtin hover");
        let md = hover_markdown(&h);
        assert!(md.contains("close"), "builtin doc unchanged: {md}");
        assert!(
            !md.contains("import"),
            "must be the builtin doc, not an import: {md}"
        );
    }

    #[test]
    fn hover_on_plain_identifier_still_none() {
        let src = "//@version=6\nlen = 14\nplot(len)\n";
        let d = doc(src);
        // `len` is a user var, not a builtin and not an import → no hover.
        assert!(hover_at(&d, pos_of(&d, src, "len")).is_none());
    }

    #[test]
    fn completion_top_level_includes_import_alias() {
        let d = doc("//@version=6\nimport User/MyLib/1 as myLib\n\n");
        let items = completions_at(&d, Position::new(2, 0), None);
        let alias = items
            .iter()
            .find(|i| i.label == "myLib")
            .expect("myLib completion");
        assert_eq!(alias.kind, Some(CompletionItemKind::MODULE));
        assert!(
            alias
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("User/MyLib/1"),
            "detail: {:?}",
            alias.detail
        );
    }

    #[test]
    fn completion_alias_not_duplicated_for_builtin_namespace() {
        // An import aliased `ta` collides with the builtin `ta.` namespace head.
        let d = doc("//@version=6\nimport User/MyLib/1 as ta\n\n");
        let top = completions_at(&d, Position::new(2, 0), None);
        // No MODULE item named `ta` was added (the builtin namespace is untouched).
        assert!(
            !top.iter()
                .any(|i| i.label == "ta" && i.kind == Some(CompletionItemKind::MODULE)),
            "must not add a spurious `ta` MODULE item"
        );
        // The existing post-dot `ta.` member completion still works.
        let d2 = doc("//@version=6\nimport User/MyLib/1 as ta\nx = ta.\n");
        let members = completions_at(&d2, Position::new(2, 7), None);
        assert!(
            members.iter().any(|i| i.label == "sma"),
            "ta.sma still resolves"
        );
    }

    #[test]
    fn completion_after_dot_unchanged() {
        // With no `base_dir` (`None`), `myLib.` must NOT invent members: no path
        // means no cross-file resolution, and `myLib` is not a builtin namespace
        // so `namespace_members` finds nothing. This proves the graceful
        // degrade for in-memory/`untitled:` docs.
        let src = "//@version=6\nimport User/MyLib/1 as myLib\nx = myLib.\n";
        let d = doc(src);
        let after_dot = completions_at(&d, Position::new(2, 10), None);
        assert!(
            after_dot.is_empty(),
            "no fabricated members for `myLib.` with no path"
        );
    }

    /// The committed fixture-lib directory used as the resolver's `base_dir`,
    /// resolved relative to `CARGO_MANIFEST_DIR` (matching pine-core's
    /// resolve_imports tests). Deterministic; no temp files.
    fn libs_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libs")
    }

    #[test]
    fn completion_after_dot_resolves_imported_lib_members() {
        // `/// @source math_utils.pine` + `as mu`, cursor after `mu.`, resolved
        // against the committed fixture dir: the lib's EXPORTED fns appear as
        // bare FUNCTION items; the non-exported `helper` does not.
        let src =
            "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.\n";
        let d = doc(src);
        let items = completions_at(&d, Position::new(3, 7), Some(&libs_dir()));
        let add = items
            .iter()
            .find(|i| i.label == "add")
            .expect("exported `add` member");
        assert_eq!(add.kind, Some(CompletionItemKind::FUNCTION));
        assert!(
            items.iter().any(|i| i.label == "clamp"),
            "exported `clamp` member"
        );
        assert!(
            !items.iter().any(|i| i.label == "helper"),
            "non-exported `helper` must be absent"
        );
        assert!(
            items.iter().all(|i| !i.label.contains('.')),
            "members are bare"
        );
    }

    #[test]
    fn completion_after_dot_no_path_degrades() {
        // Same source, but `base_dir = None`: no cross-file members, no panic.
        let src =
            "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.\n";
        let d = doc(src);
        let items = completions_at(&d, Position::new(3, 7), None);
        assert!(
            !items.iter().any(|i| i.label == "add" || i.label == "clamp"),
            "no cross-file members without a path"
        );
    }

    #[test]
    fn completion_after_dot_published_import_no_members() {
        // No `/// @source` (published import) + a real base_dir -> Unresolved ->
        // nothing extra appended.
        let src = "//@version=6\nimport User/MathUtils/1 as mu\nx = mu.\n";
        let d = doc(src);
        let items = completions_at(&d, Position::new(2, 7), Some(&libs_dir()));
        assert!(
            !items.iter().any(|i| i.label == "add" || i.label == "clamp"),
            "published import resolves no local members"
        );
    }

    #[test]
    fn completion_after_dot_missing_source_file_no_panic() {
        // `@source` names a file that does not exist -> NotFound -> empty, no panic.
        let src = "//@version=6\n/// @source does-not-exist.pine\nimport User/MathUtils/1 as mu\nx = mu.\n";
        let d = doc(src);
        let items = completions_at(&d, Position::new(3, 7), Some(&libs_dir()));
        assert!(
            !items.iter().any(|i| i.label == "add" || i.label == "clamp"),
            "missing @source file yields no members"
        );
    }

    #[test]
    fn completion_aliasless_import_members_resolve() {
        // Aliasless import: effective_namespace falls back to the lib name
        // (`MathUtils`). Proves the `effective_namespace` match path (NOT
        // `by_alias`, which only matches explicit aliases).
        let src =
            "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1\nx = MathUtils.\n";
        let d = doc(src);
        let items = completions_at(&d, Position::new(3, 14), Some(&libs_dir()));
        assert!(
            items.iter().any(|i| i.label == "add"),
            "aliasless `add` resolves"
        );
        assert!(
            items.iter().any(|i| i.label == "clamp"),
            "aliasless `clamp` resolves"
        );
    }

    #[test]
    fn completion_builtin_namespace_still_works_with_base_dir() {
        // `ta.` with a real base_dir still lists builtin members: the additive
        // import path does not regress builtin member completion.
        let d = doc("//@version=6\nx = ta.\n");
        let items = completions_at(&d, Position::new(1, 7), Some(&libs_dir()));
        assert!(
            items.iter().any(|i| i.label == "sma"),
            "builtin `ta.sma` still listed"
        );
    }

    #[test]
    fn source_less_import_emits_no_diagnostic() {
        // `/// @source` is a local-library convenience; published imports
        // legitimately omit it. A missing directive must NOT be flagged
        // (regression guard against the removed `import-no-source` false-positive).
        let d = doc("//@version=6\nindicator(\"x\")\nimport TradingView/ta/7\nplot(close)\n");
        let diags = all_diagnostics(&d);
        assert!(
            !diags
                .iter()
                .any(|d| d.code == Some(NumberOrString::String("import-no-source".to_string()))),
            "source-less import must not produce a diagnostic"
        );
    }

    #[test]
    fn all_diagnostics_keeps_syntax_errors_with_imports() {
        let d = doc("//@version=6\nimport User/MyLib/1 as myLib\nx = (1 + \n");
        let diags = all_diagnostics(&d);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR)),
            "syntax ERROR must remain"
        );
    }

    #[test]
    fn no_import_no_diagnostics_no_completion_change() {
        // A plain indicator/plot doc with zero imports: additive-only proof.
        let d = doc("//@version=6\nindicator(\"x\")\nplot(close)\n");
        let items = completions_at(&d, Position::new(3, 0), None);
        assert!(
            !items
                .iter()
                .any(|i| i.kind == Some(CompletionItemKind::MODULE)),
            "no MODULE items without imports"
        );
    }

    // ---- exported_detail: synthesized signature strings -----------------------

    /// Build an `ExportedParam` (name, optional type, defaulted) for tests.
    fn param(
        name: &str,
        type_name: Option<&str>,
        has_default: bool,
    ) -> pine_core::imports::ExportedParam {
        pine_core::imports::ExportedParam {
            name: name.to_string(),
            type_name: type_name.map(str::to_string),
            has_default,
        }
    }

    /// Build an `ExportedSymbol`; the name byte span is irrelevant to
    /// `exported_detail`, so it is left zeroed.
    fn sym(
        name: &str,
        kind: ExportKind,
        params: Vec<pine_core::imports::ExportedParam>,
    ) -> ExportedSymbol {
        ExportedSymbol {
            name: name.to_string(),
            kind,
            params,
            name_byte_start: 0,
            name_byte_end: 0,
        }
    }

    #[test]
    fn exported_detail_function_uses_ellipsis_for_defaults() {
        // The synthesizer has only `has_default` (no default-value text), so a
        // defaulted param renders as `=…`, NOT `=1.0`.
        let add = sym(
            "add",
            ExportKind::Function,
            vec![
                param("a", Some("int"), false),
                param("b", Some("float"), true),
            ],
        );
        assert_eq!(exported_detail(&add), "add(int a, float b=…)");
    }

    #[test]
    fn exported_detail_typeless_param_omits_type() {
        // `f(int a, c)`: the typeless `c` renders bare (no type prefix).
        let f = sym(
            "f",
            ExportKind::Function,
            vec![param("a", Some("int"), false), param("c", None, false)],
        );
        assert_eq!(exported_detail(&f), "f(int a, c)");
    }

    #[test]
    fn exported_detail_type_and_enum_are_empty() {
        let point = sym("Point", ExportKind::Type, Vec::new());
        let color = sym("Color", ExportKind::Enum, Vec::new());
        assert_eq!(exported_detail(&point), "");
        assert_eq!(exported_detail(&color), "");
    }

    // ---- goto_definition: cross-file alias.member -----------------------------

    #[test]
    fn goto_on_imported_member_jumps_into_lib() {
        // Cursor on `add` in `mu.add`, resolved against the committed fixture
        // dir, jumps to the `add` export's name in math_utils.pine (row 3).
        let src = "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.add(1, 2.0)\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        let pos = pos_of(&d, src, "add(1, 2.0)"); // the member `add` on the use line
        let resp = goto_definition(&d, pos, uri, Some(&libs_dir())).expect("cross-file goto");
        let GotoDefinitionResponse::Scalar(loc) = resp else {
            panic!("expected a scalar location");
        };
        assert!(
            loc.uri.path().ends_with("math_utils.pine"),
            "uri must point at the lib: {}",
            loc.uri
        );
        // `add` is declared on row 3 (0-indexed), column 7 (`export add`).
        assert_eq!(loc.range.start.line, 3, "row of the `add` export");
        assert_eq!(loc.range.start.character, 7, "col of the `add` name");
    }

    #[test]
    fn goto_on_imported_member_without_base_dir_is_none() {
        // No base_dir -> no cross-file resolution; `add` is not a same-file
        // symbol either, so the result is None (graceful degrade).
        let src = "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.add(1, 2.0)\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        let pos = pos_of(&d, src, "add(1, 2.0)");
        assert!(goto_definition(&d, pos, uri, None).is_none());
    }

    #[test]
    fn goto_same_file_still_works_with_base_dir() {
        // A local symbol still resolves within the file even when base_dir is set.
        let src = "//@version=6\nlen = 14\nz = len + 1\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        let use_byte = src.rfind("len").unwrap();
        let (ul, uc) = d.position_at(use_byte);
        let pos = Position::new(ul, uc);
        assert!(goto_definition(&d, pos, uri, Some(&libs_dir())).is_some());
    }

    #[test]
    fn goto_on_unexported_member_is_none() {
        // `helper` is defined in math_utils.pine but NOT exported, so it is not
        // in the resolved exports -> no jump.
        let src = "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.helper(1)\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        let pos = pos_of(&d, src, "helper(1)");
        assert!(goto_definition(&d, pos, uri, Some(&libs_dir())).is_none());
    }

    #[test]
    fn goto_on_published_import_member_is_none() {
        // No `/// @source` (published import) -> Unresolved -> no cross-file goto
        // even with a real base_dir.
        let src = "//@version=6\nimport User/MathUtils/1 as mu\nx = mu.add(1, 2.0)\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        let pos = pos_of(&d, src, "add(1, 2.0)");
        assert!(goto_definition(&d, pos, uri, Some(&libs_dir())).is_none());
    }

    #[test]
    fn goto_on_alias_object_side_does_not_jump_into_lib() {
        // Cursor on the OBJECT `mu` (not the member) must not be hijacked by the
        // cross-file member path. The alias `mu` IS a same-file symbol (the
        // import declaration), so goto resolves WITHIN the main file — never into
        // the lib. This proves the new member path doesn't shadow alias goto.
        let src = "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\nx = mu.add(1, 2.0)\n";
        let d = doc(src);
        let uri = Url::parse("file:///main.pine").unwrap();
        // The `mu` on the use line (line 3), distinct from the import line.
        let use_line_byte = src.rfind("mu.add").unwrap();
        let (ul, uc) = d.position_at(use_line_byte);
        let pos = Position::new(ul, uc);
        let resp = goto_definition(&d, pos, uri, Some(&libs_dir()))
            .expect("alias goto resolves to its import in-file");
        let GotoDefinitionResponse::Scalar(loc) = resp else {
            panic!("expected a scalar location");
        };
        assert!(
            !loc.uri.path().ends_with("math_utils.pine"),
            "object-side cursor must stay in the main file, not jump into the lib: {}",
            loc.uri
        );
        assert_eq!(loc.uri.path(), "/main.pine", "stays in the main document");
    }
}
