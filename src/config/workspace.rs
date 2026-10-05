//! Workspace member discovery from uv and chokkin config.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::discovery::ProjectRoot;

use super::error::ConfigError;
use super::types::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};

/// Resolve workspace member directories below a project root.
pub fn resolve_workspace_members(
    root: &ProjectRoot,
    config: &ChokkinConfig,
    uv_workspace: Option<&UvWorkspaceHint>,
    uv_path_sources: &BTreeMap<String, String>,
) -> Result<Vec<ResolvedWorkspaceMember>, ConfigError> {
    let mut members = BTreeMap::new();

    if let Some(hint) = uv_workspace {
        for member in resolve_uv_members(&root.path, hint)? {
            members.entry(member.path.clone()).or_insert(member);
        }
    }

    for member in path_source_members(&root.path, uv_path_sources) {
        members.entry(member.path.clone()).or_insert(member);
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
            },
        );
    }

    Ok(members.into_values().collect())
}

/// Issue #499: a path source that is its own project inside the root (`lib/`
/// in streamlit) declares the dependencies of the code under it, so it is read
/// like a uv workspace member. Trees outside the root are not scanned.
fn path_source_members(
    root: &Path,
    uv_path_sources: &BTreeMap<String, String>,
) -> Vec<ResolvedWorkspaceMember> {
    let Ok(canonical_root) = root.canonicalize() else {
        return Vec::new();
    };
    uv_path_sources
        .iter()
        .filter_map(|(name, path)| {
            let dir = root.join(path).canonicalize().ok()?;
            let rel = dir.strip_prefix(&canonical_root).ok()?;
            if rel.as_os_str().is_empty() || !declares_project(&dir.join("pyproject.toml")) {
                return None;
            }
            let rel = normalize_relative_path(rel.to_string_lossy().as_ref());
            Some(ResolvedWorkspaceMember {
                id: name.clone(),
                pyproject_toml: Some(format!("{rel}/pyproject.toml")),
                path: rel,
            })
        })
        .collect()
}

fn declares_project(pyproject: &Path) -> bool {
    std::fs::read_to_string(pyproject)
        .ok()
        .and_then(|contents| contents.parse::<toml::Table>().ok())
        .is_some_and(|table| table.contains_key("project"))
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
        });
    }
    Ok(members)
}

fn find_pyprojects(root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".git" | ".venv" | "venv" | "__pycache__" | "target" | "node_modules")
            )
        })
        .build();
    let mut out = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|error| ConfigError::Io {
            path: root.to_path_buf(),
            source: io::Error::other(error),
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            && entry.file_name() == "pyproject.toml"
        {
            out.push(entry.into_path());
        }
    }
    Ok(out)
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
    path.trim_matches('/').trim_matches('\\').replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::default_config;
    use crate::discovery::RootMarker;

    fn root(path: &Path) -> ProjectRoot {
        ProjectRoot {
            path: path.to_path_buf(),
            marker: RootMarker::PyProjectToml,
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
            &BTreeMap::new(),
        )
        .expect("resolve");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id, "api");
        assert_eq!(members[0].path, "services/api");
    }

    #[test]
    fn uv_member_scan_skips_tool_dirs_but_not_other_hidden_dirs() {
        let temp = tempfile::tempdir().expect("tempdir");
        for dir in [".venv/pkg", ".hidden/pkg"] {
            fs::create_dir_all(temp.path().join(dir)).expect("mkdir");
            fs::write(temp.path().join(dir).join("pyproject.toml"), "").expect("write");
        }
        let members = resolve_workspace_members(
            &root(temp.path()),
            &default_config(),
            Some(&UvWorkspaceHint {
                members: vec![".venv/*".to_owned(), ".hidden/*".to_owned()],
            }),
            &BTreeMap::new(),
        )
        .expect("resolve");
        let paths: Vec<_> = members.iter().map(|member| member.path.as_str()).collect();
        assert_eq!(paths, [".hidden/pkg"]);
    }

    #[test]
    fn in_tree_path_source_project_becomes_member() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (dir, pyproject) in [
            ("lib", "[project]\nname = \"acme\"\n"),
            ("tools", "[tool.black]\n"),
        ] {
            fs::create_dir_all(temp.path().join(dir)).expect("mkdir");
            fs::write(temp.path().join(dir).join("pyproject.toml"), pyproject).expect("write");
        }
        let sources: BTreeMap<String, String> = [
            ("acme", "./lib"),
            ("tools", "tools"),
            ("outside", "../elsewhere"),
            ("self", "."),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_owned(), path.to_owned()))
        .collect();
        let members =
            resolve_workspace_members(&root(temp.path()), &default_config(), None, &sources)
                .expect("resolve");
        assert_eq!(
            members,
            [ResolvedWorkspaceMember {
                id: "acme".to_owned(),
                path: "lib".to_owned(),
                pyproject_toml: Some("lib/pyproject.toml".to_owned()),
            }]
        );
    }
}
