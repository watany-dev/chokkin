//! First-party module → file path resolution for entry targets.

use std::collections::BTreeSet;

use crate::sources::{LayoutInfo, ProjectLayout};

/// Resolve a dotted module name to a root-relative `.py` path present in `known_paths`.
#[must_use]
pub(super) fn resolve_module_to_path(
    module: &str,
    layout: &LayoutInfo,
    known_paths: &BTreeSet<String>,
) -> Option<String> {
    let normalized = normalize_module_target(module);
    let suffix = normalized.replace('.', "/");
    let mut candidates = Vec::new();

    if layout.layout != ProjectLayout::Unknown
        && layout
            .packages
            .iter()
            .any(|package| normalized == package || normalized.starts_with(&format!("{package}.")))
    {
        let path = layout.package_dir(&suffix);
        candidates.push(format!("{path}.py"));
        candidates.push(format!("{path}/__init__.py"));
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
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            ..Default::default()
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
    fn resolves_module_under_declared_package_root() {
        let layout = LayoutInfo {
            package_root: "lib".to_owned(),
            ..src_layout()
        };
        let paths = known(&["lib/acme/web/cli.py", "lib/acme/__init__.py"]);
        assert_eq!(
            resolve_module_to_path("acme.web.cli", &layout, &paths),
            Some("lib/acme/web/cli.py".to_owned())
        );
        assert_eq!(
            resolve_module_to_path("acme", &layout, &paths),
            Some("lib/acme/__init__.py".to_owned())
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
            ..Default::default()
        };
        let paths = known(&["acme/foo.py"]);
        assert_eq!(
            resolve_module_to_path("acme.foo", &layout, &paths),
            Some("acme/foo.py".to_owned())
        );
    }

    #[test]
    fn workspace_member_src_layout_resolves_module_file() {
        let layout = LayoutInfo::default();
        let paths = known(&["services/api/src/api/main.py"]);
        assert_eq!(
            resolve_module_to_path("api.main", &layout, &paths),
            Some("services/api/src/api/main.py".to_owned())
        );
    }

    mod props {
        use super::*;
        use crate::sources::path_to_module;
        use proptest::prelude::*;

        const PARTS: &[&str] = &["acme", "api", "core", "x", "src", "lib", "tests"];

        fn layout_strategy() -> impl Strategy<Value = LayoutInfo> {
            (
                prop::sample::select(&[ProjectLayout::Src, ProjectLayout::Flat][..]),
                prop::sample::select(&["src", "lib", "python"][..]),
                prop::sample::subsequence(PARTS, 1..3),
            )
                .prop_map(|(layout, root, packages)| LayoutInfo {
                    layout,
                    package_root: if layout == ProjectLayout::Src {
                        root.to_owned()
                    } else {
                        String::new()
                    },
                    packages: packages.into_iter().map(str::to_owned).collect(),
                    ..Default::default()
                })
        }

        /// Files inside the layout's packages: `<pkg>/<parts>.py` or
        /// `<pkg>/<parts>/__init__.py`, under the package root.
        fn files_strategy(layout: &LayoutInfo) -> impl Strategy<Value = BTreeSet<String>> + use<> {
            let packages = layout.packages.clone();
            let package_root = layout.package_root.clone();
            prop::collection::btree_set(
                (
                    prop::sample::select(packages),
                    prop::collection::vec(prop::sample::select(PARTS), 0..3),
                    any::<bool>(),
                ),
                1..8,
            )
            .prop_map(move |entries| {
                entries
                    .into_iter()
                    .map(|(package, parts, init)| {
                        let mut segments = vec![package.as_str()];
                        segments.extend(parts);
                        let stem = segments.join("/");
                        let file = if init || segments.len() == 1 {
                            format!("{stem}/__init__.py")
                        } else {
                            format!("{stem}.py")
                        };
                        if package_root.is_empty() {
                            file
                        } else {
                            format!("{package_root}/{file}")
                        }
                    })
                    .collect()
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Every package file's module name resolves back to that file,
            /// so entry targets and the module index agree on which file a
            /// dotted name means.
            #[test]
            fn package_module_names_resolve_back_to_their_file(
                (layout, paths) in layout_strategy()
                    .prop_flat_map(|layout| (Just(layout.clone()), files_strategy(&layout)))
            ) {
                for path in &paths {
                    let Some(module) = path_to_module(path, &layout) else {
                        continue;
                    };
                    let resolved = resolve_module_to_path(&module, &layout, &paths);
                    // `x.py` beside `x/__init__.py` is one module name for two
                    // files; both the index and this resolver take `x.py`.
                    let twin = path
                        .strip_suffix("/__init__.py")
                        .map(|stem| format!("{stem}.py"))
                        .filter(|twin| paths.contains(twin));
                    prop_assert_eq!(
                        resolved,
                        Some(twin.unwrap_or_else(|| path.clone())),
                        "{} -> {}",
                        path,
                        module
                    );
                }
            }
        }
    }
}
