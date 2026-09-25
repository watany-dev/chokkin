//! Import root → distribution candidate maps.

use std::collections::BTreeMap;
use std::sync::LazyLock;

static BUNDLED_IMPORTS: LazyLock<BTreeMap<String, Vec<String>>> = LazyLock::new(|| {
    build_reverse_map(
        PACKAGE_TO_IMPORTS
            .iter()
            .map(|(distribution, imports)| (*distribution, imports.iter().copied())),
    )
});

static BUNDLED_BINARIES: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    super::bundled::binaries::BINARY_TO_DISTRIBUTION
        .iter()
        .map(|(binary, distribution)| {
            (
                (*binary).to_owned(),
                normalize_distribution_name(distribution),
            )
        })
        .collect()
});

use crate::config::ChokkinConfig;
use crate::manifest::normalize_distribution_name;

use super::bundled::package_modules::PACKAGE_TO_IMPORTS;
use super::types::ResolveConfidence;

/// Reverse index from import root to distribution candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMap {
    bundled: &'static BTreeMap<String, Vec<String>>,
    user: BTreeMap<String, Vec<String>>,
    local: BTreeMap<String, Vec<String>>,
}

impl ImportMap {
    /// Build merged import map from bundled data and user config.
    #[must_use]
    pub fn build(config: &ChokkinConfig) -> Self {
        let bundled = &*BUNDLED_IMPORTS;
        let user = build_reverse_map(config.package_module_map.iter().map(
            |(distribution, imports)| (distribution.as_str(), imports.iter().map(String::as_str)),
        ));

        Self {
            bundled,
            user,
            local: BTreeMap::new(),
        }
    }

    /// Add import roots served by local path sources; they win over the user
    /// and bundled maps because the manifest names the distribution explicitly.
    #[must_use]
    pub fn with_local_sources(mut self, local: BTreeMap<String, Vec<String>>) -> Self {
        self.local = local;
        self
    }

    /// Look up distribution candidates for `import_root`.
    #[must_use]
    pub fn candidates(&self, import_root: &str) -> Option<(Vec<String>, ResolveConfidence)> {
        if let Some(local) = self.local.get(import_root) {
            return Some((local.clone(), ResolveConfidence::Certain));
        }

        if let Some(user) = self.user.get(import_root) {
            return Some((user.clone(), ResolveConfidence::Likely));
        }

        if let Some(bundled) = self.bundled.get(import_root) {
            let confidence = if bundled.len() == 1 {
                ResolveConfidence::Certain
            } else {
                ResolveConfidence::Maybe
            };
            return Some((bundled.clone(), confidence));
        }

        canonicalize_match(import_root)
            .map(|distribution| (vec![distribution], ResolveConfidence::Maybe))
    }
}

fn build_reverse_map<'a>(
    entries: impl IntoIterator<Item = (&'a str, impl IntoIterator<Item = &'a str>)>,
) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (distribution, imports) in entries {
        let dist = normalize_distribution_name(distribution);
        for import in imports {
            map.entry(import.to_owned()).or_default().push(dist.clone());
        }
    }
    sort_dedup_map_values(&mut map);
    map
}

fn sort_dedup_map_values(map: &mut BTreeMap<String, Vec<String>>) {
    for values in map.values_mut() {
        values.sort();
        values.dedup();
    }
}

fn canonicalize_match(import_root: &str) -> Option<String> {
    let normalized = normalize_distribution_name(import_root);
    if normalized.is_empty() || normalized == import_root {
        None
    } else {
        Some(normalized)
    }
}

/// Build merged binary name → distribution map.
#[must_use]
pub fn build_binary_map(
    config: &ChokkinConfig,
    venv: &super::venv::VenvIndex,
) -> BTreeMap<String, String> {
    let mut map = BUNDLED_BINARIES.clone();
    for (binary, distribution) in &config.binary_map {
        map.insert(binary.clone(), normalize_distribution_name(distribution));
    }
    for (binary, distribution) in &venv.binaries {
        map.insert(binary.clone(), distribution.clone());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;

    #[test]
    fn overlays_do_not_mutate_bundled_defaults() {
        let defaults = default_config();
        let mut custom = defaults.clone();
        custom
            .package_module_map
            .insert("custom-yaml".to_owned(), vec!["yaml".to_owned()]);
        custom
            .binary_map
            .insert("pytest".to_owned(), "custom-test".to_owned());
        let mut venv = super::super::venv::VenvIndex::default();
        venv.binaries
            .insert("pytest".to_owned(), "venv-test".to_owned());
        assert_eq!(
            ImportMap::build(&custom).candidates("yaml").map(|(d, _)| d),
            Some(vec!["custom-yaml".to_owned()])
        );
        assert_eq!(
            ImportMap::build(&defaults)
                .candidates("yaml")
                .map(|(d, _)| d),
            Some(vec!["pyyaml".to_owned()])
        );
        assert_eq!(build_binary_map(&custom, &venv)["pytest"], "venv-test");
        assert_eq!(
            build_binary_map(&custom, &super::super::venv::VenvIndex::default())["pytest"],
            "custom-test"
        );
        assert_eq!(
            build_binary_map(&defaults, &super::super::venv::VenvIndex::default())["pytest"],
            "pytest"
        );
    }

    #[test]
    fn resolves_pyyaml_from_bundled_map() {
        let import_map = ImportMap::build(&default_config());
        assert!(
            import_map
                .candidates("yaml")
                .is_some_and(|(c, _)| c.iter().any(|d| d == "pyyaml"))
        );
    }

    #[test]
    fn user_map_overrides_bundled() {
        let mut config = default_config();
        config
            .package_module_map
            .insert("PyYAML".to_owned(), vec!["yaml".to_owned()]);
        let import_map = ImportMap::build(&config);
        assert_eq!(
            import_map.candidates("yaml"),
            Some((vec!["pyyaml".to_owned()], ResolveConfidence::Likely))
        );
    }

    #[test]
    fn resolves_import_name_aliases_from_bundled_map() {
        let import_map = ImportMap::build(&default_config());
        for (import_root, distribution) in [
            ("multipart", "python-multipart"),
            ("OpenSSL", "pyopenssl"),
            ("socks", "pysocks"),
            ("argon2", "argon2-cffi"),
        ] {
            let candidates = import_map.candidates(import_root);
            assert!(
                candidates
                    .as_ref()
                    .is_some_and(|(c, _)| c.iter().any(|d| d == distribution)),
                "expected {import_root} -> {distribution}, got {candidates:?}"
            );
        }
    }

    #[test]
    fn local_sources_win_over_bundled_map() {
        let local = BTreeMap::from([("yaml".to_owned(), vec!["my-yaml".to_owned()])]);
        let import_map = ImportMap::build(&default_config()).with_local_sources(local);
        assert_eq!(
            import_map.candidates("yaml"),
            Some((vec!["my-yaml".to_owned()], ResolveConfidence::Certain))
        );
    }

    #[test]
    fn canonicalize_matches_mixed_case_import_root() {
        let import_map = ImportMap::build(&default_config());
        assert_eq!(
            import_map.candidates("DefinitelyNotInBundledMap"),
            Some((
                vec!["definitelynotinbundledmap".to_owned()],
                ResolveConfidence::Maybe
            ))
        );
    }
}
