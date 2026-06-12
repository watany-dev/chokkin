//! Integration tests for configuration loading.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use yokei::{
    Confidence, ConfigError, PluginId, ProjectMode, ProjectRoot, RootMarker, RuntimeOverrides,
    apply_overrides, default_config, discover_project_root, load_config,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config")
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

fn load_fixture(name: &str) -> yokei::LoadedConfig {
    let path = fixture(name);
    let root = discover_project_root(&path).expect("discover root");
    load_config(&root).expect("load config")
}

#[test]
fn defaults_when_no_config_files() {
    let path = fixture("no_config");
    let root = discover_project_root(&path).expect("discover root");
    let loaded = load_config(&root).expect("load config");

    assert_eq!(loaded.effective, default_config());
    assert!(loaded.sources.used_defaults);
    assert!(!loaded.sources.pyproject_tool_yokei);
    assert!(loaded.uv_workspace.is_none());
}

#[test]
fn defaults_when_pyproject_has_no_tool_yokei() {
    let loaded = load_fixture("defaults_only");
    assert_eq!(loaded.effective, default_config());
    assert!(!loaded.sources.pyproject_tool_yokei);
}

#[test]
fn loads_pyproject_tool_yokei() {
    let loaded = load_fixture("pyproject_full");
    let config = &loaded.effective;

    assert!(loaded.sources.pyproject_tool_yokei);
    assert_eq!(config.mode, ProjectMode::Library);
    assert!(config.production);
    assert_eq!(config.target_version.as_str(), "py312");
    assert!(!config.respect_gitignore);
    assert_eq!(config.confidence, Confidence::Certain);
    assert_eq!(config.exclude, vec!["custom/**".to_owned()]);
    assert_eq!(config.entry.len(), 2);
    assert_eq!(config.entry[0].path, "manage.py");
    assert!(config.entry[0].symbol.is_none());
    assert_eq!(config.entry[1].path, "src/acme/asgi.py");
    assert_eq!(config.entry[1].symbol.as_deref(), Some("application"));
    assert_eq!(
        config.project,
        vec!["src/**/*.py".to_owned(), "tests/**/*.py".to_owned()]
    );
    assert_eq!(config.dependencies.dev_groups, vec!["dev".to_owned()]);
    assert_eq!(
        config.dependencies.runtime_groups,
        vec!["server".to_owned()]
    );
    assert_eq!(config.dependencies.type_groups, vec!["types".to_owned()]);
    assert_eq!(
        config.package_module_map.get("PyYAML"),
        Some(&vec!["yaml".to_owned()])
    );
    assert_eq!(config.binary_map.get("pytest"), Some(&"pytest".to_owned()));
    assert_eq!(config.plugins.get(&PluginId::Pytest), Some(&false));
    assert_eq!(config.plugins.get(&PluginId::Celery), Some(&true));
    assert_eq!(config.plugins.get(&PluginId::Django), Some(&true));
    assert_eq!(config.ignore.get("YOK002"), Some(&vec!["boto3".to_owned()]));
    let api = config.workspaces.get("api").expect("api workspace");
    assert_eq!(api.path, "services/api");
    assert_eq!(
        api.project.as_deref(),
        Some(["src/**/*.py".to_owned()].as_slice())
    );
}

#[test]
fn merge_priority_pyproject_wins() {
    let loaded = load_fixture("merge_priority");
    let config = &loaded.effective;

    assert_eq!(config.mode, ProjectMode::Library);
    assert_ne!(config.mode, ProjectMode::App);
    assert!(config.production);
    assert_eq!(config.confidence, Confidence::Maybe);
    assert!(loaded.sources.dot_yokei_toml.is_some());
    assert!(loaded.sources.yokei_toml.is_some());
    assert!(loaded.sources.pyproject_tool_yokei);
}

#[test]
fn loads_standalone_yokei_toml() {
    let loaded = load_fixture("yokei_toml_only");
    assert_eq!(loaded.effective.mode, ProjectMode::App);
    assert!(loaded.effective.production);
}

#[test]
fn loads_dot_yokei_toml() {
    let loaded = load_fixture("dot_yokei_only");
    assert_eq!(loaded.effective.mode, ProjectMode::App);
    assert_eq!(loaded.effective.confidence, Confidence::Maybe);
}

#[test]
fn parses_workspace_overrides() {
    let loaded = load_fixture("workspace_overrides");
    let worker = loaded
        .effective
        .workspaces
        .get("worker")
        .expect("worker workspace");
    assert_eq!(worker.path, "services/worker");
    assert_eq!(worker.mode, Some(ProjectMode::App));
    assert_eq!(
        worker.entry.as_ref().expect("entry")[0].path,
        "src/worker/__main__.py"
    );
}

#[test]
fn reads_uv_workspace_hint() {
    let loaded = load_fixture("uv_workspace_hint");
    let hint = loaded.uv_workspace.expect("uv workspace hint");
    assert_eq!(hint.members, vec!["services/*", "packages/core"]);
}

