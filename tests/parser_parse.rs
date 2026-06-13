//! Integration tests for Python parsing (Phase 0 spike).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use yokei::{ImportKind, ProjectRoot, RootMarker, TargetVersion, parse_file};

fn spike_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser_spike")
        .join(name)
}

fn parse_spike(name: &str) -> yokei::ParsedModule {
    let path = spike_fixture(name);
    let root = ProjectRoot {
        path: path.parent().expect("parent").to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: path.parent().expect("parent").to_path_buf(),
    };
    parse_file(&root, name, TargetVersion::default_py311()).expect("parse")
}

#[test]
fn parses_basic_imports() {
    let parsed = parse_spike("p2_basic_imports.py");
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.imports.len(), 2);
    assert!(parsed.imports.iter().any(|import| import.module == "os"));
    assert!(
        parsed
            .imports
            .iter()
            .any(|import| import.module == "collections" && import.kind == ImportKind::ImportFrom)
    );
}

#[test]
fn skips_relative_import_module_name() {
    let parsed = parse_spike("p3_relative_import.py");
    assert!(parsed.imports.is_empty());
}

#[test]
fn collects_try_block_import() {
    let parsed = parse_spike("p5_try_import.py");
    assert!(
        parsed
            .imports
            .iter()
            .any(|import| import.module == "orjson")
    );
}

#[test]
fn collects_type_checking_import() {
    let parsed = parse_spike("p4_type_checking.py");
    assert!(
        parsed
            .imports
            .iter()
            .any(|import| import.module == "pandas")
    );
}

#[test]
fn parses_match_statement_file() {
    let parsed = parse_spike("p7_match.py");
    assert!(parsed.diagnostics.is_empty());
}

#[test]
fn syntax_error_yields_diagnostic() {
    let parsed = parse_spike("p9_syntax_error.py");
    assert!(parsed.imports.is_empty());
    assert_eq!(parsed.diagnostics.len(), 1);
}

#[test]
fn spike_success_rate_meets_phase0_bar() {
    let fixtures = [
        "p1_empty_init.py",
        "p2_basic_imports.py",
        "p3_relative_import.py",
        "p4_type_checking.py",
        "p5_try_import.py",
        "p6_fstring.py",
        "p7_match.py",
        "p8_ignore_comment.py",
        "p9_syntax_error.py",
    ];
    let mut successes = 0_u32;
    for name in fixtures {
        let parsed = parse_spike(name);
        let ok = parsed.diagnostics.is_empty() || name == "p9_syntax_error.py";
        if ok {
            successes += 1;
        }
    }
    let rate = f64::from(successes) / f64::from(u32::try_from(fixtures.len()).expect("fits"));
    assert!(
        rate >= 0.95,
        "parser spike success rate {rate} below 95% bar"
    );
}
