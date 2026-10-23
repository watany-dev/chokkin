//! Manifest extraction orchestration.

use std::collections::BTreeSet;
use std::path::Path;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ScanCacheKey, ScanInputFingerprints, stable_hex_hash,
};
use crate::config::{ChokkinConfig, LoadedConfig, TargetVersion};
use crate::discovery::ProjectRoot;
use crate::path_util::{is_file_named, rel_to_root};

use super::error::ManifestError;
use super::lockfile::extract_lockfile;
use super::pep508::{Operator, parse_version_specifiers};
use super::pep508_util::normalize_distribution_name;
use super::pyproject::extract_pyproject;
use super::requirements::{
    extra_requirements_candidates, extract_requirements_file, extract_requirements_path,
    requirements_path_context,
};
use super::setup_cfg::extract_setup_cfg;
use super::setup_py::extract_setup_py;
use super::types::{
    DeclaredDependency, DependencyContext, ExtrasState, LoadedManifest, LockfileGraph,
    LockfileKind, ManifestSources, ProjectMetadata, UvToolSettings,
};
use super::warnings::ManifestWarning;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ManifestCachePayload {
    manifest: LoadedManifest,
    inputs: ScanInputFingerprints,
}

/// Extract declared dependencies, entry points, and lockfile graph.
#[expect(
    clippy::too_many_lines,
    reason = "reads each manifest source in turn and merges them; the order is the logic"
)]
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
    let mut uv = UvToolSettings::default();
    let mut pyproject_dynamic_extras: Option<ExtrasState> = None;
    let pyproject_path = root_path.join("pyproject.toml");
    if is_file_named(root_path, "pyproject.toml") {
        let extracted = extract_pyproject(root_path, &pyproject_path)?;
        metadata = extracted.metadata;
        dependencies.extend(extracted.dependencies);
        entry_points.extend(extracted.entry_points);
        sources.poetry = extracted.poetry_detected;
        constraints.extend(extracted.constraints);
        uv = extracted.uv;
        warnings.extend(extracted.warnings);
        sources.pyproject_toml = true;
        sources.pyproject_project_table = extracted.has_project_table;
        pyproject_dynamic_extras = extracted.dynamic_extras;
    }
    let pyproject_requires_python = metadata.requires_python.clone();
    // Manifest the kept `requires-python` comes from: the first one setting it.
    let mut requires_python_file = pyproject_requires_python
        .is_some()
        .then_some("pyproject.toml");

    // Manifest whose runtime dependencies could not be read, if any.
    let mut runtime_unknown_in: Option<&str> = None;
    // Extras state setup.cfg / setup.py pass, if either passes extras.
    let mut extras: Option<ExtrasState> = None;
    let has_runtime = |dependencies: &[DeclaredDependency]| {
        dependencies
            .iter()
            .any(|dep| dep.context == DependencyContext::Runtime)
    };
    let mut static_runtime_declared = has_runtime(&dependencies);
    // Requirements files setup.* reads as its declarations.
    let mut setup_files_read: Vec<String> = Vec::new();
    let setup_cfg_path = root_path.join("setup.cfg");
    if is_file_named(root_path, "setup.cfg") {
        let extracted = extract_setup_cfg(root_path, &setup_cfg_path)?;
        if extracted.skipped || extracted.runtime_unknown {
            runtime_unknown_in = Some("setup.cfg");
        } else {
            // A partly read `install_requires` may hide the rest.
            static_runtime_declared |= has_runtime(&extracted.dependencies);
        }
        setup_files_read.extend(extracted.files_read.iter().cloned());
        sources.requirements_files.extend(extracted.files_read);
        sources.requirements_missing.extend(extracted.files_missing);
        if requires_python_file.is_none() && extracted.metadata.requires_python.is_some() {
            requires_python_file = Some("setup.cfg");
        }
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
        entry_points.extend(extracted.entry_points);
        warnings.extend(extracted.warnings);
        sources.setup_cfg = true;
        extras = extracted.extras;
    }

    let setup_py_path = root_path.join("setup.py");
    if is_file_named(root_path, "setup.py") {
        let extracted = extract_setup_py(root_path, &setup_py_path)?;
        if extracted.runtime_unknown {
            runtime_unknown_in = Some("setup.py");
        } else if extracted
            .dependencies
            .iter()
            .any(|dep| dep.context == DependencyContext::Runtime)
        {
            // A fully read `install_requires` vouches for a skipped setup.cfg.
            runtime_unknown_in = None;
        }
        if requires_python_file.is_none() && extracted.metadata.requires_python.is_some() {
            requires_python_file = Some("setup.py");
        }
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
        sources.setup_py_dynamic_name = extracted.dynamic_name;
        entry_points.extend(extracted.entry_points);
        if let Some(state) = extracted.extras {
            extras = Some(extras.unwrap_or_default().merge(state));
        }
        if extracted.parsed {
            dependencies.extend(extracted.dependencies);
            sources.setup_py = true;
        }
        sources.extra_requirements.extend(
            extracted
                .table_names
                .iter()
                .map(|name| normalize_distribution_name(name)),
        );
        setup_files_read.extend(extracted.files_read.iter().cloned());
        sources.requirements_files.extend(extracted.files_read);
        sources.requirements_missing.extend(extracted.files_missing);
        warnings.extend(extracted.warnings);
    }
    // setuptools fills dynamic extras from setup.cfg or setup.py, so only
    // extras no source passes are unknown (#905).
    if let Some(dynamic) = pyproject_dynamic_extras {
        let dynamic = match dynamic {
            ExtrasState::Unknown if extras.is_some() => ExtrasState::Known,
            dynamic => dynamic,
        };
        let unread = match &dynamic {
            ExtrasState::Known => None,
            ExtrasState::Partial(names) => Some(names.iter().cloned().collect()),
            ExtrasState::Unknown => Some(Vec::new()),
        };
        warnings.extend(unread.map(|extras| ManifestWarning::DynamicExtrasUnread {
            file: "pyproject.toml".to_owned(),
            extras,
        }));
        extras = Some(extras.unwrap_or_default().merge(dynamic));
    }
    sources.extras = extras.unwrap_or_default();
    // PEP 621: under a `[project]` table setuptools ignores setup.*
    // `python_requires` unless `requires-python` is listed in `dynamic`.
    if sources.pyproject_project_table
        && !metadata
            .dynamic
            .iter()
            .any(|field| field == "requires-python")
    {
        requires_python_file = pyproject_requires_python
            .is_some()
            .then_some("pyproject.toml");
        metadata.requires_python = pyproject_requires_python;
    }
    // An explicit `target_version` replaces the inference, so an unreadable
    // `requires-python` changes nothing then (#917).
    if config.effective.target_version.is_none()
        && let (Some(file), Some(value)) = (requires_python_file, &metadata.requires_python)
        && parse_version_specifiers(value).is_none()
    {
        warnings.push(ManifestWarning::RequiresPythonUnreadable {
            file: file.to_owned(),
            value: value.clone(),
        });
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

    let mut requirements_runtime_declared = false;
    for (filename, context) in requirements_specs {
        // Already read as `install_requires`; reading it again would report
        // every entry as a duplicate.
        if setup_files_read.iter().any(|read| read == filename) {
            continue;
        }
        let extracted = extract_requirements_file(root_path, filename, context)?;
        if extracted.skipped {
            if **context == DependencyContext::Runtime {
                runtime_unknown_in.get_or_insert(filename);
            }
        } else {
            requirements_runtime_declared |= extracted
                .dependencies
                .iter()
                .any(|dep| dep.context == DependencyContext::Runtime);
        }
        if !extracted.files_read.is_empty() {
            sources.requirements_files.extend(extracted.files_read);
        }
        sources.requirements_missing.extend(extracted.files_missing);
        dependencies.extend(extracted.dependencies);
        constraints.extend(extracted.constraints);
        warnings.extend(extracted.warnings);
    }
    read_extra_requirements(root_path, &mut sources, &mut dependencies, &mut warnings);

    // Only declarations from another source vouch for the runtime set; a
    // partially read setup.py may still hide the rest of it.
    let other_runtime_declared = static_runtime_declared || requirements_runtime_declared;
    let dynamic_runtime_unknown = metadata.dynamic.iter().any(|item| item == "dependencies")
        && !dependencies
            .iter()
            .any(|dep| dep.context == DependencyContext::Runtime);
    let unknown_file = match runtime_unknown_in {
        Some(file) if !other_runtime_declared => Some(file),
        _ if dynamic_runtime_unknown => Some("pyproject.toml"),
        _ => None,
    };
    if let Some(file) = unknown_file {
        sources.runtime_dependencies_unknown = true;
        warnings.push(ManifestWarning::RuntimeDependenciesUnknown {
            file: file.to_owned(),
        });
    }

    if let Some((source, graph)) = extract_lockfile(root_path, &mut warnings)? {
        lockfile = graph;
        sources.uv_lock = source.kind == LockfileKind::Uv;
        sources.lockfile = Some(source);
    }

    Ok(LoadedManifest {
        root: root.clone(),
        metadata,
        dependencies,
        constraints,
        uv,
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
            unit_version: "manifest-extract-v23".to_owned(),
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

/// Collect the names that requirements files outside the fixed root names
/// declare, skipping files already read. Entries written in a file whose path
/// names a context (`requirements/tests.in`, `docs/requirements.txt`) are also
/// declarations in that context; the rest only declare names, even when a
/// dev file includes them (mlflow `extra-ml-requirements.txt`, #679, #786).
/// Each such file is its own group (or optional extra), installed on its own
/// like a PEP 735 one, so it never duplicates another file or a
/// `[dependency-groups]` mirror, and a file already read declares once. A `.txt`
/// compiled from a sibling `.in` pins the whole environment, so only the `.in`
/// declares. Best effort: a broken file there only loses its names, never an
/// error, but what it could not read is still a warning (#888).
fn read_extra_requirements(
    root: &Path,
    sources: &mut ManifestSources,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let candidates = extra_requirements_candidates(root);
    let compiled = |file: &str| {
        let path = root.join(file);
        path.extension().is_some_and(|ext| ext == "txt")
            && candidates.contains(&path.with_extension("in"))
    };
    for path in &candidates {
        let rel = rel_to_root(root, path);
        if sources.requirements_files.contains(&rel) {
            continue;
        }
        let (extracted, outcome) =
            extract_requirements_path(root, path, &DependencyContext::Runtime);
        warnings.extend(extracted.warnings);
        if let Err(error) = &outcome {
            warnings.push(ManifestWarning::RequirementsUnreadable {
                file: rel.clone(),
                reason: unreadable_reason(error),
            });
        }
        // A missing include is watched even when nothing was declared, so
        // creating it refreshes the cached run.
        sources.requirements_missing.extend(extracted.files_missing);
        // Unreadable: nothing to declare, and the scan cache could not
        // fingerprint it.
        if (outcome.is_err() || extracted.skipped) && extracted.dependencies.is_empty() {
            continue;
        }
        sources.extra_requirements.extend(
            extracted
                .dependencies
                .iter()
                .filter(|dep| !dep.name.is_empty())
                .map(|dep| normalize_distribution_name(&dep.name)),
        );
        dependencies.extend(extracted.dependencies.into_iter().filter_map(|mut dep| {
            if compiled(&dep.origin.file) || sources.requirements_files.contains(&dep.origin.file) {
                return None;
            }
            let file = dep.origin.file.clone();
            dep.context = match requirements_path_context(&file)? {
                DependencyContext::Group(_) => DependencyContext::Group(file),
                DependencyContext::OptionalExtra(_) => DependencyContext::OptionalExtra(file),
                context => context,
            };
            Some(dep)
        }));
        sources.requirements_files.extend(extracted.files_read);
    }
}

/// Why an extra requirements file stopped parsing, without the absolute path
/// an I/O error names.
pub(super) fn unreadable_reason(error: &ManifestError) -> String {
    match error {
        ManifestError::Io { .. } => "could not be read".to_owned(),
        _ => error.to_string(),
    }
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
pub(crate) fn infer_target_version_from_requires_python(specifier: &str) -> Option<TargetVersion> {
    let specifiers = parse_version_specifiers(specifier)?;
    let (major, minor) = specifiers
        .iter()
        .filter(|spec| {
            matches!(
                spec.operator,
                Operator::GreaterThanEqual
                    | Operator::GreaterThan
                    | Operator::TildeEqual
                    | Operator::Equal
                    | Operator::EqualStar
                    | Operator::ExactEqual
            )
        })
        .map(|spec| {
            let release = &spec.release;
            (
                release.first().copied().unwrap_or(0),
                release.get(1).copied().unwrap_or(0),
            )
        })
        .max()?;
    TargetVersion::parse(&format!("py{major}{minor:02}"))
}

/// Whether `requires-python` parses as a specifier set at all.
pub(crate) fn requires_python_is_readable(specifier: &str) -> bool {
    parse_version_specifiers(specifier).is_some()
}

/// Highest Python 3 minor `requires-python` allows, from `<`/`<=`/`==`/`~=`
/// bounds. `None` when no bound caps the 3.x minor (or the specifier is invalid).
pub(crate) fn requires_python_max_minor(specifier: &str) -> Option<u32> {
    let specifiers = parse_version_specifiers(specifier)?;
    specifiers
        .iter()
        .filter_map(|spec| {
            let release = &spec.release;
            if release.first() != Some(&3) {
                return None;
            }
            let minor = u32::try_from(release.get(1).copied().unwrap_or(0)).ok()?;
            let has_patch = release.iter().skip(2).any(|part| *part > 0);
            match spec.operator {
                Operator::LessThan if has_patch => Some(minor),
                // `<3` / `<3.0` excludes every 3.x, so it caps no 3.x minor.
                Operator::LessThan => minor.checked_sub(1),
                // `==3.*` pins only the major.
                Operator::EqualStar if release.len() < 2 => None,
                Operator::LessThanEqual
                | Operator::Equal
                | Operator::EqualStar
                | Operator::ExactEqual => Some(minor),
                // `~=3.11` only pins the major; `~=3.11.2` pins the minor.
                Operator::TildeEqual if release.len() > 2 => Some(minor),
                _ => None,
            }
        })
        .min()
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

#[expect(
    clippy::too_many_arguments,
    reason = "names both sources so the conflict warning can say which one won"
)]
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

    /// An absolute path in an I/O error would make the warning differ by
    /// machine.
    #[test]
    fn unreadable_reason_leaves_the_path_out_of_io_errors() {
        let error = ManifestError::Io {
            path: std::path::PathBuf::from("/abs/requirements-dev.txt"),
            source: std::io::Error::other("denied"),
        };
        assert_eq!(unreadable_reason(&error), "could not be read");
    }

    /// #917: the warning names the manifest whose `requires-python` was kept.
    #[test]
    fn unreadable_requires_python_names_the_kept_manifest() {
        let unreadable = |files: &[(&str, &str)]| {
            let temp = tempfile::tempdir().expect("tempdir");
            for (file, text) in files {
                std::fs::write(temp.path().join(file), text).expect("write");
            }
            let root = ProjectRoot {
                path: temp.path().to_path_buf(),
                marker: crate::discovery::RootMarker::PyProjectToml,
            };
            let config = crate::config::load_config(&root).expect("config");
            extract_manifest(&root, &config)
                .expect("extract")
                .warnings
                .into_iter()
                .filter_map(|warning| match warning {
                    ManifestWarning::RequiresPythonUnreadable { file, value } => {
                        Some((file, value))
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let setup_cfg = "[metadata]\nname = pkg\n[options]\npython_requires = >=3.9,bogus\n";
        assert_eq!(
            unreadable(&[("setup.cfg", setup_cfg)]),
            [("setup.cfg".to_owned(), ">=3.9,bogus".to_owned())]
        );
        // PEP 621 keeps `[project]`'s value and ignores setup.cfg's.
        let project = "[project]\nname = \"pkg\"\nversion = \"0\"\nrequires-python = \",\"\n";
        assert_eq!(
            unreadable(&[("pyproject.toml", project), ("setup.cfg", setup_cfg)]),
            [("pyproject.toml".to_owned(), ",".to_owned())]
        );
        // The first manifest setting the value names it; one without it does not.
        let setup_py = "from setuptools import setup\nsetup(name='pkg', python_requires='>=3.8')\n";
        assert_eq!(
            unreadable(&[("setup.cfg", setup_cfg), ("setup.py", setup_py)]),
            [("setup.cfg".to_owned(), ">=3.9,bogus".to_owned())]
        );
        let bogus_py =
            "from setuptools import setup\nsetup(name='pkg', python_requires='>=3.8,bogus')\n";
        assert_eq!(
            unreadable(&[
                ("setup.cfg", "[metadata]\nname = pkg\n"),
                ("setup.py", bogus_py)
            ]),
            [("setup.py".to_owned(), ">=3.8,bogus".to_owned())]
        );
        let no_python = "[project]\nname = \"pkg\"\nversion = \"0\"\n";
        let none = unreadable(&[("pyproject.toml", no_python), ("setup.cfg", setup_cfg)]);
        assert!(none.is_empty(), "{none:?}");
        // No lower bound is readable, just not a target version.
        let no_floor = "[project]\nname = \"pkg\"\nversion = \"0\"\nrequires-python = \"<3.13\"\n";
        let none = unreadable(&[("pyproject.toml", no_floor)]);
        assert!(none.is_empty(), "{none:?}");
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
    fn empty_specifier_items_do_not_hide_the_bounds() {
        for specifier in [">=3.9,", ">=3.9,,<4", " , >=3.9"] {
            let inferred = infer_target_version_from_requires_python(specifier);
            assert_eq!(
                inferred.map(|v| v.as_str().to_owned()).as_deref(),
                Some("py309"),
                "{specifier:?}"
            );
        }
        assert_eq!(requires_python_max_minor(">=3.9,,<3.13,"), Some(12));
    }

    #[test]
    fn ignores_upper_bounds_and_rejects_invalid_specifier() {
        assert!(infer_target_version_from_requires_python("<3.13,!=3.9.*").is_none());
        assert!(infer_target_version_from_requires_python("not a specifier").is_none());
    }

    #[test]
    fn reads_highest_allowed_minor_from_requires_python() {
        assert_eq!(requires_python_max_minor(">=3.10,<3.15"), Some(14));
        assert_eq!(requires_python_max_minor(">=3.10,<3.12.1"), Some(12));
        assert_eq!(requires_python_max_minor("<=3.12,<3.14"), Some(12));
        assert_eq!(requires_python_max_minor("==3.11.*"), Some(11));
        assert_eq!(requires_python_max_minor("~=3.11.2"), Some(11));
        assert_eq!(requires_python_max_minor("~=3.11"), None);
        assert_eq!(requires_python_max_minor(">=3.10,<4"), None);
        assert_eq!(requires_python_max_minor(">=3.10"), None);
        assert_eq!(requires_python_max_minor("<3"), None);
        assert_eq!(requires_python_max_minor("<3.0"), None);
        assert_eq!(requires_python_max_minor(">=2.7,<3"), None);
        assert_eq!(requires_python_max_minor("<3.0.1"), Some(0));
        assert_eq!(requires_python_max_minor("==3.*"), None);
        assert_eq!(requires_python_max_minor("not a specifier"), None);
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
        assert_eq!(warnings, []);
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
