//! Dependencies consumed by an imported library rather than by its caller.

use std::collections::BTreeSet;
use std::ops::Bound;

use crate::graph::ModuleOrigin;
use crate::manifest::normalize_distribution_name;
use crate::parser::ParsedModule;
use crate::resolver::{RUNTIME_PEERS, ResolvedImport};

/// Modules a scope uses, whose runtime peers they load: each third-party
/// import's module and, for a `from` import of one, the name below it, which
/// may be a submodule (`from lxml import cssselect`) or a backend class.
pub(super) fn used_modules<'a>(
    imports: impl IntoIterator<Item = &'a ResolvedImport>,
    parsed: impl IntoIterator<Item = &'a ParsedModule>,
) -> BTreeSet<String> {
    let mut modules: BTreeSet<String> = imports
        .into_iter()
        .filter(|import| import.origin == ModuleOrigin::ThirdParty)
        .map(|import| import.full_module.clone())
        .collect();
    let names: Vec<String> = parsed
        .into_iter()
        .flat_map(|module| &module.imports)
        .filter(|import| modules.contains(&import.module))
        .filter_map(|import| Some(format!("{}.{}", import.module, import.name.as_ref()?)))
        .collect();
    modules.extend(names);
    modules
}

/// The `declared` distributions used as a companion of a used one, shared by
/// project and PEP 723 scopes. A peer can be a provider itself (a cache
/// backend's `redis` provides `hiredis`), so it repeats until none is added.
pub(super) fn used_companions<'a>(
    declared: &[&'a str],
    used: impl Fn(&str) -> bool,
    modules: &BTreeSet<String>,
) -> Vec<&'a str> {
    let mut found: Vec<&str> = Vec::new();
    loop {
        let more: Vec<_> = declared
            .iter()
            .copied()
            .filter(|name| {
                !found.contains(name)
                    && companion_is_used(
                        name,
                        |provider| used(provider) || found.contains(&provider),
                        modules,
                    )
            })
            .collect();
        if more.is_empty() {
            return found;
        }
        found.extend(more);
    }
}

/// An unused provider does not use its peers or stubs. `modules` are the
/// third-party modules the scope uses, so a peer a module loads is used with
/// it: Django's Postgres backend named in `DATABASES` uses `psycopg`, an
/// import of `lxml.cssselect` uses `cssselect`.
fn companion_is_used(name: &str, used: impl Fn(&str) -> bool, modules: &BTreeSet<String>) -> bool {
    name.strip_prefix("types-")
        .or_else(|| name.strip_suffix("-stubs"))
        .is_some_and(|runtime| used(&normalize_distribution_name(runtime)))
        || RUNTIME_PEERS.iter().any(|entry| {
            entry.peer == name
                && (entry.providers.iter().any(|provider| used(provider))
                    || entry
                        .modules
                        .iter()
                        .any(|module| uses_module(modules, module)))
        })
}

