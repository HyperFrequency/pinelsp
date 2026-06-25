//! Diagnostic: parse the TS syntax fixtures and, for each that fails, print the
//! first ERROR/MISSING node + a snippet. Used to scope the grammar merge — shows
//! which Pine constructs the current grammar can't parse. Throwaway tooling; not
//! a test.

use pine_core::Document;
use std::path::PathBuf;
use tree_sitter::Node;

fn find_first_error(node: Node) -> Option<Node> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(e) = find_first_error(child) {
            return Some(e);
        }
    }
    None
}

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/core/test/fixtures/syntax");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pine"))
        .collect();
    paths.sort();

    let mut fail = 0usize;
    for path in &paths {
        let src = std::fs::read_to_string(path).unwrap();
        let doc = Document::parse(src.clone()).unwrap();
        if !doc.has_errors() {
            continue;
        }
        fail += 1;
        let name = path.file_name().unwrap().to_string_lossy();
        match find_first_error(doc.root()) {
            Some(node) => {
                let end = node.end_byte().min(node.start_byte() + 70);
                let snippet = src[node.start_byte()..end].replace('\n', "\\n");
                println!(
                    "FAIL {name:40} {:>8} @ {:?} : {snippet:?}",
                    node.kind(),
                    node.start_position(),
                );
            }
            None => println!("FAIL {name:40} (has_error but no ERROR/MISSING node)"),
        }
    }
    println!("\n{fail}/{} fixtures fail", paths.len());
}
