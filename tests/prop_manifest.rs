//! Property-based tests for manifest extraction (public API surface).
//!
//! Strategy: feed both unstructured (fuzz-like) and structured generated
//! manifests through `extract_manifest` and check that it never panics and
//! that parsed output upholds documented invariants.

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
    ConfigSources, LoadedConfig, ProjectRoot, RootMarker, default_config, extract_manifest,
};
use proptest::prelude::*;

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

/// Default-config wrapper so manifest extraction can run without config files.
fn default_loaded_config(root: &ProjectRoot) -> LoadedConfig {
    LoadedConfig {
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
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

/// PEP 508 distribution names with alphanumeric edges.
fn valid_name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9]([A-Za-z0-9._-]{0,20}[A-Za-z0-9])?"
}

/// Reference PEP 503 normalization: runs of `-_.` collapse into one `-`.
fn normalized(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut pending = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            pending = true;
            continue;
        }
        if pending {
            result.push('-');
            pending = false;
        }
        result.push(ch.to_ascii_lowercase());
    }
    if pending {
        result.push('-');
    }
    result
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn requirements_fuzz_never_panics(
        lines in prop::collection::vec("\\PC{0,80}", 0..20),
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("requirements.txt"), &lines.join("\n"));
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        // Arbitrary content may be an include error or parse fine; it must not panic.
        if let Ok(manifest) = extract_manifest(&root, &config) {
            let line_count = u32::try_from(lines.len()).expect("line count fits u32");
            for dep in &manifest.dependencies {
                prop_assert_eq!(normalized(&dep.name), dep.name.clone());
                prop_assert_eq!(dep.origin.file.as_str(), "requirements.txt");
                let line = dep.origin.line.expect("requirements deps carry line numbers");
                prop_assert!((1..=line_count).contains(&line));
            }
        }
    }

    #[test]
    fn requirements_roundtrip_preserves_order_and_names(
        deps in prop::collection::vec((valid_name(), 0u32..100, 0u32..100), 0..10),
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut contents = String::new();
        for (name, major, minor) in &deps {
            writeln!(contents, "{name}>={major}.{minor}").expect("write to string");
        }
        write_file(&temp.path().join("requirements.txt"), &contents);
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let manifest = extract_manifest(&root, &config).expect("valid requirements");
        prop_assert_eq!(manifest.dependencies.len(), deps.len());
        for (dep, (name, _, _)) in manifest.dependencies.iter().zip(&deps) {
            prop_assert_eq!(dep.name.clone(), normalized(name));
            prop_assert!(!dep.opaque);
        }
        prop_assert!(manifest.warnings.is_empty());
    }

    #[test]
    fn requirements_include_chain_terminates(depth in 1usize..8, cyclic in proptest::bool::ANY) {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..depth {
            let next = index + 1;
            let line = if next < depth {
                format!("-r req-{next}.txt\n")
            } else if cyclic {
                "-r requirements.txt\n".to_owned()
            } else {
                "requests\n".to_owned()
            };
            let filename = if index == 0 {
                "requirements.txt".to_owned()
            } else {
                format!("req-{index}.txt")
            };
            write_file(&temp.path().join(filename), &line);
        }
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        // Cyclic include chains must error out; acyclic ones must resolve.
        let result = extract_manifest(&root, &config);
        if cyclic && depth > 1 {
            prop_assert!(result.is_err());
        } else if !cyclic {
            let manifest = result.expect("acyclic chain resolves");
            prop_assert_eq!(manifest.dependencies.len(), 1);
            prop_assert_eq!(manifest.dependencies[0].name.as_str(), "requests");
        }
    }

    #[test]
    fn pyproject_fuzz_never_panics(contents in "\\PC{0,400}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), &contents);
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let _ = extract_manifest(&root, &config);
    }

    #[test]
    fn pyproject_roundtrip_dependencies_and_groups(
        runtime_deps in prop::collection::vec(valid_name(), 0..6),
        dev_deps in prop::collection::vec(valid_name(), 0..6),
        name in valid_name(),
    ) {
        let mut doc = toml::Table::new();
        let mut project = toml::Table::new();
        project.insert("name".into(), toml::Value::String(name.clone()));
        project.insert(
            "dependencies".into(),
            toml::Value::Array(
                runtime_deps.iter().cloned().map(toml::Value::String).collect(),
            ),
        );
        doc.insert("project".into(), toml::Value::Table(project));
        let mut groups = toml::Table::new();
        groups.insert(
            "dev".into(),
            toml::Value::Array(dev_deps.iter().cloned().map(toml::Value::String).collect()),
        );
        doc.insert("dependency-groups".into(), toml::Value::Table(groups));

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join("pyproject.toml"),
            &toml::to_string(&doc).expect("serialize pyproject"),
        );
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let manifest = extract_manifest(&root, &config).expect("valid pyproject");
        prop_assert_eq!(manifest.metadata.name, Some(name));
        prop_assert_eq!(
            manifest.dependencies.len(),
            runtime_deps.len() + dev_deps.len()
        );
        for (dep, raw) in manifest
            .dependencies
            .iter()
            .zip(runtime_deps.iter().chain(&dev_deps))
        {
            prop_assert_eq!(dep.name.clone(), normalized(raw));
        }
    }

    #[test]
    fn setup_cfg_roundtrip_install_requires(
        deps in prop::collection::vec(valid_name(), 0..6),
        name in valid_name(),
    ) {
        let mut contents = format!("[metadata]\nname = {name}\n\n[options]\ninstall_requires =\n");
        for dep in &deps {
            writeln!(contents, "    {dep}").expect("write to string");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("setup.cfg"), &contents);
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let manifest = extract_manifest(&root, &config).expect("valid setup.cfg");
        prop_assert_eq!(manifest.metadata.name, Some(name));
        prop_assert_eq!(manifest.dependencies.len(), deps.len());
        for (dep, raw) in manifest.dependencies.iter().zip(&deps) {
            prop_assert_eq!(dep.name.clone(), normalized(raw));
        }
    }

    #[test]
    fn setup_py_fuzz_never_panics(contents in "\\PC{0,400}") {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("setup.py"), &contents);
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let _ = extract_manifest(&root, &config);
    }

    #[test]
    fn setup_py_roundtrip_install_requires(
        deps in prop::collection::vec(valid_name(), 1..6),
        name in valid_name(),
    ) {
        let rendered = deps
            .iter()
            .map(|dep| format!("\"{dep}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let contents = format!(
            "from setuptools import setup\nsetup(\n    name=\"{name}\",\n    install_requires=[{rendered}],\n)\n"
        );

        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("setup.py"), &contents);
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let manifest = extract_manifest(&root, &config).expect("valid setup.py");
        prop_assert!(manifest.sources.setup_py);
        prop_assert_eq!(manifest.metadata.name, Some(name));
        prop_assert_eq!(manifest.dependencies.len(), deps.len());
        for (dep, raw) in manifest.dependencies.iter().zip(&deps) {
            prop_assert_eq!(dep.name.clone(), normalized(raw));
        }
    }

    #[test]
    fn manifest_extraction_is_deterministic(
        lines in prop::collection::vec("\\PC{0,60}", 0..10),
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("requirements.txt"), &lines.join("\n"));
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);

        let first = extract_manifest(&root, &config);
        let second = extract_manifest(&root, &config);
        match (first, second) {
            (Ok(left), Ok(right)) => prop_assert_eq!(left, right),
            (Err(_), Err(_)) => {},
            _ => prop_assert!(false, "determinism violated: Ok vs Err"),
        }
    }
}
