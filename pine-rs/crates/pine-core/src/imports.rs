//! Parsing of Pine `import` statements and their `/// @source` directives into a
//! typed [`ImportTable`].
//!
//! This is a **descriptive** pass only: it records what imports a document
//! declares and which (if any) `/// @source` file each is annotated with. It does
//! **not** load or resolve library files, nor does it emit any diagnostics — so
//! it cannot itself fire a false positive on valid v6. Callers decide severity
//! (e.g. a later "unresolved import" check) from the fact that `source` is
//! `None`.
//!
//! ## Grammar facts this relies on (verified against the live CST)
//!
//! - `import User/MyLib/1 as myLib` parses to a **named** `import` node with a
//!   `path` field (an `import_path` node) and an optional `alias` field (an
//!   `identifier`). The kind string `"import"` ALSO names the unnamed `import`
//!   keyword token, so the walk filters on [`Node::is_named`].
//! - `import_path` exposes its three segments only as anonymous regex tokens
//!   (`child_by_field_name("username"/"export"/"version")` all return `None`),
//!   so segments are recovered by splitting the node's **source text** on `/` —
//!   exactly as the TS implementation splits `libraryPath`.
//! - The grammar requires all three path segments; an unversioned
//!   `import User/Lib` yields an ERROR node and NO `import` node, so malformed
//!   imports are naturally absent from the table (no panic, no bogus entry).
//! - `/// @source ...` is a plain `comment` node (not a grammar annotation), so
//!   the directive is matched against the comment's full source text, mirroring
//!   the TS regex `^\s*///\s*@source\s+(.+?)\s*$`.
//! - A directive associates with an import iff the directive comment sits on the
//!   line **immediately before** the import (`directive_row == import_row - 1`),
//!   matching TS semantics. Reformatting that moves the directive silently drops
//!   `source` (a false-negative, which is the acceptable bias here).

use crate::Document;
use std::path::Path;
use tree_sitter::Node;

/// A single parsed `import` statement.
///
/// `alias` is `Option` because the grammar makes `as <alias>` optional. When it
/// is `None`, the effective namespace downstream is the [`lib`](ImportEntry::lib)
/// name (mirroring the TS fallback `libraryPath.split('/').pop()`); see
/// [`ImportEntry::effective_namespace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    /// Explicit alias from `as <alias>`, if present.
    pub alias: Option<String>,
    /// First path segment (username), e.g. `User` in `User/MyLib/1`.
    pub user: String,
    /// Second path segment (library/export name), e.g. `MyLib`.
    pub lib: String,
    /// Third path segment (version), e.g. `1`.
    pub version: String,
    /// Path from a `/// @source <path>` directive on the immediately-preceding
    /// line, if any. `None` does not imply an error — callers decide.
    pub source: Option<String>,
    /// 0-indexed row of the `import` statement (tree-sitter `start_position`).
    pub line: usize,
    /// Byte range of the `import` node, for later go-to-definition / diagnostics.
    pub start_byte: usize,
    /// Byte range end of the `import` node.
    pub end_byte: usize,
}

impl ImportEntry {
    /// The namespace this import is referenced by in code: the explicit alias if
    /// present, otherwise the library name (TS fallback semantics).
    pub fn effective_namespace(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.lib)
    }
}

/// All `import` statements in a document, in source order.
#[derive(Debug, Clone, Default)]
pub struct ImportTable {
    entries: Vec<ImportEntry>,
}

impl ImportTable {
    /// All parsed import entries, in source order.
    pub fn entries(&self) -> &[ImportEntry] {
        &self.entries
    }

    /// Look up an import by its **explicit** alias.
    ///
    /// Entries with no `as <alias>` are not matched by this lookup (their lib
    /// name is not treated as an alias here); use [`ImportEntry::effective_namespace`]
    /// at the call site if you want the aliasless-fallback behavior.
    pub fn by_alias(&self, alias: &str) -> Option<&ImportEntry> {
        self.entries
            .iter()
            .find(|entry| entry.alias.as_deref() == Some(alias))
    }

