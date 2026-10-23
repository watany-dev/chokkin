//! Glob set construction and matching.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use super::error::SourcesError;

const CACHE_PATTERN: &str = "**/__pycache__/**";
const MANDATORY_EXCLUDES: &[&str] = &[
    ".venv/**",
    ".chokkin/**",
    "build/**",
    "dist/**",
    CACHE_PATTERN,
];

/// Compile a path glob written in config so it means the same on every OS.
///
/// globset reads `\` as a separator on Windows and as an escape elsewhere, so
/// `legacy\**` would match only on Windows. `\` is always a separator here,
/// as in workspace member paths; a literal metacharacter is spelled `[*]`.
pub(crate) fn path_glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(&pattern.replace('\\', "/"))
        .backslash_escape(false)
        .build()
}

/// Build a glob matcher from pattern strings.
pub(crate) fn build_glob_set(patterns: &[String]) -> Result<GlobSet, SourcesError> {
    let mut builder = GlobSetBuilder::new();
    let mut compiled = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let glob = path_glob(pattern).map_err(|error| SourcesError::InvalidGlob {
            pattern: pattern.clone(),
            reason: error.to_string(),
        })?;
        builder.add(glob);
        compiled.push(pattern.clone());
    }
    builder.build().map_err(|error| SourcesError::InvalidGlob {
        pattern: compiled.join(", "),
        reason: error.to_string(),
    })
}

/// Merge config excludes with mandatory safety patterns.
///
/// `.venv`, `.chokkin`, `build`, `dist`, and `__pycache__` are always excluded even when
/// the user replaces `config.exclude`.
#[must_use]
pub(super) fn effective_exclude(config_exclude: &[String]) -> Vec<String> {
    let mut patterns = config_exclude.to_vec();
    for mandatory in MANDATORY_EXCLUDES {
        if !patterns.iter().any(|pattern| pattern == *mandatory) {
            patterns.push((*mandatory).to_owned());
        }
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
    fn backslash_is_a_separator_on_every_os() {
        let patterns = vec!["legacy\\**".to_owned(), "src\\pkg\\*.py".to_owned()];
        let set = build_glob_set(&patterns).expect("valid glob");
        assert!(set.is_match("legacy/old.py"));
        assert!(set.is_match("src/pkg/mod.py"));
        assert!(!set.is_match("legacy*"));
    }

    #[test]
    fn rejects_invalid_glob() {
        let patterns = vec!["src/[unclosed".to_owned()];
        let error = build_glob_set(&patterns).expect_err("invalid glob");
        assert!(matches!(error, SourcesError::InvalidGlob { .. }));
    }

    #[test]
    fn effective_exclude_adds_mandatory_patterns() {
        let patterns = effective_exclude(&[]);
        assert!(patterns.contains(&".venv/**".to_owned()));
        assert!(patterns.contains(&"build/**".to_owned()));
        assert!(patterns.contains(&"dist/**".to_owned()));
        assert!(patterns.contains(&"**/__pycache__/**".to_owned()));
    }

    #[test]
    fn effective_exclude_keeps_mandatory_when_config_replaced() {
        let patterns = effective_exclude(&["custom/**".to_owned()]);
        assert!(patterns.contains(&"custom/**".to_owned()));
        assert!(patterns.contains(&".venv/**".to_owned()));
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
            fn effective_exclude_keeps_input_and_adds_mandatory_once(
                patterns in prop::collection::vec("[a-z0-9_/*.]{0,20}", 0..6),
            ) {
                let effective = effective_exclude(&patterns);
                prop_assert!(effective.starts_with(&patterns));
                for mandatory in MANDATORY_EXCLUDES {
                    let input_count = patterns.iter().filter(|p| *p == mandatory).count();
                    prop_assert_eq!(
                        effective.iter().filter(|p| *p == mandatory).count(),
                        input_count.max(1)
                    );
                }
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
