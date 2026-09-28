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
        local_packages: Vec::new(),
        inferred_globs: Vec::new(),
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
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
        },
        _ => LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
fn collects_type_checking_alias_import_context() {
    let parsed = parse_fixture_dir("imports", "type_checking_block.py");
    for module in ["httpx", "boto3"] {
        let import = parsed
            .imports
            .iter()
            .find(|import| import.module == module)
            .unwrap_or_else(|| panic!("missing {module}"));
        assert_eq!(import.context, ImportContext::Type);
    }
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
fn spike_fixtures_report_errors_only_for_syntax_error() {
    let fixtures = [
        ("p1_empty_init.py", 0),
        ("p2_basic_imports.py", 0),
        ("p3_relative_import.py", 0),
        ("p4_type_checking.py", 0),
        ("p5_try_import.py", 0),
        ("p6_fstring.py", 0),
        ("p7_match.py", 0),
        ("p8_ignore_comment.py", 0),
        ("p9_syntax_error.py", 1),
    ];
    for (name, expected_errors) in fixtures {
        let parsed = parse_fixture(name);
        let errors = parsed
            .diagnostics
            .iter()
            .filter(|diag| diag.severity == ParseSeverity::Error)
            .count();
        assert_eq!(errors, expected_errors, "{name}: {:?}", parsed.diagnostics);
    }
}

#[test]
fn parse_project_sources_fixture_suite() {
    // Root at `imports/` so `src/acme/...` maps to `acme...` and relative imports resolve.
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse/imports");
    let root = ProjectRoot {
        path: base.clone(),
        marker: RootMarker::PyProjectToml,
        start: base.clone(),
    };
    let mut files = Vec::new();
    collect_py_files(&base, &base, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
        },
        effective_globs: Vec::new(),
        files,
        warnings: Vec::new(),
    };

    let summary =
        parse_project_sources(&root, &sources, &TargetVersion::default_py311()).expect("parse");
    let actual: Vec<_> = summary
        .modules
        .iter()
        .map(|module| {
            assert!(
                module.diagnostics.is_empty(),
                "{}: {:?}",
                module.path,
                module.diagnostics
            );
            let imports: Vec<_> = module
                .imports
                .iter()
                .map(|import| (import.module.as_str(), import.name.as_deref(), import.line))
                .collect();
            (module.path.as_str(), imports)
        })
        .collect();
    assert_eq!(
        actual,
        [
            ("absolute_import.py", vec![("acme", Some("util"), 1)]),
            (
                "src/acme/__init__.py",
                vec![("os", None, 1), ("json", None, 2)]
            ),
            // Relative `from . import util` is normalized to the submodule itself.
            ("src/acme/api/__init__.py", vec![("acme.api.util", None, 1)]),
            (
                "src/acme/api/routes.py",
                vec![("acme.models", Some("User"), 1)]
            ),
            ("src/acme/api/util.py", vec![]),
            ("src/acme/models.py", vec![]),
            (
                "try_optional_import.py",
                vec![("ujson", None, 2), ("json", None, 4)]
            ),
            (
                "type_checking_block.py",
                vec![
                    ("typing", Some("TYPE_CHECKING"), 1),
                    ("typing", None, 2),
                    ("httpx", None, 5),
                    ("boto3", None, 8),
                ]
            ),
        ]
    );
}

#[test]
fn parse_project_sources_reuses_cache_when_inputs_match() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse");
    let root = ProjectRoot {
        path: base.clone(),
        marker: RootMarker::PyProjectToml,
        start: base,
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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

    let first = parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
        .expect("parse");
    let second = parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
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
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("mkdir");
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
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
    let second = parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
        .expect("second parse");

    let module = second.modules.first().expect("parsed module");
    assert!(module.imports.iter().any(|import| import.module == "yaml"));
    assert!(
        !module
            .imports
            .iter()
            .any(|import| import.module == "requests")
    );
    assert_eq!(cache.stats().misses, 2);
    assert_eq!(cache.stats().hits, 0);
}

fn warns_type_alias(summary: &chokkin::ParseSummary) -> bool {
    summary
        .modules
        .first()
        .expect("parsed module")
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("`type` aliases"))
}

