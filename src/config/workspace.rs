//! Workspace member discovery from uv and chokkin config.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use crate::discovery::ProjectRoot;

use super::error::ConfigError;
use super::types::{
    ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint, WorkspaceMemberSource,
};

/// Resolve workspace member directories below a project root.
pub fn resolve_workspace_members(
    root: &ProjectRoot,
    config: &ChokkinConfig,
    uv_workspace: Option<&UvWorkspaceHint>,
) -> Result<Vec<ResolvedWorkspaceMember>, ConfigError> {
    let mut members = BTreeMap::new();

    if let Some(hint) = uv_workspace {
        for member in resolve_uv_members(&root.path, hint)? {
            members.entry(member.path.clone()).or_insert(member);
        }
    }

    for (id, override_cfg) in &config.workspaces {
        let path = normalize_relative_path(&override_cfg.path);
        let pyproject = root.path.join(&override_cfg.path).join("pyproject.toml");
        members.insert(
            path.clone(),
            ResolvedWorkspaceMember {
                id: id.clone(),
                path,
                pyproject_toml: pyproject.is_file().then(|| {
                    normalize_relative_path(&format!("{}/pyproject.toml", override_cfg.path))
                }),
                source: WorkspaceMemberSource::Chokkin,
            },
        );
    }

    Ok(members.into_values().collect())
}

fn resolve_uv_members(
    root: &Path,
    hint: &UvWorkspaceHint,
) -> Result<Vec<ResolvedWorkspaceMember>, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in &hint.members {
        let normalized = normalize_relative_path(pattern);
        let glob = Glob::new(&normalized).map_err(|source| ConfigError::Validation {
            path: root.join("pyproject.toml"),
            field: "tool.uv.workspace.members".to_owned(),
            message: source.to_string(),
        })?;
        builder.add(glob);
    }
    let set = builder.build().map_err(|source| ConfigError::Validation {
        path: root.join("pyproject.toml"),
        field: "tool.uv.workspace.members".to_owned(),
        message: source.to_string(),
    })?;

    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    for pyproject in find_pyprojects(root)? {
        let Some(member_dir) = pyproject.parent() else {
            continue;
        };
        if member_dir == root {
            continue;
        }
        let rel = relative_path(root, member_dir)?;
        if !set.is_match(&rel) {
            continue;
        }
        if !seen.insert(rel.clone()) {
            continue;
        }
        let id = member_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(rel.as_str())
            .to_owned();
        members.push(ResolvedWorkspaceMember {
            id,
            path: rel.clone(),
            pyproject_toml: Some(format!("{rel}/pyproject.toml")),
            source: WorkspaceMemberSource::Uv,
        });
    }
    Ok(members)
}

fn find_pyprojects(root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let mut out = Vec::new();
    visit_dirs(root, root, &mut out)?;
    Ok(out)
}

fn visit_dirs(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ConfigError> {
    let entries = fs::read_dir(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_file() && entry.file_name().to_str() == Some("pyproject.toml") {
            out.push(path);
        } else if file_type.is_dir() && should_descend(root, &path) {
            visit_dirs(root, &path, out)?;
        }
    }
    Ok(())
}

fn should_descend(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let Some(name) = rel
        .components()
        .next_back()
        .and_then(|component| component.as_os_str().to_str())
    else {
        return true;
    };
    !matches!(
        name,
        ".git" | ".venv" | "venv" | "__pycache__" | "target" | "node_modules"
    )
}

fn relative_path(root: &Path, path: &Path) -> Result<String, ConfigError> {
    path.strip_prefix(root)
        .map(|rel| normalize_relative_path(rel.to_string_lossy().as_ref()))
        .map_err(|source| ConfigError::Validation {
            path: root.join("pyproject.toml"),
            field: "tool.uv.workspace.members".to_owned(),
            message: source.to_string(),
        })
}

fn normalize_relative_path(path: &str) -> String {
    path.trim_matches('/')
        .trim_matches('\\')
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::discovery::RootMarker;

    fn root(path: &Path) -> ProjectRoot {
        ProjectRoot {
            path: path.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: path.to_path_buf(),
        }
    }

    #[test]
    fn resolves_uv_workspace_members_with_pyproject() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("pyproject.toml"), "[tool.uv.workspace]\n").expect("write");
        fs::create_dir_all(temp.path().join("services/api")).expect("mkdir");
        fs::write(
            temp.path().join("services/api/pyproject.toml"),
            "[project]\nname = \"api\"\n",
        )
        .expect("write");
        let members = resolve_workspace_members(
            &root(temp.path()),
            &default_config(),
            Some(&UvWorkspaceHint {
                members: vec!["services/*".to_owned()],
            }),
        )
        .expect("resolve");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id, "api");
        assert_eq!(members[0].path, "services/api");
        assert_eq!(members[0].source, WorkspaceMemberSource::Uv);
    }
}
