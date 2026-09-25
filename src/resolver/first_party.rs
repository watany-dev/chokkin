//! First-party and workspace import classification.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};
use crate::manifest::{ProjectMetadata, UvSourceKind, UvToolSettings, normalize_distribution_name};
use crate::sources::{LayoutInfo, infer_layout};

/// Returns `true` when `import_root` matches a first-party package.
#[must_use]
pub fn is_first_party_import(
    import_root: &str,
    layout: &LayoutInfo,
    metadata: &ProjectMetadata,
) -> bool {
    let import_norm = normalize_distribution_name(import_root);
    if layout
        .packages
        .iter()
        .any(|package| normalize_distribution_name(package) == import_norm)
    {
        return true;
    }
    if let Some(name) = &metadata.name {
        return normalize_distribution_name(name) == import_norm;
    }
    false
}

/// Returns `true` when `import_root` matches a resolved workspace member.
#[must_use]
pub fn is_workspace_import(
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

/// Import roots provided by `[tool.uv.sources]` path / editable entries.
///
/// The local tree is read at resolve time (never cached) so it cannot go
/// stale; a tree without packages falls back to the distribution name.
#[must_use]
pub fn path_source_imports(root: &Path, uv: &UvToolSettings) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in &uv.sources {
        let UvSourceKind::Path { path, .. } = &source.kind else {
            continue;
        };
        let metadata = ProjectMetadata {
            name: Some(source.name.clone()),
            ..ProjectMetadata::default()
        };
        let (layout, _) = infer_layout(&root.join(path), &metadata);
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
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
        };
        assert!(is_first_party_import(
            "acme",
            &layout,
            &ProjectMetadata::default()
        ));
    }

    #[test]
    fn metadata_name_matches_normalized_import_root() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
        };
        let metadata = ProjectMetadata {
            name: Some("my-package".to_owned()),
            ..ProjectMetadata::default()
        };
        assert!(is_first_party_import("my_package", &layout, &metadata));
    }

    #[test]
    fn workspace_member_matches_basename() {
        let hint = UvWorkspaceHint {
            members: vec!["packages/billing".to_owned()],
        };
        assert!(is_workspace_import(
            "billing",
            &[],
            Some(&hint),
            &default_config()
        ));
    }

    #[test]
    fn missing_path_source_tree_falls_back_to_distribution_name() {
        let uv = UvToolSettings {
            sources: vec![crate::manifest::UvSource {
                name: "my-lib".to_owned(),
                kind: UvSourceKind::Path {
                    path: "does/not/exist".to_owned(),
                    editable: false,
                },
                origin: crate::manifest::DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: None,
                    label: "tool.uv.sources.my-lib".to_owned(),
                },
            }],
            default_groups: None,
        };
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
}
