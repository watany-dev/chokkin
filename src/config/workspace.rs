//! Workspace member discovery from uv and chokkin config.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::discovery::ProjectRoot;

use super::error::ConfigError;
use super::types::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};

/// Resolve workspace member directories below a project root.
pub fn resolve_workspace_members(
    root: &ProjectRoot,
    config: &ChokkinConfig,
    uv_workspace: Option<&UvWorkspaceHint>,
    uv_path_sources: &BTreeMap<String, Vec<String>>,
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
/// like a uv workspace member. Trees outside the root are not scanned. Of a
/// marker-scoped array (#508) the first qualifying path wins, so member ids
/// stay unique.
fn path_source_members(
    root: &Path,
    uv_path_sources: &BTreeMap<String, Vec<String>>,
) -> Vec<ResolvedWorkspaceMember> {
    let Ok(canonical_root) = root.canonicalize() else {
        return Vec::new();
    };
    uv_path_sources
        .iter()
        .filter_map(|(name, paths)| {
            let rel = paths.iter().find_map(|path| {
                let dir = root.join(path).canonicalize().ok()?;
                let rel = dir.strip_prefix(&canonical_root).ok()?;
                (!rel.as_os_str().is_empty() && declares_project(&dir.join("pyproject.toml")))
                    .then(|| normalize_relative_path(rel.to_string_lossy().as_ref()))
            })?;
            Some(ResolvedWorkspaceMember {
                id: name.clone(),
                pyproject_toml: Some(format!("{rel}/pyproject.toml")),
                path: rel,
            })
        })
        .collect()
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

/// Deepest member directory auto-detection visits; `llama_index` keeps its
/// integrations at `llama-index-integrations/<kind>/<package>`.
const AUTO_MEMBER_MAX_DEPTH: usize = 4;

/// Members implied by nested `pyproject.toml` files that declare a
/// `[project]` name, for monorepos without a workspace declaration (#488).
///
/// Directories are pruned the way source discovery prunes them (`exclude`
/// globs and, when `respect_gitignore`, `.gitignore`), so paths left out of
/// analysis never become members.
pub fn detect_nested_members(
    root: &ProjectRoot,
    exclude: &GlobSet,
    respect_gitignore: bool,
) -> Result<Vec<ResolvedWorkspaceMember>, ConfigError> {
    let scan_root = root.path.clone();
    let excluded = exclude.clone();
    let walker = WalkBuilder::new(&root.path)
        .standard_filters(false)
        .git_ignore(respect_gitignore)
        .require_git(false)
        .max_depth(Some(AUTO_MEMBER_MAX_DEPTH + 1))
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let skipped_name = entry
                .file_name()
                .to_str()
                .is_some_and(is_skipped_member_scan_dir);
            let excluded = relative_path(&scan_root, entry.path())
                .is_ok_and(|rel| excluded.is_match(&rel) || excluded.is_match(format!("{rel}/**")));
            !skipped_name && !excluded
        })
        .build();
    let mut paths = BTreeSet::new();
    for entry in walker {
        let entry = entry.map_err(|error| ConfigError::Io {
            path: root.path.clone(),
            source: io::Error::other(error),
        })?;
        if entry.depth() < 2
            || entry.file_name() != "pyproject.toml"
            || !entry.file_type().is_some_and(|kind| kind.is_file())
        {
            continue;
        }
        let Some(member_dir) = entry.path().parent() else {
            continue;
        };
        if declares_project(entry.path()) {
            paths.insert(relative_path(&root.path, member_dir)?);
        }
    }
    let basename = |rel: &str| rel.rsplit('/').next().unwrap_or(rel).to_owned();
    let mut basename_counts = BTreeMap::new();
    for rel in &paths {
        *basename_counts.entry(basename(rel)).or_insert(0_usize) += 1;
    }
    // A shared basename falls back to the full path for every holder; a
    // unique basename cannot equal another member's path, whose own basename
    // would then be shared.
    Ok(paths
        .into_iter()
        .map(|rel| {
            let id = match basename(&rel) {
                name if basename_counts.get(&name) == Some(&1) => name,
                _ => rel.clone(),
            };
            ResolvedWorkspaceMember {
                id,
                pyproject_toml: Some(format!("{rel}/pyproject.toml")),
                path: rel,
            }
        })
        .collect())
}

