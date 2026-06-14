//! Integration tests for manifest extraction.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use chokkin::{
    ConfigSources, DependencyContext, LoadedConfig, ManifestError, ManifestWarning, ProjectRoot,
    RootMarker, TargetVersion, default_config, discover_project_root, extract_manifest,
    load_config, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifest")
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

fn extract_fixture(name: &str) -> chokkin::LoadedManifest {
    let path = fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| project_root_at(&path));
    let config = load_config(&root).expect("load config");
    extract_manifest(&root, &config).expect("extract manifest")
}

fn dependency_names(manifest: &chokkin::LoadedManifest) -> Vec<&str> {
    manifest
        .dependencies
        .iter()
        .map(|dep| dep.name.as_str())
        .collect()
}

#[test]
fn extracts_project_dependencies() {
    let manifest = extract_fixture("pyproject_minimal");
    assert_eq!(manifest.metadata.name.as_deref(), Some("minimal"));
    assert!(
        manifest
            .dependencies
            .iter()
            .any(|dep| dep.name == "requests" && dep.context == DependencyContext::Runtime)
    );
}

#[test]
fn extracts_dependency_groups() {
    let manifest = extract_fixture("pyproject_full");
    assert!(manifest.dependencies.iter().any(|dep| {
        dep.name == "ruff" && dep.context == DependencyContext::Group("dev".to_owned())
    }));
}

#[test]
fn extracts_optional_dependencies() {
    let manifest = extract_fixture("pyproject_full");
    assert!(manifest.dependencies.iter().any(|dep| {
        dep.name == "pytest" && dep.context == DependencyContext::OptionalExtra("dev".to_owned())
    }));
}

#[test]
fn extracts_entry_points() {
    let manifest = extract_fixture("pyproject_full");
    assert!(manifest.entry_points.iter().any(|ep| {
        ep.name == "acme-cli" && ep.group == "console" && ep.target == "acme.cli:main"
    }));
    assert!(manifest.entry_points.iter().any(|ep| ep.group == "gui"));
    assert!(
        manifest
            .entry_points
            .iter()
            .any(|ep| ep.group == "acme.plugins")
    );
}

#[test]
fn requirements_recursive_include() {
    let manifest = extract_fixture("requirements_recursive");
    let names = dependency_names(&manifest);
    assert!(names.contains(&"requests"));
    assert!(names.contains(&"urllib3"));
    assert!(
        manifest
            .sources
            .requirements_files
            .iter()
            .any(|f| f.contains("nested.txt"))
    );
}

#[test]
fn requirements_constraints_not_declared() {
    let manifest = extract_fixture("requirements_constraints");
    let names = dependency_names(&manifest);
    assert!(names.contains(&"requests"));
    assert!(!names.contains(&"constraints.txt"));
    assert!(
        manifest
            .constraints
            .iter()
            .any(|dep| dep.name == "requests")
    );
    assert!(
        manifest
            .sources
            .requirements_files
            .iter()
            .any(|path| path.contains("constraints.txt"))
    );
}

#[test]
fn dynamic_dependencies_use_requirements() {
    let manifest = extract_fixture("pyproject_dynamic");
    assert!(
        manifest
            .metadata
            .dynamic
            .contains(&"dependencies".to_owned())
    );
    assert!(manifest.dependencies.iter().any(|dep| dep.name == "flask"));
}

#[test]
fn uv_lock_builds_graph() {
    let manifest = extract_fixture("uv_lock_graph");
    assert!(manifest.sources.uv_lock);
    let requests_deps = manifest
        .lockfile
        .edges
        .get("requests")
        .expect("requests in lockfile");
    assert!(requests_deps.iter().any(|dep| dep == "urllib3"));
}

#[test]
fn setup_py_static_install_requires() {
    let manifest = extract_fixture("setup_py_static");
    assert!(manifest.sources.setup_py);
    assert!(
        manifest
            .dependencies
            .iter()
            .any(|dep| dep.name == "requests")
    );
    assert!(manifest.dependencies.iter().any(|dep| {
        dep.name == "pytest" && dep.context == DependencyContext::SetupExtra("test".to_owned())
    }));
}

#[test]
fn setup_py_dynamic_warns_and_skips() {
    let manifest = extract_fixture("setup_py_dynamic");
    assert!(!manifest.sources.setup_py);
    assert!(
        manifest
            .warnings
            .iter()
            .any(|warning| { matches!(warning, ManifestWarning::SetupPyNotStatic { .. }) })
    );
}

#[test]
fn opaque_url_not_unused_candidate() {
    let manifest = extract_fixture("requirements_editable");
    let editable = manifest
        .dependencies
        .iter()
        .find(|dep| dep.opaque)
        .expect("opaque editable dependency");
    assert!(editable.name.is_empty());
    assert!(
        editable
            .specifier
            .as_deref()
            .is_some_and(|spec| spec.contains("localpkg"))
    );
}

