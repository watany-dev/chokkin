//! Property-based tests for configuration loading (public API surface).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use chokkin::{
    Confidence, PluginId, ProjectMode, ProjectRoot, RootMarker, TargetVersion, load_config,
};
use proptest::prelude::*;

const PLUGIN_KEYS: [&str; 12] = [
    "pytest",
    "django",
    "fastapi",
    "flask",
    "celery",
    "tox",
    "nox",
    "pre_commit",
    "github_actions",
    "sphinx",
    "mkdocs",
    "alembic",
];

const IGNORE_RULES: [&str; 10] = [
    "CHK001", "CHK002", "CHK003", "CHK004", "CHK005", "CHK006", "CHK007", "CHK008", "CHK009",
    "CHK010",
];

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write file");
}

fn mode_value() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("auto"), Just("app"), Just("library")]
}

fn confidence_value() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("certain"), Just("likely"), Just("maybe")]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn load_config_fuzz_never_panics(contents in "\\PC{0,400}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        let _ = load_config(&root);
    }

    #[test]
    fn load_config_pyproject_fuzz_never_panics(contents in "\\PC{0,400}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), &contents);
        let root = project_root_at(temp.path());

        let _ = load_config(&root);
    }

    #[test]
    fn load_config_roundtrips_scalar_fields(
        mode in mode_value(),
        confidence in confidence_value(),
        production in proptest::bool::ANY,
        respect_gitignore in proptest::bool::ANY,
        minor in 0u32..100,
        exclude in prop::collection::vec("[a-z][a-z0-9_/*.]{0,20}", 0..5),
    ) {
        let target_version = format!("py3{minor:02}");
        let mut contents = format!(
            "mode = \"{mode}\"\nconfidence = \"{confidence}\"\nproduction = {production}\n\
             respect_gitignore = {respect_gitignore}\ntarget_version = \"{target_version}\"\n"
        );
        contents.push_str("exclude = [");
        contents.push_str(
            &exclude
                .iter()
                .map(|pattern| format!("\"{pattern}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        contents.push_str("]\n");

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid config");
        prop_assert_eq!(loaded.effective.mode, ProjectMode::parse(mode).expect("known mode"));
        prop_assert_eq!(
            loaded.effective.confidence,
            Confidence::parse(confidence).expect("known confidence")
        );
        prop_assert_eq!(loaded.effective.production, production);
        prop_assert_eq!(loaded.effective.respect_gitignore, respect_gitignore);
        prop_assert_eq!(
            loaded.effective.target_version.as_ref().map(TargetVersion::as_str),
            Some(target_version.as_str())
        );
        prop_assert_eq!(loaded.effective.exclude, exclude);
        prop_assert!(loaded.sources.dot_chokkin_toml.is_some());
    }

    #[test]
    fn load_config_priority_pyproject_overrides_standalone(
        standalone_mode in mode_value(),
        pyproject_mode in mode_value(),
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("mode = \"{standalone_mode}\"\n"),
        );
        write_file(
            &temp.path().join("pyproject.toml"),
            &format!("[tool.chokkin]\nmode = \"{pyproject_mode}\"\n"),
        );
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid config layers");
        prop_assert_eq!(
            loaded.effective.mode,
            ProjectMode::parse(pyproject_mode).expect("known mode")
        );
        prop_assert!(loaded.sources.pyproject_tool_chokkin);
    }

    #[test]
    fn load_config_rejects_unknown_top_level_keys(key in "[a-z][a-z0-9_]{0,15}") {
        let known = [
            "entry", "project", "mode", "production", "target_version",
            "respect_gitignore", "confidence", "exclude", "dependencies",
            "package_module_map", "binary_map", "plugins", "ignore", "workspaces",
        ];
        prop_assume!(!known.contains(&key.as_str()));

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &format!("{key} = true\n"));
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_rejects_absolute_paths_everywhere(
        field in prop_oneof![Just("project"), Just("exclude"), Just("entry")],
        rest in "[a-z][a-z0-9/]{0,15}",
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("{field} = [\"/{rest}\"]\n"),
        );
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_reads_uv_workspace_members(
        members in prop::collection::vec("[a-z][a-z0-9/*-]{0,15}", 1..5),
    ) {
        let rendered = members
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join("pyproject.toml"),
            &format!("[tool.uv.workspace]\nmembers = [{rendered}]\n"),
        );
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid uv workspace hint");
        let hint = loaded.uv_workspace.expect("hint must be present");
        prop_assert_eq!(hint.members, members);
    }

    #[test]
    fn load_config_roundtrips_plugin_flags(
        flags in prop::collection::btree_map(0..PLUGIN_KEYS.len(), proptest::bool::ANY, 0..8),
    ) {
        let mut contents = String::from("[plugins]\n");
        for (&index, enabled) in &flags {
            writeln!(contents, "{} = {enabled}", PLUGIN_KEYS[index]).expect("write to string");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid plugins table");
        for (&index, enabled) in &flags {
            let plugin = PluginId::from_key(PLUGIN_KEYS[index]).expect("known plugin key");
            prop_assert_eq!(loaded.effective.plugins.get(&plugin), Some(enabled));
        }
    }

    #[test]
    fn load_config_rejects_unknown_plugin_keys(key in "[a-z][a-z0-9_]{0,12}") {
        prop_assume!(!PLUGIN_KEYS.contains(&key.as_str()));

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("[plugins]\n{key} = true\n"),
        );
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_roundtrips_ignore_rules(
        rules in prop::collection::btree_map(
            0..IGNORE_RULES.len(),
            prop::collection::vec("[a-z][a-z0-9_/*.]{0,15}", 0..4),
            0..5,
        ),
    ) {
        let mut contents = String::from("[ignore]\n");
        for (&index, patterns) in &rules {
            let rendered = patterns
                .iter()
                .map(|pattern| format!("\"{pattern}\""))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(contents, "{} = [{rendered}]", IGNORE_RULES[index]).expect("write to string");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid ignore table");
        prop_assert_eq!(loaded.effective.ignore.len(), rules.len());
        for (&index, patterns) in &rules {
            prop_assert_eq!(loaded.effective.ignore.get(IGNORE_RULES[index]), Some(patterns));
        }
    }

    #[test]
    fn load_config_rejects_unknown_ignore_rule(code in "[A-Z]{3}[0-9]{3}") {
        prop_assume!(!IGNORE_RULES.contains(&code.as_str()));

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("[ignore]\n{code} = []\n"),
        );
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_roundtrips_workspace_overrides(
        id in "[a-z][a-z0-9-]{0,10}",
        member_path in "[a-z][a-z0-9/]{0,15}",
        mode in proptest::option::of(mode_value()),
    ) {
        let mut contents = format!("[workspaces.{id}]\npath = \"{member_path}\"\n");
        if let Some(mode) = mode {
            writeln!(contents, "mode = \"{mode}\"").expect("write to string");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        let loaded = load_config(&root).expect("valid workspaces table");
        let workspace = loaded.effective.workspaces.get(&id).expect("workspace present");
        prop_assert_eq!(workspace.path.as_str(), member_path.as_str());
        let expected_mode = mode.map(|value| ProjectMode::parse(value).expect("known mode"));
        prop_assert_eq!(workspace.mode, expected_mode);
    }

    #[test]
    fn load_config_rejects_workspace_without_path(id in "[a-z][a-z0-9-]{0,10}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("[workspaces.{id}]\nmode = \"app\"\n"),
        );
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_rejects_absolute_workspace_path(rest in "[a-z][a-z0-9/]{0,12}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join(".chokkin.toml"),
            &format!("[workspaces.member]\npath = \"/{rest}\"\n"),
        );
        let root = project_root_at(temp.path());

        prop_assert!(load_config(&root).is_err());
    }

    #[test]
    fn load_config_is_deterministic(contents in "\\PC{0,200}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join(".chokkin.toml"), &contents);
        let root = project_root_at(temp.path());

        match (load_config(&root), load_config(&root)) {
            (Ok(left), Ok(right)) => prop_assert_eq!(left, right),
            (Err(_), Err(_)) => {},
            _ => prop_assert!(false, "determinism violated: Ok vs Err"),
        }
    }
}
