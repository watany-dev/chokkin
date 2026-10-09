//! §8 automatic entry detection from discovered source files.

use crate::config::EntrySpec;
use crate::sources::{DiscoveredSources, FileContext, FileKind, ProjectLayout};

use super::types::{EntryCandidate, EntryOrigin};

const SHALLOW_ENTRY_NAMES: &[&str] = &[
    "main.py",
    "app.py",
    "manage.py",
    "asgi.py",
    "wsgi.py",
    "noxfile.py",
];

const ALL_DEPTH_ENTRY_NAMES: &[&str] = &["__main__.py", "conftest.py"];

const EXACT_PATH_ENTRIES: &[&str] = &["docs/conf.py", "alembic/env.py"];

/// Top-level trees of standalone scripts run directly, e.g.
/// `python examples/foo.py` or `fastmcp run docs/demo.py` (#666).
const SCRIPT_TREES: &[&str] = &["examples", "docs"];

/// Collect auto-detected entry candidates from discovered files (§8).
#[must_use]
pub(super) fn detect_auto_entries(sources: &DiscoveredSources) -> Vec<EntryCandidate> {
    let mut candidates = Vec::new();
    let layout = &sources.layout;

    for file in sources.python_files() {
        let path = file.path.as_str();
        let file_name = path.rsplit('/').next().unwrap_or(path);

        if ALL_DEPTH_ENTRY_NAMES.contains(&file_name) {
            candidates.push(candidate(path, file.context, format!("auto:{file_name}")));
            continue;
        }

        if SHALLOW_ENTRY_NAMES.contains(&file_name)
            && is_shallow_entry_path(path, file_name, layout)
        {
            candidates.push(candidate(path, file.context, format!("auto:{file_name}")));
            continue;
        }

        if EXACT_PATH_ENTRIES.contains(&path) {
            candidates.push(candidate(path, file.context, format!("auto:{path}")));
            continue;
        }

        if let Some(tree) = script_tree(path, layout) {
            candidates.push(candidate(path, file.context, format!("auto:{tree}/**")));
            continue;
        }

        if path.starts_with("scripts/")
            && std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
        {
            candidates.push(candidate(path, file.context, "auto:scripts/**".to_owned()));
        }
    }

    // A notebook is run cell by cell and nothing imports it, so it can only
    // ever be a root; the modules it imports are reachable through it (#514).
    for file in sources
        .files
        .iter()
        .filter(|file| file.kind == FileKind::Notebook)
    {
        candidates.push(candidate(
            &file.path,
            file.context,
            "auto:**/*.ipynb".to_owned(),
        ));
    }

    candidates
}

fn candidate(path: &str, context: FileContext, rule: String) -> EntryCandidate {
    EntryCandidate {
        spec: EntrySpec {
            path: path.to_owned(),
            symbol: None,
        },
        context,
        origin: EntryOrigin::Auto { rule },
    }
}

/// The script tree holding `path`, directly under the innermost workspace
/// member or else the project root, unless the tree is one of that layout's
/// packages (a flat `examples` package is library code).
fn script_tree(path: &str, layout: &crate::sources::LayoutInfo) -> Option<&'static str> {
    let (rest, layout) = layout
        .member_for(path)
        .map_or((path, layout), |(member, rest)| (rest, &member.layout));
    let (top, _) = rest.split_once('/')?;
    let tree = SCRIPT_TREES.iter().copied().find(|tree| *tree == top)?;
    let in_package = layout.packages.iter().any(|package| {
        rest.strip_prefix(layout.package_dir(package).as_str())
            .is_some_and(|tail| tail.starts_with('/'))
    });
    (!in_package).then_some(tree)
}

