//! User-symbol extraction over the CST: definitions, the identifier under a
//! cursor, and same-name references.
//!
//! This is name-based, not yet scope-resolved — a local parameter and a global
//! variable that share a name are treated as the same symbol. That's good enough
//! for go-to-definition / references / rename on typical Pine scripts; real
//! lexical scoping is a later refinement.

use crate::Document;
use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Variable,
    Parameter,
    Type,
    Enum,
}

#[derive(Debug, Clone)]
pub struct SymbolDef {
    pub name: String,
    pub kind: SymbolKind,
    /// Byte range of the defining *name* identifier.
    pub start_byte: usize,
    pub end_byte: usize,
}

/// All user symbol definitions: top-level variables/functions/types/enums plus
/// function parameters.
pub fn definitions(doc: &Document) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    collect_defs(doc.root(), doc.text(), &mut out);
    out
}

fn collect_defs(node: Node, src: &str, out: &mut Vec<SymbolDef>) {
    match node.kind() {
        // `x = expr` and `x = switch/if ...` (the latter is a *_statement with an
        // `initial_structure` RHS); both bind the `variable` field.
        "variable_definition" | "variable_definition_statement" => {
            if let Some(id) = node.child_by_field_name("variable") {
                push(out, id, src, SymbolKind::Variable);
            }
        }
        "function_declaration_statement" => {
            // Regular functions use the `function` field; methods use `method`.
            if let Some(id) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("method"))
            {
                push(out, id, src, SymbolKind::Function);
            }
            let mut cursor = node.walk();
            for arg in node.children_by_field_name("argument", &mut cursor) {
                push(out, arg, src, SymbolKind::Parameter);
            }
        }
        "for_statement" => {
            if let Some(id) = node.child_by_field_name("counter") {
                push(out, id, src, SymbolKind::Variable);
            }
        }
        "import" => {
            if let Some(id) = node.child_by_field_name("alias") {
                push(out, id, src, SymbolKind::Variable);
            }
        }
        "type_definition_statement" => {
            if let Some(id) = node.child_by_field_name("name") {
                push(out, id, src, SymbolKind::Type);
            }
        }
        "enum_declaration" => {
            if let Some(id) = node.child_by_field_name("name") {
                push(out, id, src, SymbolKind::Enum);
            }
        }
        "tuple_declaration" | "tuple_declaration_statement" => {
            let mut cursor = node.walk();
            for var in node.children_by_field_name("variables", &mut cursor) {
                push(out, var, src, SymbolKind::Variable);
            }
        }
        "for_in_statement" => {
            if let Some(id) = node.child_by_field_name("array_element") {
                push(out, id, src, SymbolKind::Variable);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_defs(child, src, out);
    }
}

fn push(out: &mut Vec<SymbolDef>, id: Node, src: &str, kind: SymbolKind) {
    if id.kind() != "identifier" {
        return;
    }
    out.push(SymbolDef {
        name: src[id.start_byte()..id.end_byte()].to_string(),
        kind,
        start_byte: id.start_byte(),
        end_byte: id.end_byte(),
    });
}

/// The identifier `(name, start_byte, end_byte)` under `byte`, if the cursor is
/// on an `identifier` node.
pub fn identifier_at(doc: &Document, byte: usize) -> Option<(String, usize, usize)> {
    let node = doc.root().named_descendant_for_byte_range(byte, byte)?;
    if node.kind() != "identifier" {
        return None;
    }
    let src = doc.text();
    Some((
        src[node.start_byte()..node.end_byte()].to_string(),
        node.start_byte(),
        node.end_byte(),
    ))
}

/// Byte ranges of every `identifier` whose text == `name`, excluding the member
/// side of a member access (`obj.MEMBER` — only `obj` is a free reference).
pub fn references(doc: &Document, name: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    collect_refs(doc.root(), doc.text(), name, &mut out);
    out
}

fn collect_refs(node: Node, src: &str, name: &str, out: &mut Vec<(usize, usize)>) {
    if node.kind() == "identifier" && &src[node.start_byte()..node.end_byte()] == name {
        let is_member = node.parent().is_some_and(|p| {
            p.kind() == "attribute"
                && p.child_by_field_name("attribute").is_some_and(|a| a == node)
        });
        if !is_member {
            out.push((node.start_byte(), node.end_byte()));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_refs(child, src, name, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "//@version=6\nlen = 14\nf(a, b) =>\n    a + b\nz = f(len, 2)\n";

    fn doc() -> Document {
        Document::parse(SRC).unwrap()
    }

    #[test]
    fn finds_definitions() {
        let defs = definitions(&doc());
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"len"));
        assert!(names.contains(&"f"));
        assert!(names.contains(&"a")); // parameter
        assert!(names.contains(&"z"));
        assert!(defs.iter().any(|d| d.name == "f" && d.kind == SymbolKind::Function));
        assert!(defs.iter().any(|d| d.name == "a" && d.kind == SymbolKind::Parameter));
    }

    #[test]
    fn references_to_len() {
        let d = doc();
        // "len" appears at its definition and inside f(len, 2)
        let refs = references(&d, "len");
        assert_eq!(refs.len(), 2, "len defined once + used once");
    }

    #[test]
    fn collects_tuple_and_for_in_vars() {
        let d = Document::parse(
            "//@version=6\n[a, b] = ta.macd(close, 12, 26)\nfor item in array.new_float()\n    plot(item)\n",
        )
        .unwrap();
        let names: Vec<_> = definitions(&d).into_iter().map(|x| x.name).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"item".to_string()));
    }

    #[test]
    fn identifier_under_cursor() {
        let d = doc();
        // byte of the 'f' in "z = f(len, 2)" — find it
        let byte = SRC.rfind("f(len").unwrap();
        let (name, _, _) = identifier_at(&d, byte).unwrap();
        assert_eq!(name, "f");
    }
}