#[test]
fn rejects_invalid_mode() {
    let path = fixture("invalid_mode");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("invalid mode");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_unknown_plugin_key() {
    let path = fixture("unknown_plugin");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("unknown plugin");
    assert!(matches!(error, ConfigError::UnknownKey { .. }));
}

#[test]
fn rejects_non_boolean_plugin_value() {
    let path = fixture("invalid_plugin_type");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("non-boolean plugin");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_invalid_ignore_rule() {
    let path = fixture("invalid_ignore");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("invalid ignore");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_absolute_entry_path() {
    let path = fixture("invalid_entry");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("absolute entry");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn invalid_toml_returns_error() {
    let path = fixture("broken_toml");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("broken toml");
    assert!(matches!(error, ConfigError::InvalidToml { .. }));
}

#[test]
fn apply_overrides_production() {
    let mut config = default_config();
    apply_overrides(
        &mut config,
        &RuntimeOverrides {
            production: Some(true),
            ..RuntimeOverrides::default()
        },
    );
    assert!(config.production);
}

#[test]
fn io_error_propagates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pyproject = temp.path().join("pyproject.toml");
    // Invalid UTF-8 makes read_to_string fail on every platform, even as root
    // (unlike chmod 000, which CAP_DAC_OVERRIDE bypasses).
    std::fs::write(&pyproject, b"[tool.yokei]\nmode = \xff\xfe\n").expect("write pyproject");

    let root = project_root_at(temp.path());
    let error = load_config(&root).expect_err("unreadable pyproject");
    assert!(matches!(error, ConfigError::Io { .. }));
}

#[test]
fn rejects_unknown_top_level_key() {
    let path = fixture("unknown_top_level_key");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("unknown top-level key");
    assert!(matches!(error, ConfigError::UnknownKey { .. }));
}

#[test]
fn rejects_invalid_target_version() {
    let path = fixture("invalid_target_version");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("invalid target_version");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_invalid_confidence() {
    let path = fixture("invalid_confidence");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("invalid confidence");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn partial_dependencies_keep_defaults_for_missing_keys() {
    let loaded = load_fixture("incomplete_dependencies");
    let defaults = default_config();
    assert_eq!(loaded.effective.dependencies.dev_groups, vec!["dev"]);
    assert_eq!(
        loaded.effective.dependencies.runtime_groups,
        defaults.dependencies.runtime_groups
    );
    assert_eq!(
        loaded.effective.dependencies.type_groups,
        defaults.dependencies.type_groups
    );
}

#[test]
fn partial_plugins_overlay_defaults() {
    let loaded = load_fixture("partial_plugins");
    let plugins = &loaded.effective.plugins;
    assert_eq!(plugins.get(&PluginId::Celery), Some(&true));
    // Defaults survive a partial [tool.yokei.plugins] table.
    assert_eq!(plugins.get(&PluginId::Pytest), Some(&true));
    assert_eq!(plugins.get(&PluginId::Django), Some(&true));
    assert_eq!(plugins.get(&PluginId::Tox), Some(&false));
}

#[test]
fn rejects_unknown_dependencies_key() {
    let path = fixture("unknown_dependencies_key");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("unknown dependencies key");
    assert!(matches!(error, ConfigError::UnknownKey { .. }));
}

#[test]
fn rejects_unknown_workspace_key() {
    let path = fixture("unknown_workspace_key");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("unknown workspace key");
    assert!(matches!(error, ConfigError::UnknownKey { .. }));
}

#[test]
fn reads_uv_workspace_string_member() {
    let loaded = load_fixture("uv_workspace_string_member");
    let hint = loaded.uv_workspace.expect("uv workspace hint");
    assert_eq!(hint.members, vec!["services/api"]);
}

#[test]
fn rejects_invalid_uv_workspace_members_type() {
    let path = fixture("invalid_uv_workspace_members");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("invalid uv workspace members");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_empty_workspace_path() {
    let path = fixture("empty_workspace_path");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("empty workspace path");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_entry_with_empty_symbol() {
    let path = fixture("entry_empty_symbol");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("empty entry symbol");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_backslash_absolute_entry_path() {
    let path = fixture("backslash_entry");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("backslash absolute entry");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_absolute_project_path() {
    let path = fixture("invalid_project_path");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("absolute project path");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn rejects_absolute_exclude_path() {
    let path = fixture("invalid_exclude_path");
    let root = project_root_at(&path);
    let error = load_config(&root).expect_err("absolute exclude path");
    assert!(matches!(error, ConfigError::Validation { .. }));
}

#[test]
fn empty_yokei_toml_uses_defaults() {
    let loaded = load_fixture("empty_yokei_toml");
    assert_eq!(loaded.effective, default_config());
    assert!(loaded.sources.yokei_toml.is_some());
    assert!(!loaded.sources.pyproject_tool_yokei);
}