fn is_shallow_entry_path(path: &str, file_name: &str, layout: &crate::sources::LayoutInfo) -> bool {
    if !path.contains('/') {
        return true;
    }

    layout.layout != ProjectLayout::Unknown
        && layout
            .packages
            .iter()
            .any(|package| path == format!("{}/{file_name}", layout.package_dir(package)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::assign_file_context;
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo, MemberLayout, ProjectLayout,
    };

    fn sources_with(paths: &[&str], layout: &LayoutInfo) -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout: layout.clone(),
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: assign_file_context(path),
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn src_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        }
    }

    #[test]
    fn detects_root_manage_py() {
        let sources = sources_with(&["manage.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(entries.iter().any(|entry| entry.spec.path == "manage.py"));
    }

    #[test]
    fn detects_root_app_modules() {
        let names = ["main.py", "app.py", "asgi.py", "wsgi.py"];
        let sources = sources_with(&names, &src_layout());
        let entries = detect_auto_entries(&sources);
        for name in names {
            assert!(
                entries.iter().any(|entry| entry.spec.path == name),
                "{name}"
            );
        }
    }

    #[test]
    fn detects_lib_package_asgi_py() {
        let layout = LayoutInfo {
            package_root: "lib".to_owned(),
            ..src_layout()
        };
        let sources = sources_with(&["lib/acme/asgi.py", "src/acme/asgi.py"], &layout);
        let paths: Vec<_> = detect_auto_entries(&sources)
            .into_iter()
            .map(|entry| entry.spec.path)
            .collect();
        assert_eq!(paths, ["lib/acme/asgi.py"]);
    }

    #[test]
    fn detects_src_package_asgi_py() {
        let sources = sources_with(&["src/acme/asgi.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(
            entries
                .iter()
                .any(|entry| entry.spec.path == "src/acme/asgi.py")
        );
    }

    #[test]
    fn ignores_deep_main_py() {
        let sources = sources_with(&["src/acme/api/main.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.spec.path == "src/acme/api/main.py")
        );
    }

    #[test]
    fn detects_nested_main_py() {
        let sources = sources_with(&["src/acme/__main__.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(
            entries
                .iter()
                .any(|entry| entry.spec.path == "src/acme/__main__.py")
        );
    }

    #[test]
    fn detects_scripts_tree() {
        let sources = sources_with(&["scripts/deploy.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(
            entries
                .iter()
                .any(|entry| entry.spec.path == "scripts/deploy.py")
        );
    }

    #[test]
    fn detects_examples_and_docs_trees() {
        let sources = sources_with(
            &[
                "examples/apps/server.py",
                "docs/apps/demos/demo.py",
                "docs/conf.py",
            ],
            &src_layout(),
        );
        let rules: Vec<_> = detect_auto_entries(&sources)
            .into_iter()
            .map(|entry| {
                let EntryOrigin::Auto { rule } = entry.origin else {
                    panic!("auto entry expected");
                };
                (entry.spec.path, rule)
            })
            .collect();
        assert_eq!(
            rules,
            [
                (
                    "examples/apps/server.py".to_owned(),
                    "auto:examples/**".to_owned()
                ),
                (
                    "docs/apps/demos/demo.py".to_owned(),
                    "auto:docs/**".to_owned()
                ),
                ("docs/conf.py".to_owned(), "auto:docs/conf.py".to_owned()),
            ]
        );
    }

    #[test]
    fn detects_member_examples_tree() {
        let layout = LayoutInfo {
            members: vec![MemberLayout {
                path: "integrations/foo".to_owned(),
                layout: src_layout(),
            }],
            ..src_layout()
        };
        let sources = sources_with(
            &[
                "integrations/foo/examples/demo.py",
                "integrations/examples/x.py",
            ],
            &layout,
        );
        let paths: Vec<_> = detect_auto_entries(&sources)
            .into_iter()
            .map(|entry| entry.spec.path)
            .collect();
        assert_eq!(paths, ["integrations/foo/examples/demo.py"]);
    }

    #[test]
    fn ignores_package_of_member_under_examples() {
        let layout = LayoutInfo {
            members: vec![MemberLayout {
                path: "examples/plugin".to_owned(),
                layout: LayoutInfo {
                    packages: vec!["plugin".to_owned()],
                    ..src_layout()
                },
            }],
            ..src_layout()
        };
        let sources = sources_with(&["examples/plugin/src/plugin/core.py"], &layout);
        let paths: Vec<_> = detect_auto_entries(&sources)
            .into_iter()
            .map(|entry| entry.spec.path)
            .collect();
        assert_eq!(paths, Vec::<String>::new());
    }

    #[test]
    fn ignores_examples_package() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            package_root: String::new(),
            packages: vec!["examples".to_owned()],
            ..src_layout()
        };
        let sources = sources_with(&["examples/demo.py", "src/acme/examples/demo.py"], &layout);
        let paths: Vec<_> = detect_auto_entries(&sources)
            .into_iter()
            .map(|entry| entry.spec.path)
            .collect();
        assert_eq!(paths, Vec::<String>::new());
    }
}
