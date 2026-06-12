//! `pyproject.toml` manifest extraction (`[project]` and entry points).

use std::path::Path;

use toml::Value;

use super::error::ManifestError;
use super::pep508_util::parse_requirement;
use super::types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, ProjectMetadata,
};
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
                    push_pyproject_dependency(
                        &mut result.dependencies,
                        &mut result.warnings,
                        raw,
                        DependencyContext::Runtime,
                        &rel,
                        &format!("project.dependencies[{index}]"),
                    );
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
                            push_pyproject_dependency(
                                &mut result.dependencies,
                                &mut result.warnings,
                                raw,
                                DependencyContext::OptionalExtra(extra.clone()),
                                &rel,
                                &format!("project.optional-dependencies.{extra}[{index}]"),
                            );
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
                        push_pyproject_dependency(
                            &mut result.dependencies,
                            &mut result.warnings,
                            raw,
                            DependencyContext::Group(group.clone()),
                            &rel,
                            &format!("dependency-groups.{group}[{index}]"),
                        );
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

#[allow(clippy::too_many_arguments)]
fn push_pyproject_dependency(
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
    raw: &str,
    context: DependencyContext,
    file: &str,
    label: &str,
) {
    let origin = DependencyOrigin {
        file: file.to_owned(),
        line: None,
        label: label.to_owned(),
    };
    match parse_requirement(raw, context, origin) {
        Ok(dep) => dependencies.push(dep),
        Err(warning) => warnings.push(warning),
    }
}

fn read_to_string(path: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| {
            path.file_name().map_or_else(
                || path.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}
