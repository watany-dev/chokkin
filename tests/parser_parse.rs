//! Integration tests for Python parsing (Step 6).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use chokkin::{
    FileContext, ImportContext, ImportKind, LayoutInfo, ParseCacheStore, ParseSeverity,
    ProjectLayout, ProjectRoot, RootMarker, TargetVersion, parse_file, parse_project_sources,
    parse_project_sources_with_cache,
};

fn spike_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser_spike")
        .join(name)
}

fn parse_fixture(name: &str) -> chokkin::ParsedModule {
    let path = spike_fixture(name);
    let root = ProjectRoot {
        path: path.parent().expect("parent").to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: path.parent().expect("parent").to_path_buf(),
    };
    let layout = LayoutInfo {
        layout: ProjectLayout::Unknown,
        packages: Vec::new(),
        inferred_globs: Vec::new(),
        flat_candidates: Vec::new(),
        ambiguous_flat_resolution: false,
    };
    parse_file(
        &root,
        name,
        &layout,
        FileContext::Runtime,
        &TargetVersion::default_py311(),
    )
    .expect("parse")
}

fn parse_fixture_dir(dir: &str, name: &str) -> chokkin::ParsedModule {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parse")
        .join(dir);
    let root = ProjectRoot {
        path: base.clone(),
        marker: RootMarker::PyProjectToml,
        start: base,
    };
    let layout = match dir {
        "imports" => LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        _ => LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
    };
    parse_file(
        &root,
        name,
        &layout,
        FileContext::Runtime,
        &TargetVersion::default_py311(),
    )
    .expect("parse")
}

#[test]
fn parses_basic_imports() {
    let parsed = parse_fixture("p2_basic_imports.py");
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
fn relative_import_without_package_emits_warning() {
    let parsed = parse_fixture("p3_relative_import.py");
    assert_eq!(parsed.imports.len(), 1);
    assert!(parsed.imports[0].module.is_empty());
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("relative"))
    );
}

#[test]
fn collects_try_block_import_as_optional() {
    let parsed = parse_fixture("p5_try_import.py");
    let orjson = parsed
        .imports
        .iter()
        .find(|import| import.module == "orjson")
        .expect("orjson");
    assert!(orjson.optional);
}

#[test]
fn collects_type_checking_import_context() {
    let parsed = parse_fixture("p4_type_checking.py");
    let pandas = parsed
        .imports
        .iter()
        .find(|import| import.module == "pandas")
        .expect("pandas");
    assert_eq!(pandas.context, ImportContext::Type);
}

#[test]
fn parses_match_statement_file() {
    let parsed = parse_fixture("p7_match.py");
    assert!(parsed.diagnostics.is_empty());
}

#[test]
fn syntax_error_yields_diagnostic() {
    let parsed = parse_fixture("p9_syntax_error.py");
    assert!(parsed.imports.is_empty());
    assert_eq!(parsed.diagnostics.len(), 1);
}

#[test]
fn extracts_inline_ignore_directive() {
    let parsed = parse_fixture("p8_ignore_comment.py");
    assert_eq!(parsed.ignores.len(), 1);
    assert_eq!(parsed.ignores[0].codes, vec!["CHK003".to_owned()]);
}

#[test]
fn resolves_relative_import_in_src_layout() {
    let parsed = parse_fixture_dir("imports", "src/acme/api/routes.py");
    let models = parsed
        .imports
        .iter()
        .find(|import| import.name.as_deref() == Some("User"))
        .expect("User import");
    assert_eq!(models.module, "acme.models");
}

#[test]
fn extracts_dynamic_import_literal() {
    let parsed = parse_fixture_dir("dynamic", "importlib_literal.py");
    assert_eq!(parsed.dynamic_imports.len(), 1);
    assert_eq!(parsed.dynamic_imports[0].module, "acme.plugins");
}

#[test]
fn extracts_all_exports() {
    let parsed = parse_fixture_dir("exports", "all_list.py");
    assert_eq!(parsed.exports, vec!["foo".to_owned(), "bar".to_owned()]);
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
        let parsed = parse_fixture(name);
        let ok = !parsed
            .diagnostics
            .iter()
            .any(|diag| diag.severity == ParseSeverity::Error)
            || name == "p9_syntax_error.py";
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

#[test]
fn parse_project_sources_fixture_suite() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse");
    let root = ProjectRoot {
        path: base.clone(),
        marker: RootMarker::PyProjectToml,
        start: base.clone(),
    };
    let mut files = Vec::new();
    collect_py_files(&base.join("imports"), &base, &mut files);
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        effective_globs: Vec::new(),
        files,
        warnings: Vec::new(),
    };

    let summary =
        parse_project_sources(&root, &sources, &TargetVersion::default_py311()).expect("parse");
    assert!(summary.parsed_count >= 3);
    assert_eq!(summary.skipped_count, 0);
}

