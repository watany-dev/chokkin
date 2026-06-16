//! First-party and workspace import classification.

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};
use crate::manifest::ProjectMetadata;
use crate::sources::LayoutInfo;

/// Returns `true` when `import_root` matches a first-party package.
#[must_use]
pub fn is_first_party_import(
    import_root: &str,
    layout: &LayoutInfo,
    metadata: &ProjectMetadata,
) -> bool {
    if layout.packages.iter().any(|package| package == import_root) {
        return true;
    }
    if let Some(name) = &metadata.name {
        for candidate in normalized_project_names(name) {
            if candidate == import_root {
                return true;
            }
        }
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

fn member_basename(pattern: &str) -> &str {
    pattern
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(pattern)
}

fn normalized_project_names(name: &str) -> Vec<String> {
    let mut names = vec![name.replace('-', "_"), name.replace('_', "-")];
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ResolvedWorkspaceMember, WorkspaceMemberSource, default_config};
    use crate::manifest::ProjectMetadata;
    use crate::sources::{LayoutInfo, ProjectLayout};

    #[test]
    fn layout_package_is_first_party() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert!(is_first_party_import(
            "acme",
            &layout,
            &ProjectMetadata::default()
        ));
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
    fn resolved_workspace_member_matches_id() {
        let member = ResolvedWorkspaceMember {
            id: "api".to_owned(),
            path: "services/api".to_owned(),
            pyproject_toml: Some("services/api/pyproject.toml".to_owned()),
            source: WorkspaceMemberSource::Uv,
        };
        assert!(is_workspace_import(
            "api",
            &[member],
            None,
            &default_config()
        ));
    }
}
