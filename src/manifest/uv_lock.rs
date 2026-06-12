//! `uv.lock` graph extraction.

use std::path::Path;

use toml::Value;

use super::error::ManifestError;
use super::pep508_util::normalize_distribution_name;
use super::types::LockfileGraph;

/// Parse `uv.lock` into a dependency name graph.
pub fn extract_uv_lock(path: &Path) -> Result<LockfileGraph, ManifestError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let table: toml::Table =
        toml::from_str(&contents).map_err(|error| ManifestError::InvalidUvLock {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

    let requires_python = table
        .get("requires-python")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut edges = super::types::LockfileGraph::default().edges;

    if let Some(packages) = table.get("package").and_then(Value::as_array) {
        for package in packages {
            let Some(package_table) = package.as_table() else {
                continue;
            };
            let Some(name) = package_table.get("name").and_then(Value::as_str) else {
                continue;
            };
            let normalized = normalize_distribution_name(name);
            let mut deps = Vec::new();
            if let Some(dependencies) = package_table.get("dependencies").and_then(Value::as_array)
            {
                for dep in dependencies {
                    if let Some(dep_table) = dep.as_table() {
                        if let Some(dep_name) = dep_table.get("name").and_then(Value::as_str) {
                            deps.push(normalize_distribution_name(dep_name));
                        }
                    } else if let Some(dep_name) = dep.as_str() {
                        deps.push(normalize_distribution_name(dep_name));
                    }
                }
            }
            edges.insert(normalized, deps);
        }
    }

    Ok(LockfileGraph {
        edges,
        requires_python,
    })
}
