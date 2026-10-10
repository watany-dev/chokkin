//! First-party and workspace import classification.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};
use crate::manifest::{ProjectMetadata, UvSourceKind, UvToolSettings, normalize_distribution_name};
use crate::sources::{LayoutInfo, infer_layout};

/// Returns `true` when `import_root` matches a first-party package.
#[must_use]
pub(crate) fn is_first_party_import(
    import_root: &str,
    layout: &LayoutInfo,
    metadata: &ProjectMetadata,
) -> bool {
    let import_norm = normalize_distribution_name(import_root);
    // A workspace member's own packages are first-party to every file, the
    // member's own included (`llama_dev` inside `llama-dev/`).
    if layout
        .packages
        .iter()
        .chain(&layout.local_packages)
        .chain(
            layout
                .members
                .iter()
                .flat_map(|member| &member.layout.packages),
        )
        .any(|package| normalize_distribution_name(package) == import_norm)
    {
        return true;
    }
    if metadata
        .version_files
        .iter()
        .any(|path| version_file_root(path, layout) == Some(import_root))
    {
        return true;
    }
    if let Some(name) = &metadata.name {
        return normalize_distribution_name(name) == import_norm;
    }
    false
}

/// Import root of a generated version file, taken below the layout's package
/// root or `src/` (`src/_black_version.py` → `_black_version`).
fn version_file_root<'a>(path: &'a str, layout: &LayoutInfo) -> Option<&'a str> {
    let module = path.strip_suffix(".py")?;
    let module = [layout.package_root.as_str(), "src"]
        .into_iter()
        .find_map(|dir| module.strip_prefix(dir)?.strip_prefix('/'))
        .unwrap_or(module);
    module.split('/').next()
}

/// Returns `true` when `import_root` matches a resolved workspace member.
#[must_use]
pub(super) fn is_workspace_import(
    import_root: &str,
    members: &[ResolvedWorkspaceMember],
    workspace: Option<&UvWorkspaceHint>,
    config: &ChokkinConfig,
) -> bool {
    for member in members {
        if member.id == import_root || member_basename(&member.path) == import_root {
            return true;
        }
    }
    if let Some(hint) = workspace {
        for member in &hint.members {
            if member_basename(member) == import_root {
                return true;
            }
        }
    }
    for override_cfg in config.workspaces.values() {
        if member_basename(&override_cfg.path) == import_root {
            return true;
        }
    }
    false
}

/// Import roots provided by `[tool.uv.sources]` path entries.
///
/// The local tree is read at resolve time (never cached) so it cannot go
/// stale; a tree without packages falls back to the distribution name.
#[must_use]
pub(super) fn path_source_imports(
    root: &Path,
    uv: &UvToolSettings,
) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in &uv.sources {
        let UvSourceKind::Path(path) = &source.kind else {
            continue;
        };
        let metadata = ProjectMetadata {
            name: Some(source.name.clone()),
            ..ProjectMetadata::default()
        };
        let (layout, _) = infer_layout(&root.join(path), &metadata, &UvToolSettings::default());
        let mut packages = layout.packages;
        if packages.is_empty() {
            packages.push(source.name.replace('-', "_"));
        }
        for package in packages {
            let distributions = map.entry(package).or_default();
            if !distributions.contains(&source.name) {
                distributions.push(source.name.clone());
            }
        }
    }
    map
}

fn member_basename(pattern: &str) -> &str {
    pattern
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ResolvedWorkspaceMember, default_config};
    use crate::manifest::ProjectMetadata;
    use crate::sources::{LayoutInfo, ProjectLayout};

    #[test]
    fn layout_package_is_first_party() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            local_packages: vec!["tests".to_owned()],
            ..Default::default()
        };
        for root in ["acme", "tests"] {
            assert!(is_first_party_import(
                root,
                &layout,
                &ProjectMetadata::default()
            ));
        }
    }

    #[test]
    fn metadata_name_matches_normalized_import_root() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            ..Default::default()
        };
        let metadata = ProjectMetadata {
            name: Some("my-package".to_owned()),
            ..ProjectMetadata::default()
        };
        assert!(is_first_party_import("my_package", &layout, &metadata));
    }

    #[test]
    fn generated_version_file_is_first_party_below_the_package_root() {
        let metadata = ProjectMetadata {
            version_files: vec![
                "src/_black_version.py".to_owned(),
                "lib/acme/_version.py".to_owned(),
                "_flat_version.py".to_owned(),
            ],
            ..ProjectMetadata::default()
        };
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "lib".to_owned(),
            ..Default::default()
        };
        for root in ["_black_version", "acme", "_flat_version"] {
            assert!(is_first_party_import(root, &layout, &metadata), "{root}");
        }
        for root in ["src", "lib", "_version"] {
            assert!(!is_first_party_import(root, &layout, &metadata), "{root}");
        }
    }

    #[test]
    fn workspace_member_matches_basename() {
        let hint = UvWorkspaceHint {
            members: vec!["packages/billing".to_owned()],
            exclude: Vec::new(),
        };
        assert!(is_workspace_import(
            "billing",
            &[],
            Some(&hint),
            &default_config()
        ));
    }

    fn my_lib_path_source(path: &str) -> UvToolSettings {
        UvToolSettings {
            sources: vec![crate::manifest::UvSource {
                name: "my-lib".to_owned(),
                kind: UvSourceKind::Path(path.to_owned()),
            }],
        }
    }

    #[test]
    fn missing_path_source_tree_falls_back_to_distribution_name() {
        let uv = my_lib_path_source("does/not/exist");
        let map = path_source_imports(&std::env::temp_dir().join("chokkin-no-such-root"), &uv);
        assert_eq!(map.get("my_lib"), Some(&vec!["my-lib".to_owned()]));
    }

    #[test]
    fn resolved_workspace_member_matches_id() {
        let member = ResolvedWorkspaceMember {
            id: "api".to_owned(),
            path: "services/api".to_owned(),
            pyproject_toml: Some("services/api/pyproject.toml".to_owned()),
        };
        assert!(is_workspace_import(
            "api",
            &[member],
            None,
            &default_config()
        ));
    }

    #[test]
    fn resolved_workspace_member_matches_basename_when_id_differs() {
        let member = ResolvedWorkspaceMember {
            id: "api-service".to_owned(),
            path: "services/api".to_owned(),
            pyproject_toml: None,
        };
        assert!(is_workspace_import(
            "api",
            &[member],
            None,
            &default_config()
        ));
    }

    #[test]
    fn workspace_override_matches_only_its_basename() {
        let mut config = default_config();
        config.workspaces.insert(
            "billing".to_owned(),
            crate::config::WorkspaceOverride {
                path: "libs/billing".to_owned(),
                entry: None,
                project: None,
                mode: None,
            },
        );
        assert!(is_workspace_import("billing", &[], None, &config));
        assert!(!is_workspace_import("shipping", &[], None, &config));
    }

    #[test]
    fn path_source_picks_the_flat_package_named_after_the_distribution() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for package in ["aaa_helpers", "my_lib"] {
            let package_dir = dir.path().join("vendor").join(package);
            std::fs::create_dir_all(&package_dir).expect("mkdir");
            std::fs::write(package_dir.join("__init__.py"), "").expect("write");
        }
        let uv = my_lib_path_source("vendor");
        let map = path_source_imports(dir.path(), &uv);
        assert_eq!(
            map,
            BTreeMap::from([("my_lib".to_owned(), vec!["my-lib".to_owned()])])
        );
    }
}