    /// True when the document declares no imports.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of import entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Parse all `import` statements (and their `/// @source` directives) from a
/// document into an [`ImportTable`].
///
/// Two small CST passes keep this readable: one collects the imports, the other
/// collects `/// @source` directive rows; then the directives are joined onto
/// imports by the strict previous-line rule.
pub fn import_table(doc: &Document) -> ImportTable {
    let src = doc.text();
    let root = doc.root();

    let mut entries = collect_imports(root, src);
    let source_directives = collect_source_directives(root, src);

    for entry in &mut entries {
        // A directive on the line immediately before the import. Guard the
        // usize subtraction so an import on row 0 can't underflow.
        if entry.line == 0 {
            continue;
        }
        let previous_row = entry.line - 1;
        if let Some(path) = source_directives
            .iter()
            .find(|(row, _)| *row == previous_row)
            .map(|(_, path)| path.clone())
        {
            entry.source = Some(path);
        }
    }

    ImportTable { entries }
}

/// Walk the CST collecting every **named** `import` node as an [`ImportEntry`]
/// (with `source` left `None` for the join pass to fill).
fn collect_imports(root: Node, src: &str) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    walk_imports(root, src, &mut out);
    out
}

fn walk_imports(node: Node, src: &str, out: &mut Vec<ImportEntry>) {
    // Filter to the named statement node: kind "import" also names the unnamed
    // `import` keyword token, which would otherwise be double-counted. An import
    // node has no nested imports, but we recurse unconditionally below anyway
    // (cheap, and robust to ERROR-recovery shapes).
    let is_named_import = node.kind() == "import" && node.is_named();
    if let Some(entry) = is_named_import
        .then(|| parse_import_node(node, src))
        .flatten()
    {
        out.push(entry);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_imports(child, src, out);
    }
}

