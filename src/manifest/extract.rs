//! Manifest extraction orchestration.

use std::collections::BTreeSet;

use pep508_rs::pep440_rs::{Operator, VersionSpecifiers};

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ScanCacheKey, ScanInputFingerprints, stable_hex_hash,
};
use crate::config::{ChokkinConfig, LoadedConfig, TargetVersion};
use crate::discovery::ProjectRoot;

use super::error::ManifestError;
use super::lockfile::extract_lockfile;
use super::pyproject::extract_pyproject;
use super::requirements::extract_requirements_file;
use super::setup_cfg::extract_setup_cfg;
use super::setup_py::extract_setup_py;
use super::types::{
    DeclaredDependency, DependencyContext, LoadedManifest, LockfileGraph, LockfileKind,
    ManifestSources, ProjectMetadata,
};
use super::warnings::ManifestWarning;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ManifestCachePayload {
    manifest: LoadedManifest,
    inputs: ScanInputFingerprints,
}

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
        sources.poetry = extracted.poetry_detected;
        warnings.extend(extracted.warnings);
        sources.pyproject_toml = true;
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

    let dev_group = DependencyContext::Group("dev".to_owned());
    let docs_group = DependencyContext::Group("docs".to_owned());
    let tests_group = DependencyContext::Group("tests".to_owned());
    let requirements_txt_context = requirements_txt_context(&sources, &dependencies);
    let requirements_specs: &[(&str, &DependencyContext)] = &[
        ("requirements.txt", &requirements_txt_context),
        ("requirements-dev.txt", &dev_group),
        ("dev-requirements.txt", &dev_group),
        ("requirements-docs.txt", &docs_group),
        ("requirements-tests.txt", &tests_group),
        ("requirements-test.txt", &tests_group),
    ];

    for (filename, context) in requirements_specs {
        let extracted = extract_requirements_file(root_path, filename, context)?;
        if !extracted.files_read.is_empty() {
            sources.requirements_files.extend(extracted.files_read);
        }
        sources.requirements_missing.extend(extracted.files_missing);
        dependencies.extend(extracted.dependencies);
        constraints.extend(extracted.constraints);
        warnings.extend(extracted.warnings);
    }

    if let Some((source, graph)) = extract_lockfile(root_path)? {
        lockfile = graph;
        sources.uv_lock = source.kind == LockfileKind::Uv;
        sources.lockfile = Some(source);
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

/// Extract a manifest, optionally using the Phase 2 scan cache payload slot.
///
/// Cache misses and incompatible payloads fall back to static extraction.
pub fn extract_manifest_with_cache(
    root: &ProjectRoot,
    config: &LoadedConfig,
    cache: Option<&CacheOptions>,
) -> Result<LoadedManifest, ManifestError> {
    let Some(cache) = cache.filter(|cache| cache.enabled) else {
        return extract_manifest(root, config);
    };
    let key = manifest_cache_key(root, config)?;
    if let Some(payload) = cache
        .read_scan_payload::<ManifestCachePayload>(root.path.as_path(), &key)
        .map_err(|source| ManifestError::Io {
            path: cache.scan_entry_path(root.path.as_path(), &key),
            source,
        })?
    {
        let current_inputs = manifest_inputs_for_payload(root, config, &payload.manifest)?;
        if current_inputs == payload.inputs {
            // Inputs are fingerprinted root-relative, so a moved or copied
            // project hits an entry that still carries the old absolute root.
            let mut manifest = payload.manifest;
            manifest.root = root.clone();
            return Ok(manifest);
        }
    }

    let manifest = extract_manifest(root, config)?;
    let payload = ManifestCachePayload {
        inputs: manifest_inputs_for_payload(root, config, &manifest)?,
        manifest: manifest.clone(),
    };
    cache
        .write_scan_payload(root.path.as_path(), key, &payload)
        .map_err(|source| ManifestError::Io {
            path: cache.directory_path(root.path.as_path()),
            source,
        })?;
    Ok(manifest)
}

fn manifest_cache_key(
    root: &ProjectRoot,
    config: &LoadedConfig,
) -> Result<ScanCacheKey, ManifestError> {
    let inputs =
        ScanInputFingerprints::collect_manifest_candidates(root.path.as_path(), &config.sources)
            .map_err(|source| ManifestError::Io {
                path: root.path.clone(),
                source,
            })?;
    let target = config
        .effective
        .target_version
        .clone()
        .unwrap_or_else(TargetVersion::default_py311);
    Ok(ScanCacheKey {
        context: CacheKeyContext {
            chokkin_version: VERSION.to_owned(),
            config_hash: stable_hex_hash(format!("{:?}", config.effective).as_bytes()),
            manifest_hash: stable_hex_hash(format!("{:?}", config.uv_workspace).as_bytes()),
            target_version: target.as_str().to_owned(),
            unit_version: "manifest-extract-v3".to_owned(),
        },
        inputs,
    })
}

fn manifest_inputs_for_payload(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
) -> Result<ScanInputFingerprints, ManifestError> {
    ScanInputFingerprints::collect(root.path.as_path(), &config.sources, &manifest.sources).map_err(
        |source| ManifestError::Io {
            path: root.path.clone(),
            source,
        },
    )
}

/// When runtime deps are declared in pyproject/setup, root `requirements.txt` is dev tooling.
fn requirements_txt_context(
    sources: &ManifestSources,
    dependencies: &[DeclaredDependency],
) -> DependencyContext {
    let has_runtime_declaration = dependencies.iter().any(|dep| {
        matches!(
            dep.context,
            DependencyContext::Runtime | DependencyContext::SetupExtra(_)
        )
    });
    if (sources.pyproject_toml || sources.setup_cfg || sources.setup_py) && has_runtime_declaration
    {
        return DependencyContext::Group("dev".to_owned());
    }
    DependencyContext::Runtime
}

/// Prefer explicit `[tool.chokkin].target_version`, else infer from `requires-python`.
#[must_use]
pub fn resolve_target_version(config: &ChokkinConfig, manifest: &LoadedManifest) -> TargetVersion {
    if let Some(explicit) = &config.target_version {
        return explicit.clone();
    }

    if let Some(ref requires_python) = manifest.metadata.requires_python
        && let Some(inferred) = infer_target_version_from_requires_python(requires_python)
    {
        return inferred;
    }

    TargetVersion::default_py311()
}

/// Effective lower bound of `requires-python`: the highest `>=`/`>`/`~=`/`==` release.
fn infer_target_version_from_requires_python(specifier: &str) -> Option<TargetVersion> {
    let specifiers: VersionSpecifiers = specifier.parse().ok()?;
    let (major, minor) = specifiers
        .iter()
        .filter(|spec| {
            matches!(
                spec.operator(),
                Operator::GreaterThanEqual
                    | Operator::GreaterThan
                    | Operator::TildeEqual
                    | Operator::Equal
                    | Operator::EqualStar
                    | Operator::ExactEqual
            )
        })
        .map(|spec| {
            let release = spec.version().release();
            (
                release.first().copied().unwrap_or(0),
                release.get(1).copied().unwrap_or(0),
            )
        })
        .max()?;
    TargetVersion::parse(&format!("py{major}{minor:02}"))
}

fn merge_metadata(
    mut base: ProjectMetadata,
    overlay: ProjectMetadata,
    warnings: &mut Vec<ManifestWarning>,
    kept_source: &str,
    overlay_source: &str,
) -> ProjectMetadata {
    for (field, base, overlay_value) in [
        ("name", &mut base.name, overlay.name),
        ("version", &mut base.version, overlay.version),
        (
            "requires-python",
            &mut base.requires_python,
            overlay.requires_python,
        ),
    ] {
        merge_optional_field(
            base,
            overlay_value,
            warnings,
            field,
            kept_source,
            overlay_source,
        );
    }

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

#[allow(clippy::too_many_arguments)]
fn merge_optional_field(
    base: &mut Option<String>,
    overlay: Option<String>,
    warnings: &mut Vec<ManifestWarning>,
    field: &str,
    kept_source: &str,
    overlay_source: &str,
) {
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
    fn infers_target_version_from_tilde_and_exact_specifiers() {
        let tilde = infer_target_version_from_requires_python("~=3.9").expect("infer");
        assert_eq!(tilde.as_str(), "py309");
        let exact = infer_target_version_from_requires_python("==3.11.*").expect("infer");
        assert_eq!(exact.as_str(), "py311");
    }

    #[test]
    fn ignores_upper_bounds_and_rejects_invalid_specifier() {
        assert!(infer_target_version_from_requires_python("<3.13,!=3.9.*").is_none());
        assert!(infer_target_version_from_requires_python("not a specifier").is_none());
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn infer_target_version_never_panics(specifier in "\\PC{0,80}") {
                if let Some(version) = infer_target_version_from_requires_python(&specifier) {
                    prop_assert!(TargetVersion::parse(version.as_str()).is_some());
                }
            }

            #[test]
            fn infer_target_version_reads_lower_bound(minor in 10u32..40) {
                let inferred = infer_target_version_from_requires_python(&format!(">=3.{minor}"))
                    .expect("lower bound must infer");
                let expected = format!("py3{minor:02}");
                prop_assert_eq!(inferred.as_str(), expected.as_str());
            }

            #[test]
            fn infer_target_version_picks_highest_lower_bound(
                low in 10u32..20,
                high in 20u32..40,
            ) {
                let inferred =
                    infer_target_version_from_requires_python(&format!(">=3.{low},>=3.{high}"))
                        .expect("compound bound must infer");
                let expected = format!("py3{high:02}");
                prop_assert_eq!(inferred.as_str(), expected.as_str());
            }

            #[test]
            fn merge_metadata_keeps_base_and_warns_on_conflict(
                base_name in "[a-z][a-z0-9-]{0,15}",
                overlay_name in "[a-z][a-z0-9-]{0,15}",
            ) {
                let base = ProjectMetadata {
                    name: Some(base_name.clone()),
                    ..ProjectMetadata::default()
                };
                let overlay = ProjectMetadata {
                    name: Some(overlay_name.clone()),
                    ..ProjectMetadata::default()
                };
                let mut warnings = Vec::new();
                let merged =
                    merge_metadata(base, overlay, &mut warnings, "pyproject.toml", "setup.cfg");
                prop_assert_eq!(merged.name.as_deref(), Some(base_name.as_str()));
                prop_assert_eq!(warnings.len(), usize::from(base_name != overlay_name));
            }

            #[test]
            fn merge_metadata_dynamic_union_is_deduplicated(
                base_dynamic in prop::collection::vec("[a-z]{1,8}", 0..4),
                overlay_dynamic in prop::collection::vec("[a-z]{1,8}", 0..4),
            ) {
                let base = ProjectMetadata {
                    dynamic: dedup(base_dynamic),
                    ..ProjectMetadata::default()
                };
                let overlay = ProjectMetadata {
                    dynamic: overlay_dynamic,
                    ..ProjectMetadata::default()
                };
                let expected_base = base.dynamic.clone();
                let mut warnings = Vec::new();
                let merged =
                    merge_metadata(base, overlay, &mut warnings, "pyproject.toml", "setup.cfg");

                let mut seen = BTreeSet::new();
                prop_assert!(merged.dynamic.iter().all(|item| seen.insert(item.clone())));
                prop_assert!(merged.dynamic.starts_with(&expected_base));
            }
        }

        fn dedup(items: Vec<String>) -> Vec<String> {
            let mut seen = BTreeSet::new();
            items
                .into_iter()
                .filter(|item| seen.insert(item.clone()))
                .collect()
        }
    }
}