/// Hidden and tool directories never hold members; test trees hold fixture
/// projects (chokkin's own `tests/fixtures/*/pyproject.toml`), not packages.
fn is_skipped_member_scan_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "venv"
                | "__pycache__"
                | "target"
                | "node_modules"
                | "site-packages"
                | "build"
                | "dist"
                | "tests"
                | "test"
                | "fixtures"
                | "testdata"
        )
}

/// A `pyproject.toml` that only configures tools (ruff, pytest) is not a
/// distribution; unreadable ones are skipped rather than failing the run.
fn declares_project(pyproject: &Path) -> bool {
    std::fs::read_to_string(pyproject)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| {
            table
                .get("project")
                .and_then(|project| project.get("name"))
                .is_some_and(toml::Value::is_str)
        })
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

    fn write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, text).expect("write");
    }

    fn detect(root_path: &Path, exclude: &[&str]) -> Vec<(String, String)> {
        let mut builder = GlobSetBuilder::new();
        for pattern in exclude {
            builder.add(Glob::new(pattern).expect("glob"));
        }
        detect_nested_members(&root(root_path), &builder.build().expect("globset"), true)
            .expect("detect")
            .into_iter()
            .map(|member| (member.id, member.path))
            .collect()
    }

    #[test]
    fn nested_members_need_a_project_name_within_the_depth_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = "[project]\nname = \"pkg\"\n";
        write(temp.path(), "pyproject.toml", project);
        write(temp.path(), "core/pyproject.toml", project);
        write(
            temp.path(),
            "integrations/llms/openai/pyproject.toml",
            project,
        );
        write(temp.path(), "a/b/c/d/pyproject.toml", project);
        write(temp.path(), "a/b/c/d/e/pyproject.toml", project);
        write(temp.path(), "docs/pyproject.toml", "[tool.ruff]\n");
        write(temp.path(), "broken/pyproject.toml", "[project");
        for skipped in [
            ".venv/pkg",
            "node_modules/pkg",
            "tests/fixtures/pkg",
            "build/pkg",
        ] {
            write(temp.path(), &format!("{skipped}/pyproject.toml"), project);
        }
        assert_eq!(
            detect(temp.path(), &[]),
            [
                ("d".to_owned(), "a/b/c/d".to_owned()),
                ("core".to_owned(), "core".to_owned()),
                ("openai".to_owned(), "integrations/llms/openai".to_owned()),
            ]
        );
    }

    #[test]
    fn nested_members_honor_excludes_gitignore_and_unique_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = "[project]\nname = \"pkg\"\n";
        write(temp.path(), "llms/openai/pyproject.toml", project);
        write(temp.path(), "embeddings/openai/pyproject.toml", project);
        write(temp.path(), "examples/demo/pyproject.toml", project);
        write(temp.path(), "vendor/lib/pyproject.toml", project);
        write(temp.path(), ".gitignore", "vendor/\n");
        write(temp.path(), "openai/pyproject.toml", project);
        assert_eq!(
            detect(temp.path(), &["examples"]),
            [
                (
                    "embeddings/openai".to_owned(),
                    "embeddings/openai".to_owned()
                ),
                ("llms/openai".to_owned(), "llms/openai".to_owned()),
                ("openai".to_owned(), "openai".to_owned()),
            ]
        );
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
        let sources: BTreeMap<String, Vec<String>> = [
            ("acme", vec!["../elsewhere", "./lib", "lib"]),
            ("tools", vec!["tools"]),
            ("outside", vec!["../elsewhere"]),
            ("self", vec!["."]),
        ]
        .into_iter()
        .map(|(name, paths)| {
            (
                name.to_owned(),
                paths.into_iter().map(str::to_owned).collect(),
            )
        })
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
