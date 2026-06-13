//! Glob set construction and matching.

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::error::SourcesError;

const CACHE_PATTERN: &str = "**/__pycache__/**";

/// Build a glob matcher from pattern strings.
pub fn build_glob_set(patterns: &[String]) -> Result<GlobSet, SourcesError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|error| SourcesError::InvalidGlob {
            pattern: pattern.clone(),
            reason: error.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| SourcesError::InvalidGlob {
        pattern: String::new(),
        reason: error.to_string(),
    })
}

/// Merge config excludes with mandatory cache-directory exclusion.
#[must_use]
pub fn effective_exclude(config_exclude: &[String]) -> Vec<String> {
    let mut patterns = config_exclude.to_vec();
    if !patterns.iter().any(|pattern| pattern == CACHE_PATTERN) {
        patterns.push(CACHE_PATTERN.to_owned());
    }
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_glob_set() {
        let patterns = vec!["src/**/*.py".to_owned()];
        let set = build_glob_set(&patterns).expect("valid glob");
        assert!(set.is_match("src/pkg/module.py"));
    }

    #[test]
    fn rejects_invalid_glob() {
        let patterns = vec!["src/[unclosed".to_owned()];
        let error = build_glob_set(&patterns).expect_err("invalid glob");
        assert!(matches!(error, SourcesError::InvalidGlob { .. }));
    }

    #[test]
    fn effective_exclude_adds_pycache_pattern() {
        let patterns = effective_exclude(&[]);
        assert!(patterns.contains(&"**/__pycache__/**".to_owned()));
    }
}
