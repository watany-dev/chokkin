//! Manifest extraction orchestration.

use std::collections::BTreeSet;

use crate::config::{LoadedConfig, TargetVersion, YokeiConfig};
use crate::discovery::ProjectRoot;

use super::error::ManifestError;
use super::pyproject::extract_pyproject;
use super::requirements::extract_requirements_file;
use super::setup_cfg::extract_setup_cfg;
use super::setup_py::extract_setup_py;
use super::types::{
    DependencyContext, LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata,
};
use super::uv_lock::extract_uv_lock;
use super::warnings::ManifestWarning;

/// Extract declared dependencies, entry points, and lockfile graph.
#[allow(clippy::too_many_lines)]
pub fn extract_manifest(
    root: &ProjectRoot,
    config: &LoadedConfig,
) -> Result<LoadedManifest, ManifestError> {
    let root_path = &root.path;
    let mut metadata = ProjectMetadata::default();
    let mut dependencies = Vec::new();
    let mut constraints = Vec::new();
    let mut entry_points = Vec::new();
    let mut warnings = Vec::new();
    let mut sources = ManifestSources::default();
    let mut lockfile = LockfileGraph::default();
    let pyproject_path = root_path.join("pyproject.toml");
    if pyproject_path.is_file() {
        let extracted = extract_pyproject(root_path, &pyproject_path)?;
        metadata = extracted.metadata;
        dependencies.extend(extracted.dependencies);
        entry_points.extend(extracted.entry_points);
        sources.skipped_poetry = extracted
            .warnings
            .iter()
            .any(|w| matches!(w, ManifestWarning::PoetryDetected));
        warnings.extend(extracted.warnings);
        sources.pyproject_toml = true;
    }

    let dev_group = DependencyContext::Group("dev".to_owned());
    let requirements_specs: &[(&str, &DependencyContext)] = &[
        ("requirements.txt", &DependencyContext::Runtime),
        ("requirements-dev.txt", &dev_group),
        ("dev-requirements.txt", &dev_group),
    ];

    for (filename, context) in requirements_specs {
        let extracted = extract_requirements_file(root_path, filename, context)?;
        if !extracted.files_read.is_empty() {
            sources.requirements_files.extend(extracted.files_read);
        }
        dependencies.extend(extracted.dependencies);
        constraints.extend(extracted.constraints);
        warnings.extend(extracted.warnings);
    }

    let setup_cfg_path = root_path.join("setup.cfg");
    if setup_cfg_path.is_file() {
        let extracted = extract_setup_cfg(root_path, &setup_cfg_path)?;
        let kept_source = if sources.pyproject_toml {
            "pyproject.toml"
        } else {
            "setup.cfg"
        };
        metadata = merge_metadata(
            metadata,
            extracted.metadata,
            &mut warnings,
            kept_source,
            "setup.cfg",
        );
        dependencies.extend(extracted.dependencies);
        warnings.extend(extracted.warnings);
        sources.setup_cfg = true;
    }

    let setup_py_path = root_path.join("setup.py");
    if setup_py_path.is_file() {
        let extracted = extract_setup_py(root_path, &setup_py_path)?;
        let kept_source = if sources.pyproject_toml {
            "pyproject.toml"
        } else if sources.setup_cfg {
            "setup.cfg"
        } else {
            "setup.py"
        };
        metadata = merge_metadata(
            metadata,
            extracted.metadata,
            &mut warnings,
            kept_source,
            "setup.py",
        );
        if extracted.parsed {
            dependencies.extend(extracted.dependencies);
            sources.setup_py = true;
        }
        warnings.extend(extracted.warnings);
    }

    let uv_lock_path = root_path.join("uv.lock");
    if uv_lock_path.is_file() {
        lockfile = extract_uv_lock(&uv_lock_path)?;
        sources.uv_lock = true;
    }

    Ok(LoadedManifest {
        root: root.clone(),
        metadata,
        dependencies,
        constraints,
        uv_workspace: config.uv_workspace.clone(),
        entry_points,
        lockfile,
        sources,
        warnings,
    })
}

/// Prefer explicit `[tool.yokei].target_version`, else infer from `requires-python`.
#[must_use]
pub fn resolve_target_version(config: &YokeiConfig, manifest: &LoadedManifest) -> TargetVersion {
    if config.target_version != TargetVersion::default_py311() {
        return config.target_version.clone();
    }

    if let Some(ref requires_python) = manifest.metadata.requires_python
        && let Some(inferred) = infer_target_version_from_requires_python(requires_python)
    {
        return inferred;
    }

    config.target_version.clone()
}

