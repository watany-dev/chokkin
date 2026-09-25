//! Bundled tables tying pytest options and plugin modules to distributions.

/// pytest command-line options contributed by plugins. A key ending in `-`,
/// or a single-dash short option (`-n4`), matches as a prefix; others match
/// the option name before any `=value`.
const PYTEST_OPTION_DISTRIBUTIONS: &[(&str, &str)] = &[
    ("--cov", "pytest-cov"),
    ("--cov-", "pytest-cov"),
    ("--no-cov", "pytest-cov"),
    ("-n", "pytest-xdist"),
    ("--numprocesses", "pytest-xdist"),
    ("--dist", "pytest-xdist"),
    ("--maxprocesses", "pytest-xdist"),
    ("--benchmark-", "pytest-benchmark"),
    ("--ds", "pytest-django"),
    ("--dc", "pytest-django"),
    ("--reuse-db", "pytest-django"),
    ("--create-db", "pytest-django"),
    ("--nomigrations", "pytest-django"),
    ("--no-migrations", "pytest-django"),
    ("--timeout", "pytest-timeout"),
    ("--reruns", "pytest-rerunfailures"),
    ("--asyncio-mode", "pytest-asyncio"),
    ("--html", "pytest-html"),
    ("--randomly-", "pytest-randomly"),
    ("--mypy", "pytest-mypy"),
    ("--hypothesis-", "hypothesis"),
    ("--snapshot-update", "syrupy"),
    ("--testmon", "pytest-testmon"),
    ("--instafail", "pytest-instafail"),
    ("--mpl", "pytest-mpl"),
    ("--disable-socket", "pytest-socket"),
    ("--block-network", "pytest-recording"),
    ("--record-mode", "pytest-recording"),
];

/// Plugin modules (pytest `-p`, mypy `plugins`) whose import root the import
/// map cannot tie to their distribution by name.
const PLUGIN_MODULE_DISTRIBUTIONS: &[(&str, &str)] = &[
    ("xdist", "pytest-xdist"),
    ("mypy_django_plugin", "django-stubs"),
    ("mypy_drf_plugin", "djangorestframework-stubs"),
    ("sqlmypy", "sqlalchemy-stubs"),
];

pub(super) fn pytest_option_distribution(word: &str) -> Option<&'static str> {
    let option = word.split_once('=').map_or(word, |(name, _)| name);
    PYTEST_OPTION_DISTRIBUTIONS
        .iter()
        .find(|(key, _)| option_matches(option, key))
        .map(|(_, distribution)| *distribution)
}

fn option_matches(option: &str, key: &str) -> bool {
    let prefix = key.ends_with('-') || !key.starts_with("--");
    if prefix {
        option.starts_with(key)
    } else {
        option == key
    }
}

pub(super) fn plugin_module_distribution(module: &str) -> Option<&'static str> {
    let root = module.split('.').next().unwrap_or(module);
    PLUGIN_MODULE_DISTRIBUTIONS
        .iter()
        .find(|(name, _)| *name == root)
        .map(|(_, distribution)| *distribution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pytest_options_by_name_or_prefix() {
        assert_eq!(pytest_option_distribution("--cov=src"), Some("pytest-cov"));
        assert_eq!(
            pytest_option_distribution("--cov-report=xml"),
            Some("pytest-cov")
        );
        assert_eq!(pytest_option_distribution("-n"), Some("pytest-xdist"));
        assert_eq!(pytest_option_distribution("-n4"), Some("pytest-xdist"));
        assert_eq!(
            pytest_option_distribution("--benchmark-skip"),
            Some("pytest-benchmark")
        );
        assert_eq!(pytest_option_distribution("--covx"), None);
        assert_eq!(pytest_option_distribution("-q"), None);
        assert_eq!(pytest_option_distribution("--strict-markers"), None);
    }

    #[test]
    fn maps_plugin_module_roots() {
        assert_eq!(
            plugin_module_distribution("xdist.plugin"),
            Some("pytest-xdist")
        );
        assert_eq!(
            plugin_module_distribution("mypy_django_plugin.main"),
            Some("django-stubs")
        );
        assert_eq!(plugin_module_distribution("pydantic.mypy"), None);
    }
}
