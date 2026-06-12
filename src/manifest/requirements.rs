//! `requirements*.txt` parsing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::error::ManifestError;
use super::pep508_util::{normalize_distribution_name, parse_requirement};
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Result of parsing one or more requirements files.
#[derive(Debug, Default)]
pub struct RequirementsExtraction {
    /// Parsed dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Root-relative paths that were read.
    pub files_read: Vec<String>,
}

/// Parse a root-level requirements file by conventional name.
pub fn extract_requirements_file(
    root: &Path,
    filename: &str,
    default_context: &DependencyContext,
) -> Result<RequirementsExtraction, ManifestError> {
    let path = root.join(filename);
    if !path.is_file() {
        return Ok(RequirementsExtraction::default());
    }

    let mut visited = HashSet::new();
    let mut result = RequirementsExtraction::default();
    parse_requirements_path(root, &path, default_context, &mut visited, &mut result)?;
    Ok(result)
}

fn parse_requirements_path(
    root: &Path,
    path: &Path,
    default_context: &DependencyContext,
    visited: &mut HashSet<PathBuf>,
    result: &mut RequirementsExtraction,
) -> Result<(), ManifestError> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        let cycle = visited
            .iter()
            .map(|p| relative_path(root, p))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(ManifestError::RequirementsCircularInclude { cycle });
    }

    let rel = relative_path(root, path);
    result.files_read.push(rel.clone());

    let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    for (line_number, line) in contents.lines().enumerate() {
        let line_no = u32::try_from(line_number + 1).unwrap_or(u32::MAX);
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(include) = trimmed
            .strip_prefix("-r")
            .or_else(|| trimmed.strip_prefix("--requirement"))
        {
            let include_path = include.trim();
            let resolved = resolve_requirements_include(root, path, include_path);
            let resolved_path =
                resolved.ok_or_else(|| ManifestError::RequirementsIncludeMissing {
                    path: include_path.to_owned(),
                })?;
            parse_requirements_path(root, &resolved_path, default_context, visited, result)?;
            continue;
        }

        if trimmed.starts_with("-c") || trimmed.starts_with("--constraint") {
            continue;
        }

        if let Some(editable) = trimmed
            .strip_prefix("-e")
            .or_else(|| trimmed.strip_prefix("--editable"))
        {
            let path_spec = editable.trim();
            result.dependencies.push(DeclaredDependency {
                name: normalize_distribution_name(
                    Path::new(path_spec)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(path_spec),
                ),
                extras: Vec::new(),
                marker: None,
                specifier: None,
                context: default_context.clone(),
                origin: DependencyOrigin {
                    file: rel.clone(),
                    line: Some(line_no),
                    label: rel.clone(),
                },
                opaque: true,
            });
            continue;
        }

        let origin = DependencyOrigin {
            file: rel.clone(),
            line: Some(line_no),
            label: rel.clone(),
        };
        match parse_requirement(trimmed, default_context.clone(), origin) {
            Ok(dep) => result.dependencies.push(dep),
            Err(warning) => result.warnings.push(warning),
        }
    }

    visited.remove(&canonical);
    Ok(())
}

fn resolve_requirements_include(root: &Path, base: &Path, include: &str) -> Option<PathBuf> {
    let candidate = if base.parent().is_some() {
        base.parent().map(|parent| parent.join(include))
    } else {
        None
    };

    if let Some(path) = candidate
        && path.is_file()
    {
        return Some(path);
    }

    let root_candidate = root.join(include);
    if root_candidate.is_file() {
        return Some(root_candidate);
    }

    None
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.to_string_lossy().replace('\\', "/"),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}
