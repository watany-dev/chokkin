//! Integration tests for source file discovery.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use yokei::{
    DiscoveredSources, FileContext, FileKind, ProjectLayout, ProjectRoot, RootMarker, SourcesError,
    SourcesWarning, discover_project_root, discover_sources, extract_manifest, load_config,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sources")
        .join(name)
}

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

fn discover_fixture(name: &str) -> DiscoveredSources {
    let path = fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| project_root_at(&path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    discover_sources(&root, &config, &manifest).expect("discover sources")
}

fn paths(sources: &DiscoveredSources) -> Vec<&str> {
    sources
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

#[test]
fn infers_src_layout() {
    let sources = discover_fixture("src_layout");
    assert_eq!(sources.layout.layout, ProjectLayout::Src);
    assert_eq!(sources.layout.packages, vec!["acme".to_owned()]);
    assert_eq!(
        sources.effective_globs,
        vec![
            "src/**/*.{py,pyi}".to_owned(),
            "tests/**/*.{py,pyi}".to_owned(),
            "scripts/**/*.{py,pyi}".to_owned(),
        ]
    );

    let paths = paths(&sources);
    assert!(paths.contains(&"src/acme/module.py"));
    assert!(paths.contains(&"tests/test_module.py"));
    assert!(paths.contains(&"scripts/run.py"));
    assert!(!paths.contains(&"docs/conf.py"));
}

#[test]
fn infers_flat_layout() {
    let sources = discover_fixture("flat_layout");
    assert_eq!(sources.layout.layout, ProjectLayout::Flat);
    assert_eq!(sources.layout.packages, vec!["acme".to_owned()]);
    assert!(paths(&sources).contains(&"acme/foo.py"));
}

#[test]
fn fallback_glob_when_no_layout() {
    let sources = discover_fixture("fallback_layout");
    assert_eq!(sources.layout.layout, ProjectLayout::Unknown);
    assert!(paths(&sources).contains(&"manage.py"));
}

#[test]
fn honors_explicit_project_globs() {
    let sources = discover_fixture("explicit_globs");
    assert_eq!(sources.effective_globs, vec!["custom/**/*.py".to_owned()]);
    let discovered = paths(&sources);
    assert!(discovered.contains(&"custom/only.py"));
    assert!(!discovered.contains(&"tests/test_other.py"));
}

#[test]
fn applies_exclude() {
    let sources = discover_fixture("exclude_tests");
    let discovered = paths(&sources);
    assert!(discovered.contains(&"src/acme/module.py"));
    assert!(!discovered.contains(&"tests/test_module.py"));
}

#[test]
fn respects_gitignore() {
    let sources = discover_fixture("gitignore_respected");
    let discovered = paths(&sources);
    assert!(discovered.contains(&"acme/visible.py"));
    assert!(!discovered.contains(&"local/hidden.py"));
}

#[test]
fn filters_production_contexts() {
    let sources = discover_fixture("production_mode");
    let discovered = paths(&sources);
    assert!(discovered.contains(&"src/acme/module.py"));
    assert!(!discovered.contains(&"tests/test_module.py"));
}

#[test]
fn assigns_test_context_for_conftest() {
    let sources = discover_fixture("src_layout");
    let by_path = |target: &str| {
        sources
            .files
            .iter()
            .find(|file| file.path == target)
            .map(|file| file.context)
    };
    assert_eq!(by_path("tests/conftest.py"), Some(FileContext::Test));
}

#[test]
fn includes_pyi_as_stub_kind() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root_path = temp.path();
    std::fs::create_dir_all(root_path.join("src/acme")).expect("create package");
    std::fs::write(root_path.join("src/acme/__init__.py"), "").expect("write init");
    std::fs::write(root_path.join("src/acme/module.py"), "").expect("write py");
    std::fs::write(root_path.join("src/acme/module.pyi"), "").expect("write pyi");
    std::fs::write(
        root_path.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n",
    )
    .expect("write pyproject");

    let root = discover_project_root(root_path).expect("discover root");
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");

    let stub = sources
        .files
        .iter()
        .find(|file| file.path == "src/acme/module.pyi")
        .expect("pyi file");
    assert_eq!(stub.kind, FileKind::Stub);
}

#[test]
fn assigns_file_context() {
    let sources = discover_fixture("src_layout");
    let by_path = |target: &str| {
        sources
            .files
            .iter()
            .find(|file| file.path == target)
            .map(|file| file.context)
            .expect("path")
    };
    assert_eq!(by_path("src/acme/module.py"), FileContext::Runtime);
    assert_eq!(by_path("tests/test_module.py"), FileContext::Test);
    assert_eq!(by_path("scripts/run.py"), FileContext::Dev);
}

#[test]
fn warns_missing_entry_path() {
    let sources = discover_fixture("missing_entry");
    assert!(sources.warnings.iter().any(|warning| matches!(
        warning,
        SourcesWarning::MissingEntryPath { path } if path == "missing.py"
    )));
}

#[test]
fn resolves_ambiguous_flat_layout_with_metadata_name() {
    let sources = discover_fixture("ambiguous_flat");
    assert_eq!(sources.layout.packages, vec!["acme".to_owned()]);
    assert!(paths(&sources).contains(&"acme/foo.py"));
    assert!(!paths(&sources).contains(&"other/bar.py"));
    assert!(sources.warnings.iter().any(|warning| matches!(
        warning,
        SourcesWarning::AmbiguousFlatLayout { chosen, .. } if chosen == "acme"
    )));
}

#[test]
fn excludes_nested_cache_and_venv_paths() {
    let sources = discover_fixture("nested_ignored");
    let discovered = paths(&sources);
    assert!(discovered.contains(&"src/acme/module.py"));
    assert!(!discovered.contains(&".venv/fake/ignored.py"));
    assert!(!discovered.contains(&"build/fake/ignored.py"));
}

#[test]
fn paths_use_forward_slashes() {
    let sources = discover_fixture("src_layout");
    assert!(sources.files.iter().all(|file| !file.path.contains('\\')));
}

#[test]
fn empty_project_returns_empty_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = project_root_at(temp.path());
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    assert!(sources.files.is_empty());
}

#[test]
fn invalid_glob_returns_error() {
    let path = fixture("src_layout");
    let root = discover_project_root(&path).expect("discover root");
    let mut config = load_config(&root).expect("load config");
    config.effective.project = vec!["src/[unclosed".to_owned()];
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let error = discover_sources(&root, &config, &manifest).expect_err("invalid glob");
    assert!(matches!(error, SourcesError::InvalidGlob { .. }));
}

#[test]
fn full_pipeline_step4() {
    let path = fixture("src_layout");
    let root = discover_project_root(&path).expect("discover root");
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    assert_eq!(sources.python_files().count(), 5);
    assert!(
        sources
            .files
            .iter()
            .any(|file| file.kind == FileKind::Python)
    );
    assert!(
        sources
            .files
            .iter()
            .any(|file| file.path == "tests/conftest.py" && file.context == FileContext::Test)
    );
}