fn infer_target_version_from_requires_python(specifier: &str) -> Option<TargetVersion> {
    let mut best: Option<(u32, u32)> = None;

    for part in specifier.split(',') {
        let part = part.trim();
        if part.starts_with('<') || part.starts_with("!=") {
            continue;
        }
        let lower_bound = part
            .strip_prefix(">=")
            .or_else(|| part.strip_prefix("~="))
            .or_else(|| part.strip_prefix('>'))
            .unwrap_or(part);
        let digits = lower_bound.trim_start_matches("py").trim();
        let (major, minor) = parse_python_version(digits)?;
        let candidate = (major, minor);
        if best.is_none_or(|current| candidate > current) {
            best = Some(candidate);
        }
    }

    best.and_then(|(major, minor)| {
        TargetVersion::parse(&format!("py{major}{minor:02}"))
            .or_else(|| TargetVersion::parse(&format!("py{major}{minor}")))
    })
}

fn parse_python_version(digits: &str) -> Option<(u32, u32)> {
    if let Some((major, minor)) = digits.split_once('.') {
        let major = major.parse().ok()?;
        let minor = minor
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()?;
        return Some((major, minor));
    }

    let major = digits.chars().next()?.to_digit(10)?;
    let minor = digits.chars().skip(1).collect::<String>().parse().ok()?;
    Some((major, minor))
}

fn merge_metadata(
    mut base: ProjectMetadata,
    overlay: ProjectMetadata,
    warnings: &mut Vec<ManifestWarning>,
    kept_source: &str,
    overlay_source: &str,
) -> ProjectMetadata {
    merge_optional_field(MetadataFieldMerge {
        base: &mut base.name,
        overlay: overlay.name,
        warnings,
        field: "name",
        kept_source,
        overlay_source,
    });
    merge_optional_field(MetadataFieldMerge {
        base: &mut base.version,
        overlay: overlay.version,
        warnings,
        field: "version",
        kept_source,
        overlay_source,
    });
    merge_optional_field(MetadataFieldMerge {
        base: &mut base.requires_python,
        overlay: overlay.requires_python,
        warnings,
        field: "requires-python",
        kept_source,
        overlay_source,
    });

    if !overlay.dynamic.is_empty() {
        let mut seen = BTreeSet::new();
        for item in &base.dynamic {
            seen.insert(item.clone());
        }
        for item in overlay.dynamic {
            if seen.insert(item.clone()) {
                base.dynamic.push(item);
            }
        }
    }

    base
}

struct MetadataFieldMerge<'a> {
    base: &'a mut Option<String>,
    overlay: Option<String>,
    warnings: &'a mut Vec<ManifestWarning>,
    field: &'a str,
    kept_source: &'a str,
    overlay_source: &'a str,
}

fn merge_optional_field(merge: MetadataFieldMerge<'_>) {
    let MetadataFieldMerge {
        base,
        overlay,
        warnings,
        field,
        kept_source,
        overlay_source,
    } = merge;
    let Some(overlay_value) = overlay else {
        return;
    };

    match base {
        Some(existing) if existing != &overlay_value => {
            warnings.push(ManifestWarning::MetadataConflict {
                field: field.to_owned(),
                kept: existing.clone(),
                ignored: overlay_value,
                kept_source: kept_source.to_owned(),
                ignored_source: overlay_source.to_owned(),
            });
        },
        None => {
            *base = Some(overlay_value);
        },
        Some(_) => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_target_version_from_requires_python() {
        let inferred = infer_target_version_from_requires_python(">=3.12").expect("infer");
        assert_eq!(inferred.as_str(), "py312");
    }

    #[test]
    fn infers_highest_minor_from_compound_specifier() {
        let inferred = infer_target_version_from_requires_python(">=3.10,<3.13").expect("infer");
        assert_eq!(inferred.as_str(), "py310");
    }

    #[test]
    fn merge_metadata_keeps_pyproject_requires_python() {
        let base = ProjectMetadata {
            requires_python: Some(">=3.12".to_owned()),
            ..ProjectMetadata::default()
        };
        let overlay = ProjectMetadata {
            requires_python: Some(">=3.10".to_owned()),
            ..ProjectMetadata::default()
        };
        let mut warnings = Vec::new();
        let merged = merge_metadata(base, overlay, &mut warnings, "pyproject.toml", "setup.cfg");
        assert_eq!(merged.requires_python.as_deref(), Some(">=3.12"));
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            ManifestWarning::MetadataConflict { .. }
        ));
    }

    #[test]
    fn merge_metadata_unions_dynamic_fields() {
        let base = ProjectMetadata {
            dynamic: vec!["version".to_owned()],
            ..ProjectMetadata::default()
        };
        let overlay = ProjectMetadata {
            dynamic: vec!["dependencies".to_owned(), "version".to_owned()],
            ..ProjectMetadata::default()
        };
        let mut warnings = Vec::new();
        let merged = merge_metadata(base, overlay, &mut warnings, "pyproject.toml", "setup.cfg");
        assert_eq!(
            merged.dynamic,
            vec!["version".to_owned(), "dependencies".to_owned()]
        );
        assert!(warnings.is_empty());
    }
}
