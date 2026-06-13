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

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn build_glob_set_never_panics(patterns in prop::collection::vec("\\PC{0,40}", 0..6)) {
                let _ = build_glob_set(&patterns);
            }

            #[test]
            fn build_glob_set_accepts_safe_patterns(
                patterns in prop::collection::vec("[a-z0-9_/*.-]{1,30}", 0..6),
            ) {
                // Patterns without meta-character openers ([, {, escape) always build.
                prop_assert!(build_glob_set(&patterns).is_ok());
            }

            #[test]
            fn effective_exclude_keeps_input_and_adds_cache_once(
                patterns in prop::collection::vec("[a-z0-9_/*.]{0,20}", 0..6),
            ) {
                let effective = effective_exclude(&patterns);
                prop_assert!(effective.starts_with(&patterns));
                let input_count = patterns.iter().filter(|p| *p == CACHE_PATTERN).count();
                prop_assert_eq!(
                    effective.iter().filter(|p| *p == CACHE_PATTERN).count(),
                    input_count.max(1)
                );
            }

            #[test]
            fn effective_exclude_is_idempotent(
                patterns in prop::collection::vec("[a-z0-9_/*.]{0,20}", 0..6),
            ) {
                let once = effective_exclude(&patterns);
                prop_assert_eq!(effective_exclude(&once), once);
            }
        }
    }
}
