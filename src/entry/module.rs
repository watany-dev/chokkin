//! First-party module → file path resolution for entry targets.

use std::collections::{BTreeSet, HashMap};
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

/// Project files by each `/`-bounded tail of their path, for resolving many
/// names over one file set. The directories a name may resolve under are the
/// same for every name, so each is ranked once in the order
/// [`resolve_module_to_path`] tries them, and a lookup ranks only the few
/// files whose path ends in the name: airflow's 11k string literals walked
/// its 133 members over every file for 3 s (#1025).
pub(crate) struct ModuleIndex<'a> {
    by_tail: HashMap<&'a str, Vec<&'a str>>,
    base_rank: HashMap<String, usize>,
}

impl<'a> ModuleIndex<'a> {
    pub(crate) fn new(known_paths: &'a BTreeSet<String>, layout: &LayoutInfo) -> Self {
        let mut by_tail: HashMap<&str, Vec<&str>> = HashMap::new();
        for path in known_paths {
            let tails = path.match_indices('/').map(|(slash, _)| &path[slash + 1..]);
            for tail in std::iter::once(path.as_str()).chain(tails) {
                by_tail.entry(tail).or_default().push(path);
            }
        }
        let mut bases = Vec::new();
        push_bases(layout, "", &mut bases);
        let mut base_rank = HashMap::new();
        for (rank, base) in bases.into_iter().enumerate() {
            base_rank.entry(base).or_insert(rank);
        }
        Self { by_tail, base_rank }
    }

    /// What [`resolve_module_to_path`] returns over the indexed files.
    pub(crate) fn resolve(&self, module: &str) -> Option<String> {
        let suffix = normalize_module_target(module).replace('.', "/");
        [format!("{suffix}/__init__.py"), format!("{suffix}.py")]
            .iter()
            .enumerate()
            .flat_map(|(kind, tail)| {
                let files = self.by_tail.get(tail.as_str()).into_iter().flatten();
                files.filter_map(move |path| {
                    let base = path.strip_suffix(tail.as_str())?;
                    let base = base.strip_suffix('/').unwrap_or(base);
                    Some(((self.base_rank.get(base)?, kind), *path))
                })
            })
            .min()
            .map(|(_, path)| path.to_owned())
    }
}

/// The directories [`resolve_in`] joins a name to, in its order.
fn push_bases(layout: &LayoutInfo, prefix: &str, bases: &mut Vec<String>) {
    let package_root = package_root(layout);
    if !package_root.is_empty() {
        bases.push(join_rel(prefix, package_root));
    }
    bases.push(prefix.to_owned());
    for member in &layout.members {
        push_bases(&member.layout, &join_rel(prefix, &member.path), bases);
    }
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
    let package_root = package_root(layout);
    std::iter::once(package_root)
        .chain((!package_root.is_empty()).then_some(""))
        .flat_map(|root| {
            let stem = join_rel(prefix, &join_rel(root, suffix));
            // The path finder tries the package before the module (#1108).
            [format!("{stem}/__init__.py"), format!("{stem}.py")]
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

fn package_root(layout: &LayoutInfo) -> &str {
    match layout.layout {
        ProjectLayout::Src | ProjectLayout::PackageRoot => layout.package_root.as_str(),
        ProjectLayout::Unknown => "src",
        ProjectLayout::Flat => "",
    }
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

    /// `foo/__init__.py` shadows `foo.py` beside it, as in the path finder (#1108).
    #[test]
    fn a_package_wins_over_the_module_beside_it() {
        let layout = src_layout();
        let paths = known(&["src/acme/foo.py", "src/acme/foo/__init__.py"]);
        let package = Some("src/acme/foo/__init__.py".to_owned());
        assert_eq!(resolve_module_to_path("acme.foo", &layout, &paths), package);
        assert_eq!(
            ModuleIndex::new(&paths, &layout).resolve("acme.foo"),
            package
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

            /// The index answers what the resolver answers over the same
            /// files, for any name, layout and workspace member (#1025).
            #[test]
            fn module_index_agrees_with_the_resolver(
                layout in layout_strategy(),
                members in prop::collection::vec(
                    (prop::sample::select(&["svc/a", "svc/b", "x"][..]), layout_strategy()),
                    0..3,
                ),
                paths in prop::collection::btree_set(
                    (
                        prop::sample::select(&["", "svc/a/", "svc/b/src/", "x/lib/"][..]),
                        prop::collection::vec(prop::sample::select(PARTS), 1..5),
                        any::<bool>(),
                    )
                        .prop_map(|(member, parts, init)| {
                            let stem = parts.join("/");
                            let file = if init { "/__init__.py" } else { ".py" };
                            format!("{member}{stem}{file}")
                        }),
                    0..12,
                ),
                names in prop::collection::vec(
                    (prop::collection::vec(prop::sample::select(PARTS), 1..5), any::<bool>())
                        .prop_map(|(parts, app_config)| {
                            let name = parts.join(".");
                            if app_config { format!("{name}.AcmeAppConfig") } else { name }
                        }),
                    1..8,
                ),
            ) {
                let layout = LayoutInfo {
                    members: members
                        .into_iter()
                        .map(|(path, layout)| MemberLayout { path: path.to_owned(), layout })
                        .collect(),
                    ..layout
                };
                let index = ModuleIndex::new(&paths, &layout);
                for name in &names {
                    prop_assert_eq!(
                        index.resolve(name),
                        resolve_module_to_path(name, &layout, &paths),
                        "{}",
                        name
                    );
                }
            }

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
                    // files; both the index and this resolver take the
                    // package, as the path finder does (#1108).
                    let twin = path
                        .strip_suffix(".py")
                        .map(|stem| format!("{stem}/__init__.py"))
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
