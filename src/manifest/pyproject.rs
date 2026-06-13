//! `pyproject.toml` manifest extraction (`[project]` and entry points).

use std::path::Path;

use toml::Value;

use super::error::ManifestError;
use super::types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, ProjectMetadata,
};
use super::util::{DependencyPush, push_dependency, read_to_string, relative_path};
use super::warnings::ManifestWarning;

/// Partial extraction result from `pyproject.toml`.
#[derive(Debug, Default)]
pub struct PyprojectExtraction {
    /// Project metadata.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Entry points.
    pub entry_points: Vec<EntryPointDecl>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// When true, `[project].dependencies` is dynamic and should come from requirements.
    pub skip_project_dependencies: bool,
}

/// Extract manifest data from `pyproject.toml`.
#[allow(clippy::too_many_lines)]
pub fn extract_pyproject(root: &Path, path: &Path) -> Result<PyprojectExtraction, ManifestError> {
    let contents = read_to_string(path)?;
    let table: toml::Table =
        toml::from_str(&contents).map_err(|source| ManifestError::InvalidToml {
            path: path.to_path_buf(),
            source,
        })?;

    let rel = relative_path(root, path);
    let mut result = PyprojectExtraction::default();

    detect_unsupported_tools(&table, &mut result.warnings);

    if let Some(project) = table.get("project").and_then(Value::as_table) {
        result.metadata = parse_project_metadata(project);
        result.skip_project_dependencies = result
            .metadata
            .dynamic
            .iter()
            .any(|item| item == "dependencies");

        if !result.skip_project_dependencies
            && let Some(deps) = project.get("dependencies").and_then(Value::as_array)
        {
            for (index, dep) in deps.iter().enumerate() {
                if let Some(raw) = dep.as_str() {
                    push_dependency(DependencyPush {
                        dependencies: &mut result.dependencies,
                        warnings: &mut result.warnings,
                        raw,
                        context: DependencyContext::Runtime,
                        file: &rel,
                        label: &format!("project.dependencies[{index}]"),
                        line: None,
                    });
                }
            }
        }

        if let Some(optional) = project
            .get("optional-dependencies")
            .and_then(Value::as_table)
        {
            for (extra, deps_value) in optional {
                if let Some(deps) = deps_value.as_array() {
                    for (index, dep) in deps.iter().enumerate() {
                        if let Some(raw) = dep.as_str() {
                            push_dependency(DependencyPush {
                                dependencies: &mut result.dependencies,
                                warnings: &mut result.warnings,
                                raw,
                                context: DependencyContext::OptionalExtra(extra.clone()),
                                file: &rel,
                                label: &format!("project.optional-dependencies.{extra}[{index}]"),
                                line: None,
                            });
                        }
                    }
                }
            }
        }

        if let Some(scripts) = project.get("scripts").and_then(Value::as_table) {
            for (name, target) in scripts {
                if let Some(target_str) = target.as_str() {
                    result.entry_points.push(EntryPointDecl {
                        name: name.clone(),
                        target: target_str.to_owned(),
                        group: "console".to_owned(),
                        origin: DependencyOrigin {
                            file: rel.clone(),
                            line: None,
                            label: format!("project.scripts.{name}"),
                        },
                    });
                }
            }
        }

        if let Some(scripts) = project.get("gui-scripts").and_then(Value::as_table) {
            for (name, target) in scripts {
                if let Some(target_str) = target.as_str() {
                    result.entry_points.push(EntryPointDecl {
                        name: name.clone(),
                        target: target_str.to_owned(),
                        group: "gui".to_owned(),
                        origin: DependencyOrigin {
                            file: rel.clone(),
                            line: None,
                            label: format!("project.gui-scripts.{name}"),
                        },
                    });
                }
            }
        }

        if let Some(entry_points) = project.get("entry-points").and_then(Value::as_table) {
            for (group, entries) in entry_points {
                if let Some(entries_table) = entries.as_table() {
                    for (name, target) in entries_table {
                        if let Some(target_str) = target.as_str() {
                            result.entry_points.push(EntryPointDecl {
                                name: name.clone(),
                                target: target_str.to_owned(),
                                group: group.clone(),
                                origin: DependencyOrigin {
                                    file: rel.clone(),
                                    line: None,
                                    label: format!("project.entry-points.{group}.{name}"),
                                },
                            });
                        }
                    }
                }
            }
        }
    }

    if let Some(groups) = table.get("dependency-groups").and_then(Value::as_table) {
        for (group, deps_value) in groups {
            if let Some(deps) = deps_value.as_array() {
                for (index, dep) in deps.iter().enumerate() {
                    if let Some(raw) = dep.as_str() {
                        push_dependency(DependencyPush {
                            dependencies: &mut result.dependencies,
                            warnings: &mut result.warnings,
                            raw,
                            context: DependencyContext::Group(group.clone()),
                            file: &rel,
                            label: &format!("dependency-groups.{group}[{index}]"),
                            line: None,
                        });
                    }
                }
            }
        }
    }

    Ok(result)
}

