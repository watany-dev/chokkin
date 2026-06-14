//! Import root → distribution candidate maps.

use std::collections::BTreeMap;

use crate::config::YokeiConfig;
use crate::manifest::normalize_distribution_name;

use super::bundled::package_modules::PACKAGE_TO_IMPORTS;
use super::types::ResolveConfidence;

/// Lookup source for a distribution candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapSource {
    /// Bundled package-module map.
    Bundled,
    /// User `[tool.yokei].package_module_map`.
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
    pub fn build(config: &YokeiConfig) -> Self {
        let mut bundled: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (distribution, imports) in PACKAGE_TO_IMPORTS {
            let dist = normalize_distribution_name(distribution);
            for import in *imports {
                bundled
                    .entry(import.to_string())
                    .or_default()
                    .push(dist.clone());
            }
        }
        for imports in bundled.values_mut() {
            let entries: &mut Vec<String> = imports;
            entries.sort();
            entries.dedup();
        }

        let mut user: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (distribution, imports) in &config.package_module_map {
            let dist = normalize_distribution_name(distribution);
            for import in imports {
                user.entry(import.clone()).or_default().push(dist.clone());
            }
        }
        for imports in user.values_mut() {
            let entries: &mut Vec<String> = imports;
            entries.sort();
            entries.dedup();
        }

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

fn canonicalize_match(import_root: &str) -> Option<String> {
    let guess = normalize_distribution_name(import_root);
    if guess.replace('-', "_") == import_root {
        return Some(guess);
    }
    None
}

/// Build merged binary name → distribution map.
#[must_use]
pub fn build_binary_map(
    config: &YokeiConfig,
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
}
