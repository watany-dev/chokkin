//! Import root → distribution candidate maps.

use std::collections::BTreeMap;

use crate::config::ChokkinConfig;
use crate::manifest::normalize_distribution_name;

use super::bundled::package_modules::PACKAGE_TO_IMPORTS;
use super::types::ResolveConfidence;

/// Lookup source for a distribution candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapSource {
    /// Bundled package-module map.
    Bundled,
    /// User `[tool.chokkin].package_module_map`.
    User,
    /// Canonicalized name fallback.
    Canonicalize,
}

/// Candidate distribution for an import root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionCandidate {
    /// Normalized distribution name.
    pub distribution: String,
    /// Lookup source.
    pub source: MapSource,
    /// Resolution confidence.
    pub confidence: ResolveConfidence,
}

/// Reverse index from import root to distribution candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMap {
    bundled: BTreeMap<String, Vec<String>>,
    user: BTreeMap<String, Vec<String>>,
}

impl ImportMap {
    /// Build merged import map from bundled data and user config.
    #[must_use]
    pub fn build(config: &ChokkinConfig) -> Self {
        let bundled = build_reverse_map(
            PACKAGE_TO_IMPORTS
                .iter()
                .map(|(distribution, imports)| (*distribution, imports.iter().copied())),
        );
        let user = build_reverse_map(config.package_module_map.iter().map(
            |(distribution, imports)| (distribution.as_str(), imports.iter().map(String::as_str)),
        ));

        Self { bundled, user }
    }

    /// Look up distribution candidates for `import_root`.
    #[must_use]
    pub fn candidates(&self, import_root: &str) -> Vec<DistributionCandidate> {
        if let Some(user) = self.user.get(import_root) {
            return user
                .iter()
                .map(|distribution| DistributionCandidate {
                    distribution: distribution.clone(),
                    source: MapSource::User,
                    confidence: ResolveConfidence::Likely,
                })
                .collect();
        }

        if let Some(bundled) = self.bundled.get(import_root) {
            let confidence = if bundled.len() == 1 {
                ResolveConfidence::Certain
            } else {
                ResolveConfidence::Maybe
            };
            return bundled
                .iter()
                .map(|distribution| DistributionCandidate {
                    distribution: distribution.clone(),
                    source: MapSource::Bundled,
                    confidence,
                })
                .collect();
        }

        let canonical = canonicalize_match(import_root);
        if let Some(distribution) = canonical {
            return vec![DistributionCandidate {
                distribution,
                source: MapSource::Canonicalize,
                confidence: ResolveConfidence::Maybe,
            }];
        }

        Vec::new()
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
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (binary, distribution) in super::bundled::binaries::BINARY_TO_DISTRIBUTION {
        map.insert(
            binary.to_string(),
            normalize_distribution_name(distribution),
        );
    }
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
    fn resolves_pyyaml_from_bundled_map() {
        let import_map = ImportMap::build(&default_config());
        let candidates = import_map.candidates("yaml");
        assert!(candidates.iter().any(|c| c.distribution == "pyyaml"));
    }

    #[test]
    fn user_map_overrides_bundled() {
        let mut config = default_config();
        config
            .package_module_map
            .insert("PyYAML".to_owned(), vec!["yaml".to_owned()]);
        let import_map = ImportMap::build(&config);
        let candidates = import_map.candidates("yaml");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, MapSource::User);
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
                candidates.iter().any(|c| c.distribution == distribution),
                "expected {import_root} -> {distribution}, got {candidates:?}"
            );
        }
    }

    #[test]
    fn canonicalize_matches_mixed_case_import_root() {
        let import_map = ImportMap::build(&default_config());
        let candidates = import_map.candidates("DefinitelyNotInBundledMap");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].distribution, "definitelynotinbundledmap");
        assert_eq!(candidates[0].source, MapSource::Canonicalize);
        assert_eq!(candidates[0].confidence, ResolveConfidence::Maybe);
    }
}