fn parse_project_metadata(project: &toml::Table) -> ProjectMetadata {
    let name = project
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let version = project
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let requires_python = project
        .get("requires-python")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let dynamic = project
        .get("dynamic")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    ProjectMetadata {
        name,
        version,
        requires_python,
        dynamic,
    }
}

fn detect_unsupported_tools(table: &toml::Table, warnings: &mut Vec<ManifestWarning>) {
    let Some(tool) = table.get("tool").and_then(Value::as_table) else {
        return;
    };

    if tool.contains_key("poetry") {
        warnings.push(ManifestWarning::PoetryDetected);
    }
    if tool.contains_key("pdm") {
        warnings.push(ManifestWarning::PdmDetected);
    }
    if tool.contains_key("hatch") {
        warnings.push(ManifestWarning::HatchDetected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(contents: &str) -> Result<PyprojectExtraction, ManifestError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pyproject.toml");
        std::fs::write(&path, contents).expect("write pyproject.toml");
        extract_pyproject(temp.path(), &path)
    }

    #[test]
    fn dynamic_dependencies_skip_project_list() {
        let result = extract(
            "[project]\nname = \"x\"\ndynamic = [\"dependencies\"]\ndependencies = [\"requests\"]\n",
        )
        .expect("valid pyproject");

        assert!(result.skip_project_dependencies);
        assert!(result.dependencies.is_empty());
    }

    #[test]
    fn detects_all_unsupported_tools() {
        let result = extract("[tool.poetry]\n[tool.pdm]\n[tool.hatch]\n").expect("valid pyproject");
        assert_eq!(
            result.warnings,
            vec![
                ManifestWarning::PoetryDetected,
                ManifestWarning::PdmDetected,
                ManifestWarning::HatchDetected,
            ]
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn bare_key() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9-]{0,8}"
        }

        fn dep_name() -> impl Strategy<Value = String> {
            "[A-Za-z0-9]([A-Za-z0-9._-]{0,12}[A-Za-z0-9])?"
        }

        proptest! {
            #[test]
            fn extract_pyproject_never_panics(contents in "\\PC{0,300}") {
                let _ = extract(&contents);
            }

            #[test]
            fn metadata_fields_roundtrip(
                name in proptest::option::of(dep_name()),
                version in proptest::option::of("[0-9]{1,3}\\.[0-9]{1,3}"),
                requires_python in proptest::option::of(">=3\\.[0-9]{1,2}"),
                dynamic in prop::collection::vec(bare_key(), 0..4),
            ) {
                let mut project = toml::Table::new();
                if let Some(name) = &name {
                    project.insert("name".into(), Value::String(name.clone()));
                }
                if let Some(version) = &version {
                    project.insert("version".into(), Value::String(version.clone()));
                }
                if let Some(requires) = &requires_python {
                    project.insert("requires-python".into(), Value::String(requires.clone()));
                }
                project.insert(
                    "dynamic".into(),
                    Value::Array(dynamic.iter().cloned().map(Value::String).collect()),
                );
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");
                prop_assert_eq!(result.metadata.name, name);
                prop_assert_eq!(result.metadata.version, version);
                prop_assert_eq!(result.metadata.requires_python, requires_python);
                prop_assert_eq!(&result.metadata.dynamic, &dynamic);

                let dynamic_deps = result.metadata.dynamic.iter().any(|d| d == "dependencies");
                prop_assert_eq!(result.skip_project_dependencies, dynamic_deps);
            }

            #[test]
            fn optional_dependencies_carry_extra_context(
                extras in prop::collection::btree_map(
                    bare_key(),
                    prop::collection::vec(dep_name(), 0..4),
                    0..4,
                ),
            ) {
                let mut optional = toml::Table::new();
                for (extra, deps) in &extras {
                    optional.insert(
                        extra.clone(),
                        Value::Array(deps.iter().cloned().map(Value::String).collect()),
                    );
                }
                let mut project = toml::Table::new();
                project.insert("name".into(), Value::String("x".into()));
                project.insert("optional-dependencies".into(), Value::Table(optional));
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");

                let expected: usize = extras.values().map(Vec::len).sum();
                prop_assert_eq!(result.dependencies.len(), expected);
                for dep in &result.dependencies {
                    let DependencyContext::OptionalExtra(extra) = &dep.context else {
                        let context = format!("{:?}", dep.context);
                        return Err(TestCaseError::fail(format!(
                            "unexpected context: {context}"
                        )));
                    };
                    prop_assert!(extras.contains_key(extra));
                    let label_prefix = format!("project.optional-dependencies.{extra}[");
                    prop_assert!(dep.origin.label.starts_with(&label_prefix));
                }
            }

            #[test]
            fn entry_points_roundtrip_across_groups(
                scripts in prop::collection::btree_map(bare_key(), "[a-z_.:]{1,20}", 0..4),
                gui_scripts in prop::collection::btree_map(bare_key(), "[a-z_.:]{1,20}", 0..4),
            ) {
                let to_table = |map: &std::collections::BTreeMap<String, String>| {
                    let mut table = toml::Table::new();
                    for (key, value) in map {
                        table.insert(key.clone(), Value::String(value.clone()));
                    }
                    table
                };
                let mut project = toml::Table::new();
                project.insert("name".into(), Value::String("x".into()));
                project.insert("scripts".into(), Value::Table(to_table(&scripts)));
                project.insert("gui-scripts".into(), Value::Table(to_table(&gui_scripts)));
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");

                prop_assert_eq!(
                    result.entry_points.len(),
                    scripts.len() + gui_scripts.len()
                );
                for entry in &result.entry_points {
                    let expected = match entry.group.as_str() {
                        "console" => scripts.get(&entry.name),
                        "gui" => gui_scripts.get(&entry.name),
                        other => {
                            let group = other.to_owned();
                            return Err(TestCaseError::fail(format!(
                                "unexpected group: {group}"
                            )));
                        }
                    };
                    prop_assert_eq!(Some(&entry.target), expected);
                }
            }

            #[test]
            fn unsupported_tool_warnings_match_subset(
                poetry in proptest::bool::ANY,
                pdm in proptest::bool::ANY,
                hatch in proptest::bool::ANY,
            ) {
                let mut contents = String::new();
                if poetry {
                    contents.push_str("[tool.poetry]\n");
                }
                if pdm {
                    contents.push_str("[tool.pdm]\n");
                }
                if hatch {
                    contents.push_str("[tool.hatch]\n");
                }

                let result = extract(&contents).expect("valid pyproject");
                let mut expected = Vec::new();
                if poetry {
                    expected.push(ManifestWarning::PoetryDetected);
                }
                if pdm {
                    expected.push(ManifestWarning::PdmDetected);
                }
                if hatch {
                    expected.push(ManifestWarning::HatchDetected);
                }
                prop_assert_eq!(result.warnings, expected);
            }
        }
    }
}
