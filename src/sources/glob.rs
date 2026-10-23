//! Glob set construction and matching.

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::error::SourcesError;

const CACHE_PATTERN: &str = "**/__pycache__/**";
const MANDATORY_EXCLUDES: &[&str] = &[
    ".venv/**",
    ".chokkin/**",
    "build/**",
    "dist/**",
    CACHE_PATTERN,
];

/// Most `{` a pattern may hold. `globset` compiles alternations
/// recursively, so a pattern nesting ten thousand of them overflows the
/// stack and aborts the process (#1145); counting every `{` bounds the
/// nesting whatever the escapes and classes around them.
const MAX_BRACES: usize = 1000;

/// `Err` with the reason when `pattern` holds too many `{` to hand to
/// `globset`; every pattern a project supplies passes through here first.
pub(crate) fn check_braces(pattern: &str) -> Result<(), String> {
    let braces = pattern.matches('{').count();
    if braces > MAX_BRACES {
        return Err(format!(
            "pattern holds {braces} `{{`, more than the {MAX_BRACES} chokkin compiles"
        ));
    }
    Ok(())
}

/// Build a glob matcher from pattern strings.
pub(crate) fn build_glob_set(patterns: &[String]) -> Result<GlobSet, SourcesError> {
    let mut builder = GlobSetBuilder::new();
    let mut compiled = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let invalid = |reason| SourcesError::InvalidGlob {
            pattern: pattern.clone(),
            reason,
        };
        check_braces(pattern).map_err(invalid)?;
        let glob = Glob::new(pattern).map_err(|error| invalid(error.to_string()))?;
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
    fn rejects_invalid_glob() {
        let patterns = vec!["src/[unclosed".to_owned()];
        let error = build_glob_set(&patterns).expect_err("invalid glob");
        assert!(matches!(error, SourcesError::InvalidGlob { .. }));
    }

    #[test]
    fn rejects_deeply_nested_braces_without_overflowing() {
        let deep = format!("{}{}", "{".repeat(10_000), "}".repeat(10_000));
        let error = build_glob_set(&[deep]).expect_err("too deep");
        assert!(
            matches!(&error, SourcesError::InvalidGlob { reason, .. } if reason.contains("10000 `{`")),
            "{error:?}"
        );
    }

    #[test]
    fn check_braces_allows_up_to_the_limit() {
        assert_eq!(check_braces(&"{a}".repeat(MAX_BRACES)), Ok(()));
        check_braces(&"{a}".repeat(MAX_BRACES + 1)).expect_err("one too many");
        assert_eq!(check_braces("*.{py,pyi}"), Ok(()));
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