#[test]
fn parse_cache_follows_pep723_block_edits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_path = temp.path().join("scripts/run.py");
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("mkdir");
    let root = ProjectRoot {
        path: temp.path().to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: temp.path().to_path_buf(),
    };
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
        },
        effective_globs: Vec::new(),
        files: vec![chokkin::DiscoveredFile {
            path: "scripts/run.py".to_owned(),
            kind: chokkin::FileKind::Python,
            context: FileContext::Dev,
        }],
        warnings: Vec::new(),
    };
    let target = TargetVersion::default_py311();
    let mut cache = ParseCacheStore::new();
    let body = "type Alias = int\n";
    let block =
        |requires: &str| format!("# /// script\n# requires-python = \"{requires}\"\n# ///\n{body}");
    let mut parse_with = |contents: &str| {
        std::fs::write(&source_path, contents).expect("write source");
        parse_project_sources_with_cache(&root, &sources, &target, Some(&mut cache), None)
            .expect("parse")
    };

    assert!(warns_type_alias(&parse_with(body)), "project target py311");
    assert!(
        !warns_type_alias(&parse_with(&block(">=3.12"))),
        "added block raises the script target"
    );
    assert!(
        warns_type_alias(&parse_with(&block(">=3.9"))),
        "edited block lowers it again"
    );
    assert!(
        !warns_type_alias(&parse_with(&block(">=3.12"))),
        "edited back"
    );
    assert!(
        warns_type_alias(&parse_with(body)),
        "removed block falls back to the project target"
    );
}

#[test]
fn parse_project_sources_extracts_notebook_code_cells() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root_path = temp.path();
    std::fs::write(
        root_path.join("analysis.ipynb"),
        r##"{
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
}"##,
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
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
    assert_eq!(summary.modules.len(), 1);
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
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
    assert_eq!(summary.modules.len(), 1);
    let module = summary.modules.first().expect("module");
    assert!(
        module
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != ParseSeverity::Error)
    );
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

#[test]
fn disk_parse_cache_writes_one_bundle_for_the_whole_project() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("mkdir");
    let root = ProjectRoot {
        path: temp.path().to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: temp.path().to_path_buf(),
    };
    let mut files = Vec::new();
    for index in 0..8 {
        let path = format!("src/mod_{index}.py");
        std::fs::write(temp.path().join(&path), "import requests\n").expect("write source");
        files.push(chokkin::DiscoveredFile {
            path,
            kind: chokkin::FileKind::Python,
            context: FileContext::Runtime,
        });
    }
    let sources = chokkin::DiscoveredSources {
        root: root.clone(),
        layout: LayoutInfo {
            layout: ProjectLayout::Src,
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
        },
        effective_globs: Vec::new(),
        files,
        warnings: Vec::new(),
    };
    let target = TargetVersion::default_py311();
    let cache_options = chokkin::CacheOptions::default();

    let cold =
        parse_project_sources_with_cache(&root, &sources, &target, None, Some(&cache_options))
            .expect("cold parse");

    let parse_dir = temp.path().join(".chokkin/cache/parse");
    let entries: Vec<_> = std::fs::read_dir(&parse_dir)
        .expect("read parse cache dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "8 sources must share one bundle, found {entries:?}"
    );

    let mut store = ParseCacheStore::new();
    let warm = parse_project_sources_with_cache(
        &root,
        &sources,
        &target,
        Some(&mut store),
        Some(&cache_options),
    )
    .expect("warm parse");

    assert_eq!(cold, warm);
    assert_eq!(store.stats().hits, 8, "every module came off the bundle");
    assert_eq!(store.stats().misses, 0);
    assert_eq!(store.stats().stores, 0);
}

#[test]
fn disk_parse_cache_drops_entries_for_vanished_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("mkdir");
    let root = ProjectRoot {
        path: temp.path().to_path_buf(),
        marker: RootMarker::PyProjectToml,
        start: temp.path().to_path_buf(),
    };
    let layout = LayoutInfo {
        layout: ProjectLayout::Src,
        packages: Vec::new(),
        local_packages: Vec::new(),
        inferred_globs: Vec::new(),
    };
    let discovered = |path: &str| chokkin::DiscoveredFile {
        path: path.to_owned(),
        kind: chokkin::FileKind::Python,
        context: FileContext::Runtime,
    };
    for path in ["src/kept.py", "src/gone.py"] {
        std::fs::write(temp.path().join(path), "import requests\n").expect("write source");
    }
    let target = TargetVersion::default_py311();
    let cache_options = chokkin::CacheOptions::default();

    let both = chokkin::DiscoveredSources {
        root: root.clone(),
        layout,
        effective_globs: Vec::new(),
        files: vec![discovered("src/kept.py"), discovered("src/gone.py")],
        warnings: Vec::new(),
    };
    parse_project_sources_with_cache(&root, &both, &target, None, Some(&cache_options))
        .expect("first parse");

    let one = chokkin::DiscoveredSources {
        files: vec![discovered("src/kept.py")],
        ..both
    };
    std::fs::remove_file(temp.path().join("src/gone.py")).expect("remove source");
    parse_project_sources_with_cache(&root, &one, &target, None, Some(&cache_options))
        .expect("second parse");

    let parse_dir = temp.path().join(".chokkin/cache/parse");
    let bundle_path = std::fs::read_dir(&parse_dir)
        .expect("read parse cache dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .next()
        .expect("bundle file");
    let bundle = std::fs::read_to_string(&bundle_path).expect("read bundle");
    assert!(bundle.contains("src/kept.py"));
    assert!(
        !bundle.contains("src/gone.py"),
        "the removed source must be pruned from the bundle"
    );
}