/// Build an [`ImportEntry`] from a named `import` node, or `None` if the path
/// text doesn't split into exactly three non-empty segments (defensive against
/// ERROR-recovery shapes; shouldn't happen for a well-formed import node).
fn parse_import_node(node: Node, src: &str) -> Option<ImportEntry> {
    let path_node = node.child_by_field_name("path")?;
    let path_text = &src[path_node.start_byte()..path_node.end_byte()];

    // The import_path segments are anonymous regex tokens, so we recover them by
    // splitting the source text — mirroring the TS `libraryPath.split('/')`.
    let segments: Vec<&str> = path_text.split('/').collect();
    if segments.len() != 3 || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }

    let alias = node
        .child_by_field_name("alias")
        .filter(|alias_node| alias_node.kind() == "identifier")
        .map(|alias_node| src[alias_node.start_byte()..alias_node.end_byte()].to_string());

    Some(ImportEntry {
        alias,
        user: segments[0].to_string(),
        lib: segments[1].to_string(),
        version: segments[2].to_string(),
        source: None,
        line: node.start_position().row,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

/// Walk the CST collecting `(row, source_path)` for every `comment` node whose
/// text is a `/// @source <path>` directive.
fn collect_source_directives(root: Node, src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    walk_directives(root, src, &mut out);
    out
}

fn walk_directives(node: Node, src: &str, out: &mut Vec<(usize, String)>) {
    if node.kind() == "comment" {
        let comment_text = &src[node.start_byte()..node.end_byte()];
        if let Some(path) = parse_source_directive(comment_text) {
            // Associate by the comment's end row: a `/// @source` directive is a
            // single line, so start and end rows are the same in practice.
            out.push((node.end_position().row, path));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_directives(child, src, out);
    }
}

/// Match `/// @source <path>` against a comment's full text, returning the
/// trimmed path. Replicates the TS regex `^\s*///\s*@source\s+(.+?)\s*$` with
/// plain string operations (no `regex` dependency for one directive).
///
/// Requirements (each maps to a piece of the TS regex):
/// - leading whitespace allowed (`^\s*`),
/// - the `///` comment marker starts the comment; we require it so plain `//`
///   comments and the `//@version=6` annotation are ignored (`\/\/\/`),
/// - optional whitespace, then the literal `@source` (`\s*@source`),
/// - at least one whitespace separating `@source` from the path (`\s+`),
/// - a non-empty path with trailing whitespace trimmed (`(.+?)\s*$`).
fn parse_source_directive(comment_text: &str) -> Option<String> {
    let trimmed = comment_text.trim_start();

    // Must begin with the `///` comment marker.
    let after_slashes = trimmed.strip_prefix("///")?;

    // Optional whitespace, then the literal `@source`.
    let after_at_source = after_slashes.trim_start().strip_prefix("@source")?;

    // The TS regex requires `\s+` between `@source` and the path: there must be
    // at least one whitespace char, and the path must be non-empty.
    if !after_at_source
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        return None;
    }

    let path = after_at_source.trim();
    if path.is_empty() {
        return None;
    }

    Some(path.to_string())
}

// ===========================================================================
// Bounded library-source resolver (additive, descriptive-only).
//
// Given an already-parsed [`ImportTable`] and a base directory, this resolves
// each entry whose `/// @source` directive names a LOCAL relative path by
// reading + parsing the referenced lib file and extracting its top-level
// EXPORTED declarations. Published imports (no `@source`) resolve to
// [`ImportResolution::Unresolved`], which is explicitly **not** an error.
//
// This emits NO diagnostics and changes no existing parsing, so it cannot
// create a v6 false positive. Callers decide what (if anything) to surface.
//
// ## Grammar facts this relies on (verified against the live CST)
//
// - `export` is an **anonymous** token (`{type:"export", named:false}`) and is
//   NOT a field. It appears as the FIRST direct child of
//   `function_declaration_statement`, `type_definition_statement`, and
//   `enum_declaration` (each `optional('export')`). `to_sexp()` hides it, but
//   `node.children()` yields it — so export is detected by scanning direct
//   children for `kind() == "export"` (never `child_by_field_name`, which
//   always returns `None` for an anonymous token).
// - Function name is field `function`; methods use field `method` (mirroring
//   `symbols.rs::collect_defs`).
// - Parameters live in an ordered field sequence: an optional `qualifier`
//   (`type_qualifier`, e.g. `series`/`simple`), then an optional `type`
//   (`base_type`/`array_type`/`generic_type`), then the required `argument`
//   (`identifier`), then an optional `default_value`. Because `type` is a
//   SEPARATE, possibly-shorter field list than `argument`
//   (`myFn(int a, c)` -> 2 args, 1 type), params CANNOT be zipped by index.
//   Extraction is an ORDERED single cursor walk tracking `field_name()`:
//   stash a seen `type` as the pending type, and on each `argument` emit a
//   param consuming the pending type; a following `default_value` marks the
//   just-emitted param as defaulted.
// ===========================================================================

/// What kind of top-level exported declaration a symbol is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportKind {
    Function,
    Method,
    Type,
    Enum,
}

/// One parameter of an exported function/method.
///
/// `type_name` is best-effort: the source text of the param's `type` field if
/// present, otherwise `None` (a typeless param like `c` in `f(int a, c)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedParam {
    pub name: String,
    pub type_name: Option<String>,
    pub has_default: bool,
}

/// A single top-level exported declaration from a lib file.
///
/// `params` is empty for [`ExportKind::Type`] and [`ExportKind::Enum`].
///
/// `name_byte_start`/`name_byte_end` are the byte offsets of the declaration's
/// NAME identifier in the LIB source (not row/col, to keep pine-core LSP-free).
/// The LSP converts them to a position via the lib's own `LineIndex` for
/// go-to-definition; an in-file consumer never needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedSymbol {
    pub name: String,
    pub kind: ExportKind,
    pub params: Vec<ExportedParam>,
    /// Byte offset of the first byte of the name identifier in the lib source.
    pub name_byte_start: usize,
    /// Byte offset one past the last byte of the name identifier in the lib source.
    pub name_byte_end: usize,
}

