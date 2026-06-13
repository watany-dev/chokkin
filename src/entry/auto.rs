//! §8 automatic entry detection from discovered source files.

use crate::config::EntrySpec;
use crate::sources::{DiscoveredSources, FileContext, ProjectLayout};

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

const EXACT_PATH_ENTRIES: &[(&str, &str)] = &[
    ("docs/conf.py", "docs/conf.py"),
    ("alembic/env.py", "alembic/env.py"),
];

/// Collect auto-detected entry candidates from discovered files (§8).
#[must_use]
pub fn detect_auto_entries(sources: &DiscoveredSources) -> Vec<EntryCandidate> {
    let mut candidates = Vec::new();
    let layout = &sources.layout;

    for file in sources.python_files() {
        let path = file.path.as_str();
        let file_name = path.rsplit('/').next().unwrap_or(path);

        if ALL_DEPTH_ENTRY_NAMES.contains(&file_name) {
            candidates.push(candidate(path, file.context, format!("auto:{file_name}")));
            continue;
        }

        if SHALLOW_ENTRY_NAMES.contains(&file_name) && is_shallow_entry_path(path, layout) {
            candidates.push(candidate(path, file.context, format!("auto:{file_name}")));
            continue;
        }

        for (expected, rule) in EXACT_PATH_ENTRIES {
            if path == *expected {
                candidates.push(candidate(path, file.context, format!("auto:{rule}")));
            }
        }

        if path.starts_with("scripts/")
            && std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
        {
            candidates.push(candidate(path, file.context, "auto:scripts/**".to_owned()));
        }
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

fn is_shallow_entry_path(path: &str, layout: &crate::sources::LayoutInfo) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if !SHALLOW_ENTRY_NAMES.contains(&file_name) {
        return false;
    }
    if !path.contains('/') {
        return true;
    }

    match layout.layout {
        ProjectLayout::Src => layout
            .packages
            .iter()
            .any(|package| path == format!("src/{package}/{file_name}")),
        ProjectLayout::Flat => layout
            .packages
            .iter()
            .any(|package| path == format!("{package}/{file_name}")),
        ProjectLayout::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::assign_file_context;
    use crate::sources::{DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo, ProjectLayout};

    fn sources_with(paths: &[&str], layout: &LayoutInfo) -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: layout.clone(),
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: assign_file_context(path, layout),
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn src_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        }
    }

    #[test]
    fn detects_root_manage_py() {
        let sources = sources_with(&["manage.py"], &src_layout());
        let entries = detect_auto_entries(&sources);
        assert!(entries.iter().any(|entry| entry.spec.path == "manage.py"));
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
}