#[test]
fn parse_project_sources_reuses_cache_when_inputs_match() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse");
    let root = ProjectRoot {
        path: base.clone(),
        marker: RootMarker::PyProjectToml,
        start: base.clone(),
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        effective_globs: Vec::new(),
        files: vec![chokkin::DiscoveredFile {
            path: "imports/absolute_import.py".to_owned(),
            kind: chokkin::FileKind::Python,
            context: FileContext::Runtime,
        }],
        warnings: Vec::new(),
    };
    let target = TargetVersion::default_py311();
    let mut cache = ParseCacheStore::new();

    let first =
        parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
            .expect("parse");
    let second =
        parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
            .expect("parse");

    assert_eq!(first, second);
    assert_eq!(cache.stats().misses, 1);
    assert_eq!(cache.stats().stores, 1);
    assert_eq!(cache.stats().hits, 1);
}

#[test]
fn parse_project_sources_invalidates_cache_when_source_changes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_path = temp.path().join("src/app.py");
    std::fs::create_dir_all(source_path.parent().expect("source parent"))
        .expect("mkdir");
    std::fs::write(&source_path, "import requests\n").expect("write first source");
    let root = ProjectRoot {
        path: temp.path().to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: temp.path().to_path_buf(),
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["app".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        effective_globs: Vec::new(),
        files: vec![chokkin::DiscoveredFile {
            path: "src/app.py".to_owned(),
            kind: chokkin::FileKind::Python,
            context: FileContext::Runtime,
        }],
        warnings: Vec::new(),
    };
    let target = TargetVersion::default_py311();
    let mut cache = ParseCacheStore::new();

    parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
        .expect("first parse");
    std::fs::write(&source_path, "import yaml\n").expect("write second source");
    let second =
        parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
            .expect("second parse");

    let module = second.modules.first().expect("parsed module");
    assert!(module.imports.iter().any(|import| import.module == "yaml"));
    assert!(!module.imports.iter().any(|import| import.module == "requests"));
    assert_eq!(cache.stats().misses, 2);
    assert_eq!(cache.stats().hits, 0);
}

#[test]
fn parse_project_sources_extracts_notebook_code_cells() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root_path = temp.path();
    std::fs::write(
        root_path.join("analysis.ipynb"),
        r#"{
  "cells": [
    {
      "cell_type": "markdown",
      "source": ["# ignored\n"]
    },
    {
      "cell_type": "code",
      "source": ["import pandas as pd\n", "from pathlib import Path\n"]
    }
  ],
  "metadata": {},
  "nbformat": 4,
  "nbformat_minor": 5
}"#,
    )
    .expect("write notebook");
    let root = ProjectRoot {
        path: root_path.to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: root_path.to_path_buf(),
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        effective_globs: Vec::new(),
        files: vec![chokkin::DiscoveredFile {
            path: "analysis.ipynb".to_owned(),
            kind: chokkin::FileKind::Notebook,
            context: FileContext::Runtime,
        }],
        warnings: Vec::new(),
    };

    let summary =
        parse_project_sources(&root, &sources, &TargetVersion::default_py311()).expect("parse");
    assert_eq!(summary.parsed_count, 1);
    assert_eq!(summary.skipped_count, 0);
    let module = summary.modules.first().expect("module");
    assert_eq!(module.path, "analysis.ipynb");
    assert!(
        module
            .imports
            .iter()
            .any(|import| import.module == "pandas")
    );
    assert!(
        module
            .imports
            .iter()
            .any(|import| import.module == "pathlib")
    );
}

#[test]
fn parse_project_sources_reports_invalid_notebook_as_warning() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root_path = temp.path();
    std::fs::write(root_path.join("broken.ipynb"), "not json").expect("write notebook");
    let root = ProjectRoot {
        path: root_path.to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: root_path.to_path_buf(),
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        },
        effective_globs: Vec::new(),
        files: vec![chokkin::DiscoveredFile {
            path: "broken.ipynb".to_owned(),
            kind: chokkin::FileKind::Notebook,
            context: FileContext::Runtime,
        }],
        warnings: Vec::new(),
    };

    let summary =
        parse_project_sources(&root, &sources, &TargetVersion::default_py311()).expect("parse");
    assert_eq!(summary.parsed_count, 1);
    assert_eq!(summary.error_count, 0);
    let module = summary.modules.first().expect("module");
    assert!(module.imports.is_empty());
    assert!(module.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == ParseSeverity::Warning
            && diagnostic.message.contains("invalid notebook JSON")
    }));
}

fn collect_py_files(
    dir: &std::path::Path,
    base: &std::path::Path,
    out: &mut Vec<chokkin::DiscoveredFile>,
) {
    let entries = std::fs::read_dir(dir).expect("read dir");
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_py_files(&path, base, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "py") {
            let rel = path
                .strip_prefix(base)
                .expect("strip")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(chokkin::DiscoveredFile {
                path: rel,
                kind: chokkin::FileKind::Python,
                context: FileContext::Runtime,
            });
        }
    }
}