/// The outcome of resolving one import entry's `/// @source` directive.
#[derive(Debug, Clone)]
pub enum ImportResolution {
    /// A local lib file was read + parsed; these exports were recovered. This
    /// can be returned even if the lib had parse ERROR nodes (tree-sitter still
    /// yields a tree; we collect best-effort).
    ///
    /// `path` is the canonical absolute path of the resolved lib file (the same
    /// path-safety-checked target the resolver read), so a downstream LSP can
    /// build a file URI / re-read the lib without re-deriving it.
    Resolved {
        path: std::path::PathBuf,
        symbols: Vec<ExportedSymbol>,
    },
    /// No `/// @source` directive (the common published-import case). NOT an
    /// error — the resolver has nothing local to read.
    Unresolved,
    /// A `@source` was given but the file could not be read, the path was
    /// absolute, or it escaped the base directory (refused for safety).
    NotFound,
    /// The file was read but `Document::parse` returned `None`. Does not happen
    /// in practice (tree-sitter recovers); kept for completeness.
    ParseFailed,
}

/// One import entry paired with its [`ImportResolution`].
#[derive(Debug, Clone)]
pub struct ResolvedImport<'a> {
    pub entry: &'a ImportEntry,
    pub resolution: ImportResolution,
}

/// All resolved imports for a document, in the table's source order.
#[derive(Debug, Clone, Default)]
pub struct ResolvedImports<'a> {
    entries: Vec<ResolvedImport<'a>>,
}

impl<'a> ResolvedImports<'a> {
    /// All resolved-import records, in source order.
    pub fn entries(&self) -> &[ResolvedImport<'a>] {
        &self.entries
    }

    /// True when there were no imports to resolve.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of resolved-import records.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Look up a resolved import by the underlying entry's **explicit** alias
    /// (mirrors [`ImportTable::by_alias`] — aliasless imports are not matched).
    pub fn by_alias(&self, alias: &str) -> Option<&ResolvedImport<'a>> {
        self.entries
            .iter()
            .find(|resolved| resolved.entry.alias.as_deref() == Some(alias))
    }
}

/// Resolve every entry of `table` against `base_dir`.
///
/// See [`ImportResolution`] for the per-entry outcomes and the module-level
/// docs for the safety contract. This reads files but emits no diagnostics.
pub fn resolve_imports<'a>(table: &'a ImportTable, base_dir: &Path) -> ResolvedImports<'a> {
    let entries = table
        .entries()
        .iter()
        .map(|entry| ResolvedImport {
            entry,
            resolution: resolve_entry(entry, base_dir),
        })
        .collect();
    ResolvedImports { entries }
}

/// Resolve one entry. Published imports (no `@source`) are `Unresolved`; local
/// `@source` paths are read + parsed under the [path-safety](safe_local_path)
/// contract.
fn resolve_entry(entry: &ImportEntry, base_dir: &Path) -> ImportResolution {
    let Some(rel) = entry.source.as_deref() else {
        return ImportResolution::Unresolved;
    };

    let Some(target) = safe_local_path(base_dir, rel) else {
        return ImportResolution::NotFound;
    };

    let Ok(contents) = std::fs::read_to_string(&target) else {
        return ImportResolution::NotFound;
    };

    match Document::parse(contents) {
        Some(doc) => ImportResolution::Resolved {
            path: target,
            symbols: exported_symbols(&doc),
        },
        None => ImportResolution::ParseFailed,
    }
}

/// Resolve a relative `@source` path under `base_dir`, refusing anything that
/// escapes the base directory.
///
/// Deliberate safety choices (an LSP must not be coaxed into reading arbitrary
/// files by a directive in a script):
/// - Absolute `source` paths are rejected (the contract is a LOCAL relative
///   path).
/// - The joined path is canonicalized and required to stay under the
///   canonical `base_dir`, so `../../etc/passwd` is refused.
///
/// Returns `None` (-> `NotFound`) for any rejected or non-existent path.
fn safe_local_path(base_dir: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }

    // Canonicalize the base first; if the base itself can't be canonicalized
    // (doesn't exist) we cannot safely contain anything, so refuse.
    let canonical_base = base_dir.canonicalize().ok()?;

    // Canonicalize the joined target. canonicalize() requires the path to
    // exist, which doubles as the "file is missing -> NotFound" check and
    // resolves any `..` segments so the prefix check below is sound.
    let canonical_target = canonical_base.join(rel_path).canonicalize().ok()?;

    if canonical_target.starts_with(&canonical_base) {
        Some(canonical_target)
    } else {
        None
    }
}

