//! Integration tests for the bounded library-source resolver.
//!
//! These use committed fixture libs under `tests/fixtures/libs/` resolved
//! relative to `CARGO_MANIFEST_DIR` (matching the crate's other fixture-based
//! tests), so they are deterministic and create no temp files.

use std::path::PathBuf;

use pine_core::imports::{
    import_table, resolve_imports, ExportKind, ImportResolution,
};
use pine_core::Document;

/// The committed fixture-lib directory used as the resolver's `base_dir`.
fn libs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libs")
}

/// Build an [`ImportTable`] from a Pine source snippet.
fn table(src: &str) -> pine_core::imports::ImportTable {
    let doc = Document::parse(src).expect("parse main doc");
    import_table(&doc)
}

#[test]
fn published_import_without_source_is_unresolved_not_error() {
    // No `/// @source` directive: the common published-import case.
    let src = "//@version=6\nimport User/MyLib/1 as myLib\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    assert_eq!(resolved.len(), 1);
    let my_lib = resolved.by_alias("myLib").expect("myLib");
    assert!(
        matches!(my_lib.resolution, ImportResolution::Unresolved),
        "no @source must resolve to Unresolved (not an error), got {:?}",
        my_lib.resolution
    );
}

#[test]
fn local_source_resolves_to_expected_exported_symbols() {
    let src = "//@version=6\n/// @source math_utils.pine\nimport User/MathUtils/1 as mu\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let mu = resolved.by_alias("mu").expect("mu");
    let ImportResolution::Resolved { symbols, path } = &mu.resolution else {
        panic!("expected Resolved, got {:?}", mu.resolution);
    };

    // The resolved path is the canonical absolute lib file.
    assert!(path.is_absolute(), "resolved path must be absolute: {path:?}");
    assert!(
        path.ends_with("math_utils.pine"),
        "resolved path must point at the lib file: {path:?}"
    );

    // Only the two exported functions; `helper` (non-exported) is excluded.
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["add", "clamp"]);

    let add = symbols.iter().find(|s| s.name == "add").expect("add");
    assert_eq!(add.kind, ExportKind::Function);
    assert_eq!(add.params.len(), 2);
    assert_eq!(add.params[0].name, "a");
    assert_eq!(add.params[0].type_name.as_deref(), Some("int"));
    assert!(!add.params[0].has_default);
    assert_eq!(add.params[1].name, "b");
    assert_eq!(add.params[1].type_name.as_deref(), Some("float"));
    assert!(add.params[1].has_default);
}

#[test]
fn local_source_resolves_method_and_types() {
    let src = concat!(
        "//@version=6\n",
        "/// @source with_method.pine\n",
        "import User/WithMethod/1 as wm\n",
        "/// @source types_and_enum.pine\n",
        "import User/TypesEnum/1 as te\n",
    );
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let wm = resolved.by_alias("wm").expect("wm");
    let ImportResolution::Resolved { symbols: wm_syms, .. } = &wm.resolution else {
        panic!("expected Resolved for wm, got {:?}", wm.resolution);
    };
    assert_eq!(wm_syms.len(), 1);
    assert_eq!(wm_syms[0].name, "scale");
    assert_eq!(wm_syms[0].kind, ExportKind::Method);

    let te = resolved.by_alias("te").expect("te");
    let ImportResolution::Resolved { symbols: te_syms, .. } = &te.resolution else {
        panic!("expected Resolved for te, got {:?}", te.resolution);
    };
    // Exported Point (Type) + Color (Enum); Internal/Hidden excluded.
    let names: Vec<&str> = te_syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Point", "Color"]);
    assert_eq!(te_syms[0].kind, ExportKind::Type);
    assert_eq!(te_syms[1].kind, ExportKind::Enum);
}

#[test]
fn missing_file_resolves_to_not_found_without_panic() {
    let src = "//@version=6\n/// @source does_not_exist.pine\nimport User/Missing/1 as m\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let m = resolved.by_alias("m").expect("m");
    assert!(
        matches!(m.resolution, ImportResolution::NotFound),
        "missing file must be NotFound, got {:?}",
        m.resolution
    );
}

#[test]
fn path_traversal_outside_base_dir_is_refused() {
    // `../../` escapes the fixtures dir; even if the target exists, the
    // canonical prefix check must refuse it -> NotFound.
    let src = "//@version=6\n/// @source ../../Cargo.toml\nimport User/Escape/1 as e\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let e = resolved.by_alias("e").expect("e");
    assert!(
        matches!(e.resolution, ImportResolution::NotFound),
        "path traversal must be refused, got {:?}",
        e.resolution
    );
}

#[test]
fn absolute_source_path_is_refused() {
    let src =
        "//@version=6\n/// @source /etc/hosts\nimport User/Abs/1 as a\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let a = resolved.by_alias("a").expect("a");
    assert!(
        matches!(a.resolution, ImportResolution::NotFound),
        "absolute path must be refused, got {:?}",
        a.resolution
    );
}

#[test]
fn lib_with_parse_errors_still_recovers_exports() {
    // Write a temp file? No — fixtures must be committed and deterministic.
    // Instead exercise the recovery path through `Document::parse` directly via
    // a committed fixture that contains a syntactically broken trailing line
    // but a recoverable exported function. We synthesize this with a fixture
    // that has a dangling construct after a valid export.
    //
    // `error_recovery.pine` is committed alongside the other libs.
    let src = "//@version=6\n/// @source error_recovery.pine\nimport User/Broken/1 as b\n";
    let table = table(src);
    let resolved = resolve_imports(&table, &libs_dir());

    let b = resolved.by_alias("b").expect("b");
    let ImportResolution::Resolved { symbols: syms, .. } = &b.resolution else {
        panic!(
            "a lib with ERROR nodes must still Resolve best-effort, got {:?}",
            b.resolution
        );
    };
    // The recoverable export must be present; no panic on the error node.
    assert!(
        syms.iter().any(|s| s.name == "ok"),
        "expected to recover the `ok` export from a partially-broken lib"
    );
}