#[test]
fn merge_keeps_duplicate_declarations() {
    let manifest = extract_fixture("duplicate_deps");
    let requests = manifest
        .dependencies
        .iter()
        .filter(|dep| dep.name == "requests")
        .count();
    assert_eq!(requests, 2);
}

#[test]
fn resolve_target_version_from_requires_python() {
    let manifest = extract_fixture("pyproject_full");
    let config = default_config();
    let resolved = resolve_target_version(&config, &manifest);
    assert_eq!(resolved, TargetVersion::parse("py312").expect("py312"));
}

#[test]
fn resolve_target_version_honors_explicit_py311_over_requires_python() {
    let manifest = extract_fixture("pyproject_full");
    let mut config = default_config();
    config.target_version = Some(TargetVersion::parse("py311").expect("py311"));
    let resolved = resolve_target_version(&config, &manifest);
    assert_eq!(resolved, TargetVersion::parse("py311").expect("py311"));
}

#[test]
fn extracts_without_pyproject() {
    let manifest = extract_fixture("requirements_only");
    assert!(manifest.dependencies.iter().any(|dep| dep.name == "django"));
    assert!(!manifest.sources.pyproject_toml);
}

#[test]
fn broken_pyproject_is_error() {
    let path = fixture("broken_pyproject");
    let root = project_root_at(&path);
    let config = LoadedConfig {
        root: root.clone(),
        effective: default_config(),
        sources: ConfigSources {
            used_defaults: true,
            dot_chokkin_toml: None,
            chokkin_toml: None,
            pyproject_tool_chokkin: false,
        },
        uv_workspace: None,
        workspace_members: Vec::new(),
    };
    let error = extract_manifest(&root, &config).expect_err("broken pyproject");
    assert!(matches!(error, ManifestError::InvalidToml { .. }));
}

#[test]
fn poetry_detected_emits_warning() {
    let manifest = extract_fixture("poetry_detected");
    assert!(manifest.sources.skipped_poetry);
    assert!(
        manifest
            .warnings
            .iter()
            .any(|warning| matches!(warning, ManifestWarning::PoetryDetected))
    );
}

#[test]
fn requirements_long_flag_include() {
    let manifest = extract_fixture("requirements_long_flag");
    assert!(manifest.dependencies.iter().any(|dep| dep.name == "flask"));
}

#[test]
fn requirements_egg_name_extracted() {
    let manifest = extract_fixture("requirements_egg");
    assert!(
        manifest
            .dependencies
            .iter()
            .any(|dep| dep.name == "my-package")
    );
}

#[test]
fn requirements_url_without_egg_is_opaque() {
    let manifest = extract_fixture("requirements_opaque_url");
    let dep = manifest
        .dependencies
        .iter()
        .find(|dep| dep.opaque)
        .expect("opaque git dependency");
    assert!(dep.name.is_empty());
    assert!(
        dep.specifier
            .as_deref()
            .is_some_and(|spec| spec.contains("github.com"))
    );
}

#[test]
fn setup_py_partial_comment_emits_warning() {
    let manifest = extract_fixture("setup_py_partial_comment");
    assert!(manifest.sources.setup_py);
    assert!(
        manifest
            .warnings
            .iter()
            .any(|warning| matches!(warning, ManifestWarning::SetupPyPartiallyStatic { .. }))
    );
    assert!(
        manifest
            .dependencies
            .iter()
            .any(|dep| dep.name == "requests")
    );
    assert!(
        manifest
            .dependencies
            .iter()
            .filter(|dep| dep.name == "requests")
            .count()
            == 1
    );
}

#[test]
fn metadata_conflict_emits_warning() {
    let manifest = extract_fixture("metadata_conflict");
    assert_eq!(manifest.metadata.version.as_deref(), Some("1.0.0"));
    assert!(manifest.warnings.iter().any(|warning| matches!(
        warning,
        ManifestWarning::MetadataConflict { field, .. } if field == "version"
    )));
}

#[test]
fn uv_workspace_hint_copied_from_config() {
    let path = fixture("uv_workspace_hint");
    let root = discover_project_root(&path).unwrap_or_else(|_| project_root_at(&path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let members = config
        .uv_workspace
        .as_ref()
        .map(|hint| hint.members.as_slice())
        .expect("uv workspace hint");
    assert_eq!(
        manifest
            .uv_workspace
            .as_ref()
            .map(|hint| hint.members.as_slice()),
        Some(members)
    );
}

#[test]
fn setup_cfg_install_requires() {
    let manifest = extract_fixture("setup_cfg_install_requires");
    assert!(manifest.sources.setup_cfg);
    let names = dependency_names(&manifest);
    assert!(names.contains(&"requests"), "dependencies: {names:?}");
    assert!(names.contains(&"flask"), "dependencies: {names:?}");
}