/// Whether `modules` has `module` or a name below it.
fn uses_module(modules: &BTreeSet<String>, module: &str) -> bool {
    modules
        .range::<str, _>((Bound::Included(module), Bound::Unbounded))
        .take_while(|used| used.starts_with(module))
        .any(|used| used.len() == module.len() || used[module.len()..].starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_providers_do_not_hide_their_peers_or_stubs() {
        for name in [
            "accelerate",
            "torch",
            "rouge-score",
            "types-requests",
            "pandas-stubs",
        ] {
            assert!(
                !companion_is_used(name, |_| false, &BTreeSet::new()),
                "{name}"
            );
        }
        assert!(companion_is_used(
            "types-requests",
            |name| name == "requests",
            &BTreeSet::new()
        ));
        assert!(companion_is_used(
            "pandas-stubs",
            |name| name == "pandas",
            &BTreeSet::new()
        ));
    }

    #[test]
    fn a_used_module_uses_the_peers_it_loads() {
        let used = |modules: &[&'static str], name: &str| {
            let modules = modules.iter().map(|module| (*module).to_owned()).collect();
            companion_is_used(name, |_| false, &modules)
        };
        assert!(used(&["django.db.backends.postgresql"], "psycopg"));
        assert!(used(&["django.db.backends.postgresql"], "psycopg2-binary"));
        assert!(used(&["django.contrib.postgres"], "psycopg"));
        assert!(used(&["storages.backends.s3boto3.S3Boto3Storage"], "boto3"));
        assert!(used(&["lxml.cssselect"], "cssselect"));
        assert!(used(
            &["django.contrib.auth.hashers.Argon2PasswordHasher"],
            "argon2-cffi"
        ));
        // A sibling whose name only starts with the module is another module.
        assert!(!used(&["django.db.backends.postgresqlx"], "psycopg"));
        assert!(!used(&["storages.backends.s3boto3x"], "boto3"));
        // The parent package does not load the backend.
        assert!(!used(&["django.db.backends"], "psycopg"));
        assert!(!used(&["lxml"], "cssselect"));
        assert!(!used(&["django.db.backends.mysql"], "psycopg"));
    }

    #[test]
    fn a_peer_a_module_loads_uses_its_own_peers() {
        let modules = BTreeSet::from(["django.core.cache.backends.redis".to_owned()]);
        assert_eq!(
            used_companions(&["hiredis", "redis", "rich"], |_| false, &modules),
            ["redis", "hiredis"]
        );
    }

    #[test]
    fn a_used_provider_uses_its_new_peers() {
        assert!(companion_is_used(
            "hiredis",
            |name| name == "redis",
            &BTreeSet::new()
        ));
        assert!(companion_is_used(
            "pylsp-mypy",
            |name| name == "python-lsp-server",
            &BTreeSet::new()
        ));
    }

    #[test]
    fn dagster_dev_servers_use_the_webserver() {
        // `dg dev` and `dagster dev` serve the UI from dagster-webserver,
        // which nothing imports or runs by name (#1256).
        for provider in ["dagster-dg-cli", "dagster-dg", "dagster"] {
            assert!(
                companion_is_used(
                    "dagster-webserver",
                    |name| name == provider,
                    &BTreeSet::new()
                ),
                "{provider}"
            );
        }
        assert!(!companion_is_used(
            "dagster-webserver",
            |name| name == "dagster-dbt",
            &BTreeSet::new()
        ));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// The bundled table's providers, its peers and their stub names, and
        /// a name nothing loads: `docs/dev/formal/companions_model.py`'s
        /// declarations.
        fn universe() -> (Vec<&'static str>, Vec<String>) {
            let providers: BTreeSet<&'static str> = RUNTIME_PEERS
                .iter()
                .flat_map(|entry| entry.providers.iter().copied())
                .collect();
            let declared: BTreeSet<String> = RUNTIME_PEERS
                .iter()
                .map(|entry| entry.peer.to_owned())
                .chain(providers.iter().map(|name| format!("types-{name}")))
                .chain(providers.iter().map(|name| format!("{name}-stubs")))
                .chain(["rich".to_owned(), "types-rich".to_owned()])
                .collect();
            (
                providers.into_iter().collect(),
                declared.into_iter().collect(),
            )
        }

        /// Each table module, a name below it, its parent and a sibling that
        /// only shares its prefix.
        fn module_pool() -> Vec<String> {
            let modules: BTreeSet<String> = RUNTIME_PEERS
                .iter()
                .flat_map(|entry| entry.modules.iter())
                .flat_map(|module| {
                    let parent = module
                        .rsplit_once('.')
                        .map_or(*module, |(parent, _)| parent);
                    [
                        (*module).to_owned(),
                        format!("{module}.Attr"),
                        parent.to_owned(),
                        format!("{module}x"),
                    ]
                })
                .collect();
            modules.into_iter().collect()
        }

        /// Whether `modules` has `module` or a name below it at a dot (C4),
        /// written from the rule rather than from `uses_module`.
        fn loads(modules: &BTreeSet<String>, module: &str) -> bool {
            modules.iter().any(|used| {
                used == module
                    || used
                        .strip_prefix(module)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        }

        /// The least set of `declared` closed under the peer rule, by Kleene
        /// iteration from the empty set (C1).
        fn closure(
            declared: &[String],
            used: &BTreeSet<&str>,
            modules: &BTreeSet<String>,
        ) -> BTreeSet<String> {
            let mut found = BTreeSet::new();
            loop {
                let live = |name: &str| used.contains(name) || found.contains(name);
                let step: BTreeSet<String> = declared
                    .iter()
                    .filter(|name| {
                        let stub = name
                            .strip_prefix("types-")
                            .or_else(|| name.strip_suffix("-stubs"))
                            .is_some_and(|base| live(&normalize_distribution_name(base)));
                        stub || RUNTIME_PEERS.iter().any(|entry| {
                            entry.peer == name.as_str()
                                && (entry.providers.iter().any(|provider| live(provider))
                                    || entry.modules.iter().any(|module| loads(modules, module)))
                        })
                    })
                    .cloned()
                    .collect();
                if step == found {
                    return found;
                }
                found = step;
            }
        }

        proptest! {
            /// `used_companions` over the bundled table is the least closed
            /// set (C1), whatever the declaration order (C2), and grows with
            /// the used providers and modules (C3);
            /// `docs/dev/formal/companions_model.py` checks the same on the
            /// seed.
            #[test]
            fn companions_are_the_least_closed_set(
                used_bits in prop::collection::vec(prop::bool::weighted(0.15), 16),
                module_bits in prop::collection::vec(prop::bool::weighted(0.05), 160),
                order in any::<u64>(),
                extra in 0_usize..16,
            ) {
                let (providers, declared) = universe();
                let pool = module_pool();
                prop_assert!(providers.len() <= used_bits.len());
                prop_assert!(pool.len() <= module_bits.len());
                let used: BTreeSet<&str> = providers
                    .iter()
                    .zip(&used_bits)
                    .filter_map(|(name, on)| on.then_some(*name))
                    .collect();
                let modules: BTreeSet<String> = pool
                    .iter()
                    .zip(&module_bits)
                    .filter(|(_, on)| **on)
                    .map(|(module, _)| module.clone())
                    .collect();
                let names: Vec<&str> = declared.iter().map(String::as_str).collect();
                let found = used_companions(&names, |name| used.contains(name), &modules);
                let expected = closure(&declared, &used, &modules);
                let as_set: BTreeSet<String> = found.iter().map(|name| (*name).to_owned()).collect();
                prop_assert_eq!(as_set.len(), found.len());
                prop_assert_eq!(&as_set, &expected);

                let mut shuffled = names.clone();
                let mut seed = order;
                for i in (1..shuffled.len()).rev() {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let j = usize::try_from(seed >> 33).unwrap_or(0) % (i + 1);
                    shuffled.swap(i, j);
                }
                let reordered: BTreeSet<String> =
                    used_companions(&shuffled, |name| used.contains(name), &modules)
                        .into_iter()
                        .map(str::to_owned)
                        .collect();
                prop_assert_eq!(&reordered, &expected);

                let mut more = used.clone();
                more.insert(providers[extra % providers.len()]);
                let grown: BTreeSet<String> =
                    used_companions(&names, |name| more.contains(name), &modules)
                        .into_iter()
                        .map(str::to_owned)
                        .collect();
                prop_assert!(expected.is_subset(&grown));
            }
        }
    }
}
