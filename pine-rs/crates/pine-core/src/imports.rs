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
    if let Some(entry) = is_named_import.then(|| parse_import_node(node, src)).flatten() {
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
        assert!(t.is_empty(), "must not fabricate an entry from an ERROR node");
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
}
