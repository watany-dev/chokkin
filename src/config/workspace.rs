//! Workspace member discovery from uv and chokkin config.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::discovery::ProjectRoot;

use super::error::ConfigError;
use super::types::{ChokkinConfig, ResolvedWorkspaceMember, UvWorkspaceHint};

/// Resolve workspace member directories below a project root.
pub(super) fn resolve_workspace_members(
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
        // uv expands members one path component at a time: `packages/*`
        // never reaches `packages/foo/tests/fixture/`.
        let glob = GlobBuilder::new(&normalized)
            .literal_separator(true)
            .build()
            .map_err(|source| ConfigError::Validation {
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

    let mut paths = BTreeSet::new();
    for pyproject in find_pyprojects(root)? {
        let Some(member_dir) = pyproject.parent() else {
            continue;
        };
        if member_dir == root {
            continue;
        }
        let rel = relative_path(root, member_dir)?;
        if set.is_match(&rel) {
            paths.insert(rel);
        }
    }
    Ok(members_with_unique_ids(paths))
}

/// Members named after their directory; a shared basename falls back to the
/// full path for every holder, since per-member declarations are keyed by
/// id. A unique basename cannot equal another member's path, whose own
/// basename would then be shared.
fn members_with_unique_ids(paths: BTreeSet<String>) -> Vec<ResolvedWorkspaceMember> {
    let basename = |rel: &str| rel.rsplit('/').next().unwrap_or(rel).to_owned();
    let mut basename_counts = BTreeMap::new();
    for rel in &paths {
        *basename_counts.entry(basename(rel)).or_insert(0_usize) += 1;
    }
    paths
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
        .collect()
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
pub(crate) fn detect_nested_members(
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
    Ok(members_with_unique_ids(paths))
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

/// `./packages/*/` and `packages\*` both become `packages/*`: a `.` or empty
/// segment left in would never match a walked path.
fn normalize_relative_path(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|part| !matches!(*part, "" | "."))
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use globset::Glob;

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

    fn uv_members(root_path: &Path, patterns: &[&str]) -> Vec<(String, String)> {
        resolve_workspace_members(
            &root(root_path),
            &default_config(),
            Some(&UvWorkspaceHint {
                members: patterns.iter().map(|p| (*p).to_owned()).collect(),
            }),
            &BTreeMap::new(),
        )
        .expect("resolve")
        .into_iter()
        .map(|member| (member.id, member.path))
        .collect()
    }

    #[test]
    fn uv_member_star_stays_within_one_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = "[project]\nname = \"pkg\"\n";
        write(temp.path(), "packages/api/pyproject.toml", project);
        write(
            temp.path(),
            "packages/api/tests/demo/pyproject.toml",
            project,
        );
        assert_eq!(
            uv_members(temp.path(), &["packages/*"]),
            [("api".to_owned(), "packages/api".to_owned())]
        );
    }

    #[test]
    fn uv_member_patterns_and_override_paths_drop_dot_segments() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(temp.path(), "packages/api/pyproject.toml", "");
        assert_eq!(
            uv_members(temp.path(), &["./packages/*/"]),
            [("api".to_owned(), "packages/api".to_owned())]
        );
        assert_eq!(normalize_relative_path("./lib/./x/"), "lib/x");
    }

    #[test]
    fn uv_members_sharing_a_basename_get_path_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        for dir in ["apps/core", "libs/core", "libs/api"] {
            write(temp.path(), &format!("{dir}/pyproject.toml"), "");
        }
        assert_eq!(
            uv_members(temp.path(), &["apps/*", "libs/*"]),
            [
                ("apps/core".to_owned(), "apps/core".to_owned()),
                ("api".to_owned(), "libs/api".to_owned()),
                ("libs/core".to_owned(), "libs/core".to_owned()),
            ]
        );
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

    mod props {
        use std::collections::BTreeSet;

        use super::*;
        use crate::config::WorkspaceOverride;
        use proptest::prelude::*;

        const PARTS: &[&str] = &[
            "a",
            "b",
            "core",
            "openai",
            "legacy",
            "examples",
            "vendor",
            "tests",
            ".hidden",
            "build",
            "node_modules",
        ];

        const EXCLUDES: &[&str] = &["examples", "**/legacy", "a/b"];

        const UV_PATTERNS: &[&str] = &["*", "a/*", "./a/*", "*/core", "a/**", "b/core", "./b/"];

        const SOURCE_PATHS: &[&str] = &[
            "a", "./a", "a/", "a/./core", "a/core", "../out", ".", "./", "b/core", "missing",
        ];

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Kind {
            Project,
            ToolOnly,
            Broken,
        }

        #[derive(Debug, Clone)]
        struct Tree {
            dirs: BTreeMap<Vec<&'static str>, Kind>,
            gitignore: bool,
        }

        fn tree() -> impl Strategy<Value = Tree> {
            (
                prop::collection::btree_map(
                    prop::collection::vec(prop::sample::select(PARTS), 1..7),
                    prop_oneof![
                        4 => Just(Kind::Project),
                        1 => Just(Kind::ToolOnly),
                        1 => Just(Kind::Broken),
                    ],
                    0..10,
                ),
                any::<bool>(),
            )
                .prop_map(|(dirs, gitignore)| Tree { dirs, gitignore })
        }

        fn build(tree: &Tree) -> tempfile::TempDir {
            let temp = tempfile::tempdir().expect("tempdir");
            write(
                temp.path(),
                "pyproject.toml",
                "[project]\nname = \"root\"\n",
            );
            if tree.gitignore {
                write(temp.path(), ".gitignore", "vendor/\n");
            }
            for (dir, kind) in &tree.dirs {
                let text = match kind {
                    Kind::Project => "[project]\nname = \"pkg\"\n",
                    Kind::ToolOnly => "[tool.ruff]\n",
                    Kind::Broken => "[project",
                };
                write(
                    temp.path(),
                    &format!("{}/pyproject.toml", dir.join("/")),
                    text,
                );
            }
            temp
        }

        fn glob_set(patterns: &[&str]) -> GlobSet {
            let mut builder = GlobSetBuilder::new();
            for pattern in patterns {
                builder.add(Glob::new(pattern).expect("glob"));
            }
            builder.build().expect("globset")
        }

        /// #488 auto-detection, spelled out per directory instead of per walk.
        fn model_nested(tree: &Tree, excludes: &[&str]) -> Vec<(String, String)> {
            let exclude = glob_set(excludes);
            let paths: BTreeSet<String> = tree
                .dirs
                .iter()
                .filter(|(dir, kind)| {
                    **kind == Kind::Project
                        && dir.len() <= AUTO_MEMBER_MAX_DEPTH
                        && !dir.iter().any(|part| {
                            part.starts_with('.')
                                || matches!(*part, "tests" | "build" | "node_modules")
                                || (tree.gitignore && *part == "vendor")
                        })
                        && (1..=dir.len()).all(|len| {
                            let prefix = dir[..len].join("/");
                            !exclude.is_match(&prefix) && !exclude.is_match(format!("{prefix}/**"))
                        })
                })
                .map(|(dir, _)| dir.join("/"))
                .collect();
            let basename = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
            paths
                .iter()
                .map(|path| {
                    let shared = paths
                        .iter()
                        .filter(|other| basename(other) == basename(path))
                        .count()
                        > 1;
                    let id = if shared { path.clone() } else { basename(path) };
                    (id, path.clone())
                })
                .collect()
        }

        /// What `uv` itself would take as members: `*` stays within one path
        /// component, and any `pyproject.toml` (even tool-only) counts.
        fn model_uv(tree: &Tree, patterns: &[&str]) -> BTreeSet<String> {
            let mut builder = GlobSetBuilder::new();
            for pattern in patterns {
                let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
                builder.add(
                    globset::GlobBuilder::new(pattern)
                        .literal_separator(true)
                        .build()
                        .expect("glob"),
                );
            }
            let set = builder.build().expect("globset");
            tree.dirs
                .keys()
                .filter(|dir| !dir.iter().any(|part| matches!(*part, "node_modules")))
                .map(|dir| dir.join("/"))
                .filter(|path| set.is_match(path))
                .collect()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn detect_nested_members_matches_reference_model(
                tree in tree(),
                excludes in prop::sample::subsequence(EXCLUDES, 0..=EXCLUDES.len()),
            ) {
                let temp = build(&tree);
                prop_assert_eq!(detect(temp.path(), &excludes), model_nested(&tree, &excludes));
            }

            /// Members from uv globs, path sources and overrides together:
            /// one member per directory, each a normalized path below the
            /// root, uv members exactly the directories uv would pick, and
            /// ids unique so per-member declarations never overwrite.
            #[test]
            fn resolved_members_are_distinct_normalized_directories(
                tree in tree(),
                patterns in prop::option::of(prop::sample::subsequence(UV_PATTERNS, 1..=3)),
                sources in prop::collection::btree_map(
                    prop::sample::select(&["acme", "core", "x"][..]),
                    prop::sample::select(SOURCE_PATHS),
                    0..3,
                ),
                override_path in prop::option::of(prop::sample::select(&["a", "./a", "a/", "b/core"][..])),
            ) {
                let temp = build(&tree);
                let mut config = default_config();
                if let Some(path) = override_path {
                    config.workspaces.insert(
                        "ov".to_owned(),
                        WorkspaceOverride {
                            path: path.to_owned(),
                            entry: None,
                            project: None,
                            mode: None,
                        },
                    );
                }
                let hint = patterns.as_ref().map(|patterns| UvWorkspaceHint {
                    members: patterns.iter().map(|p| (*p).to_owned()).collect(),
                });
                let sources: BTreeMap<String, Vec<String>> = sources
                    .into_iter()
                    .map(|(name, path)| (name.to_owned(), vec![path.to_owned()]))
                    .collect();
                let members = resolve_workspace_members(
                    &root(temp.path()),
                    &config,
                    hint.as_ref(),
                    &sources,
                )
                .expect("resolve");

                let mut dirs = BTreeSet::new();
                for member in &members {
                    prop_assert!(
                        member.path.split('/').all(|part| !matches!(part, "" | "." | "..")),
                        "member path `{}` is not normalized", member.path
                    );
                    let dir = temp.path().join(&member.path);
                    let key = dir.canonicalize().unwrap_or(dir);
                    prop_assert!(dirs.insert(key), "two members for `{}`", member.path);
                }
                // Only uv members' ids are checked: a path source keeps its
                // distribution name even when a uv member's basename matches
                // it (reported, not changed).
                let uv_members: Vec<&ResolvedWorkspaceMember> = members
                    .iter()
                    .filter(|m| m.id != "ov" && !sources.contains_key(&m.id))
                    .collect();
                let ids: BTreeSet<&str> = uv_members.iter().map(|m| m.id.as_str()).collect();
                prop_assert_eq!(ids.len(), uv_members.len(), "duplicate ids in {:?}", members);

                if let Some(patterns) = &patterns {
                    let uv: BTreeSet<String> = uv_members.iter().map(|m| m.path.clone()).collect();
                    let expected = model_uv(&tree, patterns);
                    for path in &uv {
                        prop_assert!(expected.contains(path), "uv would not pick `{}`", path);
                    }
                    let taken: BTreeSet<&str> = members.iter().map(|m| m.path.as_str()).collect();
                    for path in &expected {
                        prop_assert!(taken.contains(path.as_str()), "uv member `{}` missing", path);
                    }
                }
            }
        }
    }
}