/// Extract every top-level **exported** declaration from a parsed lib document.
///
/// Reusable and fs-free (testable directly). Returns an empty vec for a lib
/// with no exports — that is not an error. Best-effort on ERROR-recovery trees:
/// missing fields are simply skipped, never unwrapped.
pub fn exported_symbols(doc: &Document) -> Vec<ExportedSymbol> {
    let src = doc.text();
    let mut out = Vec::new();
    collect_exports(doc.root(), src, &mut out);
    out
}

fn collect_exports(node: Node, src: &str, out: &mut Vec<ExportedSymbol>) {
    match node.kind() {
        "function_declaration_statement" if has_export_child(node) => {
            if let Some(symbol) = parse_exported_function(node, src) {
                out.push(symbol);
            }
        }
        "type_definition_statement" if has_export_child(node) => {
            if let Some((name, name_byte_start, name_byte_end)) =
                field_identifier_span(node, "name", src)
            {
                out.push(ExportedSymbol {
                    name,
                    kind: ExportKind::Type,
                    params: Vec::new(),
                    name_byte_start,
                    name_byte_end,
                });
            }
        }
        "enum_declaration" if has_export_child(node) => {
            if let Some((name, name_byte_start, name_byte_end)) =
                field_identifier_span(node, "name", src)
            {
                out.push(ExportedSymbol {
                    name,
                    kind: ExportKind::Enum,
                    params: Vec::new(),
                    name_byte_start,
                    name_byte_end,
                });
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_exports(child, src, out);
    }
}

/// True iff a direct child is the anonymous `export` token. `export` has no
/// field, so this is the only reliable way to detect it.
fn has_export_child(node: Node) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "export")
}

/// Text of the `identifier`-kinded child under `field` plus its byte span
/// `(text, start_byte, end_byte)` in the LIB source — used to populate
/// [`ExportedSymbol::name_byte_start`]/[`ExportedSymbol::name_byte_end`] so an
/// LSP can jump to the exact name token.
fn field_identifier_span(node: Node, field: &str, src: &str) -> Option<(String, usize, usize)> {
    let id = node.child_by_field_name(field)?;
    if id.kind() != "identifier" {
        return None;
    }
    Some((
        src[id.start_byte()..id.end_byte()].to_string(),
        id.start_byte(),
        id.end_byte(),
    ))
}

/// Build an [`ExportedSymbol`] for an exported function or method declaration.
fn parse_exported_function(node: Node, src: &str) -> Option<ExportedSymbol> {
    // Regular functions bind the `function` field; methods bind `method`
    // (mirroring symbols.rs::collect_defs). The presence of `method` decides
    // the kind.
    let is_method = node.child_by_field_name("method").is_some();
    let (name, name_byte_start, name_byte_end) = field_identifier_span(node, "function", src)
        .or_else(|| field_identifier_span(node, "method", src))?;

    let kind = if is_method {
        ExportKind::Method
    } else {
        ExportKind::Function
    };

    Some(ExportedSymbol {
        name,
        kind,
        params: collect_params(node, src),
        name_byte_start,
        name_byte_end,
    })
}

