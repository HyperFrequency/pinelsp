//! Dev tool: print the tree-sitter s-expression for a Pine snippet (argv[1] or
//! a default sample). Used to discover node kinds/fields when wiring features.

use pine_core::Document;

fn main() {
    let src = std::env::args().nth(1).unwrap_or_else(|| {
        "//@version=6\nindicator(\"x\")\nlen = input.int(14)\nf(a, b) =>\n    a + b\nz = ta.sma(close, len)\nplot(z)\n".to_string()
    });
    let doc = Document::parse(&src).unwrap();
    println!("{}", doc.root().to_sexp());
}
