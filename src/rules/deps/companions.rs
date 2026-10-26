//! Dependencies consumed by an imported library rather than by its caller.

use std::collections::BTreeSet;

use crate::manifest::normalize_distribution_name;
use crate::resolver::RUNTIME_PEERS;

/// The `declared` distributions used as a companion of a used one, shared by
/// project and PEP 723 scopes. A peer can be a provider itself (a cache
/// backend's `redis` provides `hiredis`), so it repeats until none is added.
pub(super) fn used_companions<'a>(
    declared: &[&'a str],
    used: impl Fn(&str) -> bool,
    modules: &BTreeSet<&str>,
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
fn companion_is_used(name: &str, used: impl Fn(&str) -> bool, modules: &BTreeSet<&str>) -> bool {
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
fn uses_module(modules: &BTreeSet<&str>, module: &str) -> bool {
    modules
        .range(module..)
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
            companion_is_used(name, |_| false, &modules.iter().copied().collect())
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
        let modules = BTreeSet::from(["django.core.cache.backends.redis"]);
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
}
