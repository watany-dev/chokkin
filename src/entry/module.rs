//! First-party module → file path resolution for entry targets.

use std::collections::BTreeSet;

use crate::sources::{LayoutInfo, ProjectLayout};

/// Resolve a dotted module name to a root-relative `.py` path present in `known_paths`.
#[must_use]
pub fn resolve_module_to_path(
    module: &str,
    layout: &LayoutInfo,
    known_paths: &BTreeSet<String>,
) -> Option<String> {
    let normalized = normalize_module_target(module);
    let suffix = normalized.replace('.', "/");
    let mut candidates = Vec::new();

    match layout.layout {
        ProjectLayout::Src => {
            candidates.push(format!("src/{suffix}.py"));
            candidates.push(format!("src/{suffix}/__init__.py"));
        },
        ProjectLayout::Flat => {
            for package in &layout.packages {
                if normalized == *package || normalized.starts_with(&format!("{package}.")) {
                    candidates.push(format!("{suffix}.py"));
                    candidates.push(format!("{suffix}/__init__.py"));
                }
            }
        },
        ProjectLayout::Unknown => {},
    }

    candidates.push(format!("src/{suffix}.py"));
    candidates.push(format!("src/{suffix}/__init__.py"));
    candidates.push(format!("{suffix}.py"));
    candidates.push(format!("{suffix}/__init__.py"));

    if let Some(path) = candidates
        .into_iter()
        .find(|path| known_paths.contains(path))
    {
        return Some(path);
    }

    known_paths
        .iter()
        .find(|path| {
            path.ends_with(&format!("/src/{suffix}.py"))
                || path.ends_with(&format!("/src/{suffix}/__init__.py"))
        })
        .cloned()
}

/// Normalize Django `AppConfig` targets to their package root when applicable.
fn normalize_module_target(module: &str) -> &str {
    if module.ends_with("AppConfig")
        && let Some(first) = module.split('.').next()
    {
        return first;
    }
    module
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::ProjectLayout;

    fn src_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        }
    }

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn resolves_src_layout_module_file() {
        let layout = src_layout();
        let paths = known(&["src/acme/api/routes.py"]);
        assert_eq!(
            resolve_module_to_path("acme.api.routes", &layout, &paths),
            Some("src/acme/api/routes.py".to_owned())
        );
    }

    #[test]
    fn resolves_src_layout_package_init() {
        let layout = src_layout();
        let paths = known(&["src/acme/__init__.py"]);
        assert_eq!(
            resolve_module_to_path("acme", &layout, &paths),
            Some("src/acme/__init__.py".to_owned())
        );
    }

    #[test]
    fn normalizes_app_config_suffix() {
        let layout = src_layout();
        let paths = known(&["src/acme/__init__.py"]);
        assert_eq!(
            resolve_module_to_path("acme.apps.AcmeAppConfig", &layout, &paths),
            Some("src/acme/__init__.py".to_owned())
        );
    }

    #[test]
    fn flat_layout_resolves_package_module() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        let paths = known(&["acme/foo.py"]);
        assert_eq!(
            resolve_module_to_path("acme.foo", &layout, &paths),
            Some("acme/foo.py".to_owned())
        );
    }

    #[test]
    fn workspace_member_src_layout_resolves_module_file() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        let paths = known(&["services/api/src/api/main.py"]);
        assert_eq!(
            resolve_module_to_path("api.main", &layout, &paths),
            Some("services/api/src/api/main.py".to_owned())
        );
    }
}
