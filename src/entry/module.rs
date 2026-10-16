//! First-party module → file path resolution for entry targets.

use std::collections::BTreeSet;
use std::path::Path;

use crate::path_util::join_rel;
use crate::sources::{LayoutInfo, ProjectLayout};

/// Resolve a dotted module name to a root-relative `.py` path present in `known_paths`.
#[must_use]
pub(crate) fn resolve_module_to_path(
    module: &str,
    layout: &LayoutInfo,
    known_paths: &BTreeSet<String>,
) -> Option<String> {
    let suffix = normalize_module_target(module).replace('.', "/");
    resolve_in(&suffix, layout, "", &|path| known_paths.contains(path))
}

/// Where `module` would resolve if every `.py` file under `root` were
/// discovered: a target discovery left out (an `exclude`) still names a file
/// whose imports go unread, unlike one naming no file at all (#933).
#[must_use]
pub(crate) fn resolve_module_on_disk(
    module: &str,
    layout: &LayoutInfo,
    root: &Path,
) -> Option<String> {
    let suffix = normalize_module_target(module).replace('.', "/");
    resolve_in(&suffix, layout, "", &|path| {
        path.split('/')
            .fold(root.to_path_buf(), |dir, part| dir.join(part))
            .is_file()
    })
}

/// Look where `layout` names modules, then at the root, which `python -m`
/// and app runners put on `sys.path` as the working directory, then inside
/// each workspace member under `prefix`.
fn resolve_in(
    suffix: &str,
    layout: &LayoutInfo,
    prefix: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let package_root = match layout.layout {
        ProjectLayout::Src | ProjectLayout::PackageRoot => layout.package_root.as_str(),
        ProjectLayout::Unknown => "src",
        ProjectLayout::Flat => "",
    };
    std::iter::once(package_root)
        .chain((!package_root.is_empty()).then_some(""))
        .flat_map(|root| {
            let stem = join_rel(prefix, &join_rel(root, suffix));
            [format!("{stem}.py"), format!("{stem}/__init__.py")]
        })
        .find(|path| exists(path))
        .or_else(|| {
            layout.members.iter().find_map(|member| {
                resolve_in(
                    suffix,
                    &member.layout,
                    &join_rel(prefix, &member.path),
                    exists,
                )
            })
        })
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
    use crate::sources::{MemberLayout, ProjectLayout};

    fn src_layout() -> LayoutInfo {
        LayoutInfo::new("src", vec!["acme".to_owned()])
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
        let layout = LayoutInfo::new("lib", vec!["acme".to_owned()]);
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
        let layout = LayoutInfo::new("", vec!["acme".to_owned()]);
        let paths = known(&["acme/foo.py"]);
        assert_eq!(
            resolve_module_to_path("acme.foo", &layout, &paths),
            Some("acme/foo.py".to_owned())
        );
    }

    fn with_members(members: &[(&str, LayoutInfo)]) -> LayoutInfo {
        LayoutInfo {
            members: members
                .iter()
                .map(|(path, layout)| MemberLayout {
                    path: (*path).to_owned(),
                    layout: layout.clone(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn workspace_member_src_layout_resolves_module_file() {
        let member = LayoutInfo::new("src", vec!["api".to_owned()]);
        let layout = with_members(&[("services/api", member)]);
        let paths = known(&["services/api/src/api/main.py"]);
        assert_eq!(
            resolve_module_to_path("api.main", &layout, &paths),
            Some("services/api/src/api/main.py".to_owned())
        );
    }

    #[test]
    fn workspace_member_flat_layout_resolves_module_file() {
        let member = LayoutInfo::new("", vec!["llama_dev".to_owned()]);
        let layout = with_members(&[("llama-dev", member)]);
        let paths = known(&["llama-dev/llama_dev/cli.py"]);
        assert_eq!(
            resolve_module_to_path("llama_dev.cli", &layout, &paths),
            Some("llama-dev/llama_dev/cli.py".to_owned())
        );
    }

    #[test]
    fn src_tree_outside_every_member_is_not_searched() {
        // `vendor/x` is no member, so nothing imports `api` from there.
        let paths = known(&["vendor/x/src/api/main.py"]);
        assert_eq!(
            resolve_module_to_path("api.main", &LayoutInfo::default(), &paths),
            None
        );
    }

    #[test]
    fn top_level_module_under_declared_package_root_resolves() {
        let layout = LayoutInfo::new("lib", vec!["acme".to_owned()]);
        let paths = known(&["lib/acme_tool.py"]);
        assert_eq!(
            resolve_module_to_path("acme_tool", &layout, &paths),
            Some("lib/acme_tool.py".to_owned())
        );
    }

    #[test]
    fn root_module_resolves_beside_a_package_root() {
        // `python -m cli` runs from the root whatever the layout.
        let paths = known(&["cli.py", "tests/helpers.py"]);
        assert_eq!(
            resolve_module_to_path("cli", &src_layout(), &paths),
            Some("cli.py".to_owned())
        );
        assert_eq!(
            resolve_module_to_path("tests.helpers", &src_layout(), &paths),
            Some("tests/helpers.py".to_owned())
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
                .prop_map(|(layout, root, packages)| {
                    let root = if layout == ProjectLayout::Flat {
                        ""
                    } else {
                        root
                    };
                    LayoutInfo::new(root, packages.into_iter().map(str::to_owned).collect())
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
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(256)))]

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