/// Collect a declaration's parameters via an ORDERED cursor walk over its
/// direct children, tracking `field_name()`.
///
/// The grammar emits, per param and in this order: optional `qualifier`,
/// optional `type`, required `argument`, optional `default_value`. Because the
/// `type` list can be shorter than the `argument` list, we MUST associate a
/// `type` with the next `argument` positionally as we walk — never by zipping
/// the two field lists by index.
fn collect_params(node: Node, src: &str) -> Vec<ExportedParam> {
    let mut params = Vec::new();
    let mut pending_type: Option<String> = None;

    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return params;
    }
    loop {
        let child = cursor.node();
        match cursor.field_name() {
            Some("type") => {
                // Stash the type for the NEXT `argument`. Take the whole `type`
                // node's text (base_type / array_type / generic_type).
                pending_type = Some(src[child.start_byte()..child.end_byte()].to_string());
            }
            Some("argument") if child.kind() == "identifier" => {
                params.push(ExportedParam {
                    name: src[child.start_byte()..child.end_byte()].to_string(),
                    type_name: pending_type.take(),
                    has_default: false,
                });
            }
            Some("default_value") => {
                // Marks the parameter we just emitted as having a default.
                if let Some(last) = params.last_mut() {
                    last.has_default = true;
                }
            }
            // `qualifier` and any anonymous tokens (`(`, `,`, `)`) are ignored.
            // A stray `qualifier` does not disturb the pending type because we
            // only clear `pending_type` when an `argument` consumes it.
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> ImportTable {
        let doc = Document::parse(src).expect("parse");
        import_table(&doc)
    }

    #[test]
    fn versioned_import_with_alias_and_source() {
        let src = "//@version=6\n/// @source ./libs/my-lib.pine\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        let entry = &t.entries()[0];
        assert_eq!(entry.alias.as_deref(), Some("myLib"));
        assert_eq!(entry.user, "User");
        assert_eq!(entry.lib, "MyLib");
        assert_eq!(entry.version, "1");
        assert_eq!(entry.source.as_deref(), Some("./libs/my-lib.pine"));
    }

    #[test]
    fn import_with_alias_but_no_directive() {
        let src = "//@version=6\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        let entry = &t.entries()[0];
        assert_eq!(entry.alias.as_deref(), Some("myLib"));
        assert_eq!(entry.source, None);
    }

    #[test]
    fn import_with_no_alias() {
        let src = "//@version=6\nimport TV/Strategy/2\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        let entry = &t.entries()[0];
        assert_eq!(entry.alias, None);
        assert_eq!(entry.user, "TV");
        assert_eq!(entry.lib, "Strategy");
        assert_eq!(entry.version, "2");
        assert_eq!(entry.source, None);
        // Effective namespace falls back to the lib name.
        assert_eq!(entry.effective_namespace(), "Strategy");
    }

    #[test]
    fn directive_not_on_previous_line_does_not_associate() {
        // Blank line between the directive and the import => strict rule rejects.
        let src = "//@version=6\n/// @source ./libs/my-lib.pine\n\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries()[0].source, None);
    }

    #[test]
    fn unrelated_line_between_directive_and_import() {
        let src =
            "//@version=6\n/// @source ./libs/my-lib.pine\nx = 1\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries()[0].source, None);
    }

    #[test]
    fn multiple_imports_each_correctly_associated() {
        let src = concat!(
            "//@version=6\n",
            "/// @source ./libs/a.pine\n",
            "import User/LibA/1 as a\n",
            "/// @source ./libs/b.pine\n",
            "import User/LibB/2 as b\n",
        );
        let t = table(src);
        assert_eq!(t.len(), 2);
        let a = t.by_alias("a").expect("a");
        let b = t.by_alias("b").expect("b");
        assert_eq!(a.source.as_deref(), Some("./libs/a.pine"));
        assert_eq!(a.lib, "LibA");
        assert_eq!(b.source.as_deref(), Some("./libs/b.pine"));
        assert_eq!(b.lib, "LibB");
        // No cross-association.
        assert_ne!(a.source, b.source);
    }

    #[test]
    fn malformed_import_missing_version_produces_no_entries() {
        // `import User/Lib` (no version) yields an ERROR node and no import node.
        let src = "//@version=6\nimport User/Lib\n";
        let t = table(src);
        assert!(
            t.is_empty(),
            "must not fabricate an entry from an ERROR node"
        );
    }

    #[test]
    fn dangling_as_produces_no_entries() {
        let src = "//@version=6\nimport User/Lib/1 as\n";
        let t = table(src);
        assert!(t.is_empty(), "dangling `as` must not panic or fabricate");
    }

    #[test]
    fn import_on_first_line_no_underflow() {
        // Import on row 0 — there is no possible previous line for a directive.
        let src = "import User/MyLib/1 as myLib\n";
        let t = table(src);
        // The grammar may or may not accept an import without a //@version line;
        // either way this must not underflow. If it parsed, source is None.
        for entry in t.entries() {
            assert_eq!(entry.line, 0);
            assert_eq!(entry.source, None);
        }
    }

    #[test]
    fn by_alias_lookup() {
        let src = "//@version=6\nimport User/MyLib/1 as myLib\nimport TV/Strategy/2\n";
        let t = table(src);
        assert!(t.by_alias("myLib").is_some());
        assert!(t.by_alias("nope").is_none());
        // An aliasless import is not matched by its lib name.
        assert!(t.by_alias("Strategy").is_none());
    }

    #[test]
    fn plain_comments_are_not_source_directives() {
        // A plain `//` comment, the `//@version=6` annotation, and a `///` doc
        // comment that isn't `@source` must all be ignored.
        let src = concat!(
            "//@version=6\n",
            "// @source ./not-a-directive.pine\n", // only two slashes
            "/// just a doc comment\n",
            "import User/MyLib/1 as myLib\n",
        );
        let t = table(src);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries()[0].source, None);
    }

    #[test]
    fn empty_document_has_no_imports() {
        let src = "//@version=6\nindicator(\"Test\")\nplot(close)\n";
        let t = table(src);
        assert!(t.is_empty());
    }

    #[test]
    fn directive_with_leading_and_trailing_whitespace_is_trimmed() {
        // Leading indentation + trailing whitespace (and a CR) must be trimmed,
        // matching the TS `^\s*...\s*$`.
        let src = "//@version=6\n   ///   @source   ./libs/my-lib.pine   \r\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries()[0].source.as_deref(), Some("./libs/my-lib.pine"));
    }

    #[test]
    fn at_source_without_path_is_rejected() {
        // `/// @source` with no path must not produce an empty source string.
        let src = "//@version=6\n/// @source\nimport User/MyLib/1 as myLib\n";
        let t = table(src);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries()[0].source, None);
    }

    // --- exported_symbols: fs-free extraction tests ------------------------

    fn exports(src: &str) -> Vec<ExportedSymbol> {
        let doc = Document::parse(src).expect("parse");
        exported_symbols(&doc)
    }

    #[test]
    fn exported_function_with_typed_and_defaulted_params() {
        let src = "//@version=6\nexport f(int a, float b = 1.0) =>\n    a + b\n";
        let syms = exports(src);
        assert_eq!(syms.len(), 1);
        let f = &syms[0];
        assert_eq!(f.name, "f");
        assert_eq!(f.kind, ExportKind::Function);
        assert_eq!(
            f.params,
            vec![
                ExportedParam {
                    name: "a".to_string(),
                    type_name: Some("int".to_string()),
                    has_default: false,
                },
                ExportedParam {
                    name: "b".to_string(),
                    type_name: Some("float".to_string()),
                    has_default: true,
                },
            ]
        );
    }

    #[test]
    fn non_exported_function_is_not_collected() {
        let src = "//@version=6\ng(c) =>\n    c * 2\n";
        let syms = exports(src);
        assert!(
            syms.is_empty(),
            "a function without the `export` token must not be collected"
        );
    }

    #[test]
    fn exported_method_with_qualified_types() {
        let src = "//@version=6\nexport method scale(series float x, simple int n) =>\n    x * n\n";
        let syms = exports(src);
        assert_eq!(syms.len(), 1);
        let m = &syms[0];
        assert_eq!(m.name, "scale");
        assert_eq!(m.kind, ExportKind::Method);
        // Qualifier (`series`/`simple`) must not break type/name extraction.
        assert_eq!(m.params.len(), 2);
        assert_eq!(m.params[0].name, "x");
        assert_eq!(m.params[0].type_name.as_deref(), Some("float"));
        assert_eq!(m.params[1].name, "n");
        assert_eq!(m.params[1].type_name.as_deref(), Some("int"));
    }

    #[test]
    fn typeless_param_is_not_index_zipped_to_wrong_type() {
        // `f(int a, c)`: 2 args, 1 type. Index-zipping would give `c` the type
        // `int`; the ordered walk must instead leave `c` typeless.
        let src = "//@version=6\nexport f(int a, c) =>\n    a\n";
        let syms = exports(src);
        assert_eq!(syms.len(), 1);
        let params = &syms[0].params;
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "a");
        assert_eq!(params[0].type_name.as_deref(), Some("int"));
        assert_eq!(params[1].name, "c");
        assert_eq!(
            params[1].type_name, None,
            "typeless param must not inherit a sibling's type via index-zip"
        );
    }

    #[test]
    fn exported_type_and_enum_collected_with_empty_params() {
        let src = "//@version=6\nexport type Point\n    float x\n    float y\nexport enum Color\n    red\n    green\n";
        let syms = exports(src);
        assert_eq!(syms.len(), 2);
        let point = syms.iter().find(|s| s.name == "Point").expect("Point");
        assert_eq!(point.kind, ExportKind::Type);
        assert!(point.params.is_empty());
        let color = syms.iter().find(|s| s.name == "Color").expect("Color");
        assert_eq!(color.kind, ExportKind::Enum);
        assert!(color.params.is_empty());
    }

    #[test]
    fn non_exported_type_and_enum_excluded() {
        let src = "//@version=6\ntype Internal\n    int n\nenum Hidden\n    a\n    b\n";
        let syms = exports(src);
        assert!(
            syms.is_empty(),
            "type/enum without `export` must be excluded"
        );
    }

    #[test]
    fn zero_export_library_returns_empty_vec() {
        let src = "//@version=6\nindicator(\"x\")\nf(a) =>\n    a\nplot(close)\n";
        let syms = exports(src);
        assert!(syms.is_empty(), "no exports is not an error");
    }

    #[test]
    fn exported_function_name_span_points_at_the_name() {
        // The name byte span must slice exactly the function name out of the
        // SAME source the symbols were extracted from.
        let lib_src = "//@version=6\nexport add(int a, float b = 1.0) =>\n    a + b\n";
        let syms = exports(lib_src);
        assert_eq!(syms.len(), 1);
        let add = &syms[0];
        assert_eq!(
            &lib_src[add.name_byte_start..add.name_byte_end],
            "add",
            "fn name span must slice the name from the lib source"
        );
    }

    #[test]
    fn exported_method_name_span_points_at_the_name() {
        let lib_src = "//@version=6\nexport method scale(series float x) =>\n    x\n";
        let syms = exports(lib_src);
        assert_eq!(syms.len(), 1);
        let scale = &syms[0];
        assert_eq!(scale.kind, ExportKind::Method);
        assert_eq!(
            &lib_src[scale.name_byte_start..scale.name_byte_end],
            "scale"
        );
    }

    #[test]
    fn exported_type_and_enum_name_spans_point_at_the_names() {
        let lib_src = "//@version=6\nexport type Point\n    float x\nexport enum Color\n    red\n";
        let syms = exports(lib_src);
        let point = syms.iter().find(|s| s.name == "Point").expect("Point");
        assert_eq!(point.kind, ExportKind::Type);
        assert_eq!(
            &lib_src[point.name_byte_start..point.name_byte_end],
            "Point"
        );
        let color = syms.iter().find(|s| s.name == "Color").expect("Color");
        assert_eq!(color.kind, ExportKind::Enum);
        assert_eq!(
            &lib_src[color.name_byte_start..color.name_byte_end],
            "Color"
        );
    }
}
