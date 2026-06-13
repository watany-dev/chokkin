//! PEP 508 parsing helpers.

use std::str::FromStr;

use pep508_rs::{Requirement, VerbatimUrl};

use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Normalize a distribution name to lowercase hyphen form (PEP 503):
/// runs of `-`, `_`, and `.` collapse into a single `-`.
#[must_use]
pub fn normalize_distribution_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut pending_separator = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            pending_separator = true;
            continue;
        }
        if pending_separator {
            normalized.push('-');
            pending_separator = false;
        }
        normalized.push(ch.to_ascii_lowercase());
    }
    if pending_separator {
        normalized.push('-');
    }
    normalized
}

/// Extract a distribution name from a URL fragment `#egg=name`.
#[must_use]
pub fn extract_egg_name(spec: &str) -> Option<String> {
    let fragment = spec.split('#').nth(1)?;
    for part in fragment.split('&') {
        if let Some(egg) = part.strip_prefix("egg=") {
            let trimmed = egg.trim();
            if !trimmed.is_empty() {
                return Some(normalize_distribution_name(trimmed));
            }
        }
    }
    None
}

/// Parse a PEP 508 requirement string into a declared dependency.
pub fn parse_requirement(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> Result<DeclaredDependency, ManifestWarning> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ManifestWarning::InvalidRequirementLine {
            file: origin.file.clone(),
            line: origin.line.unwrap_or(0),
            raw: raw.to_owned(),
        });
    }

    // pep508_rs panics on inputs whose lenient name token fails strict
    // PEP 508 validation (e.g. `pkg_[extra]`), so gate the call ourselves.
    if is_strict_pep508_name(leading_name_token(trimmed))
        && let Ok(requirement) = Requirement::<VerbatimUrl>::from_str(trimmed)
    {
        return Ok(requirement_to_declared(&requirement, context, origin));
    }

    if let Some(name) = extract_egg_name(trimmed) {
        return Ok(DeclaredDependency {
            name,
            extras: Vec::new(),
            marker: None,
            specifier: Some(trimmed.to_owned()),
            context,
            origin,
            opaque: false,
        });
    }

    if is_url_like(trimmed) {
        return Ok(DeclaredDependency {
            name: String::new(),
            extras: Vec::new(),
            marker: None,
            specifier: Some(trimmed.to_owned()),
            context,
            origin,
            opaque: true,
        });
    }

    Err(ManifestWarning::InvalidRequirementLine {
        file: origin.file.clone(),
        line: origin.line.unwrap_or(0),
        raw: raw.to_owned(),
    })
}

fn requirement_to_declared(
    requirement: &Requirement<VerbatimUrl>,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> DeclaredDependency {
    let name = normalize_distribution_name(requirement.name.as_ref());
    let opaque = name.is_empty();

    let extras = requirement
        .extras
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    let marker = requirement
        .marker
        .contents()
        .map(|contents| contents.to_string());

    let specifier = requirement
        .version_or_url
        .as_ref()
        .map(std::string::ToString::to_string);

    DeclaredDependency {
        name,
        extras,
        marker,
        specifier,
        context,
        origin,
        opaque,
    }
}

/// Leading run of PEP 508 name characters (`[A-Za-z0-9._-]`).
#[must_use]
fn leading_name_token(spec: &str) -> &str {
    let end = spec
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(spec.len());
    &spec[..end]
}

/// Strict PEP 508 name check: alphanumeric edges (separators inside only).
#[must_use]
fn is_strict_pep508_name(name: &str) -> bool {
    match name.as_bytes() {
        [] => false,
        [single] => single.is_ascii_alphanumeric(),
        [first, .., last] => first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric(),
    }
}

#[must_use]
fn is_url_like(spec: &str) -> bool {
    spec.contains("://")
        || spec.starts_with("git+")
        || spec.starts_with("hg+")
        || spec.starts_with("bzr+")
        || spec.starts_with("svn+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_distribution_name() {
        assert_eq!(normalize_distribution_name("PyYAML"), "pyyaml");
        assert_eq!(normalize_distribution_name("scikit_learn"), "scikit-learn");
    }

    #[test]
    fn normalizes_pep503_separator_runs() {
        // PEP 503: runs of `-`, `_`, `.` collapse into a single `-`.
        assert_eq!(
            normalize_distribution_name("zope.interface"),
            "zope-interface"
        );
        assert_eq!(normalize_distribution_name("0--0"), "0-0");
        assert_eq!(normalize_distribution_name("a._-b"), "a-b");
    }

    #[test]
    fn rejects_invalid_name_token_without_panicking() {
        // Regression: pep508_rs panics internally on `<name>_[` inputs.
        for raw in ["0_[", "pkg_[extra]", "x-[dev]", "a.[b]"] {
            let result = parse_requirement(
                raw,
                DependencyContext::Runtime,
                DependencyOrigin {
                    file: "requirements.txt".to_owned(),
                    line: Some(1),
                    label: "requirements.txt".to_owned(),
                },
            );
            assert!(result.is_err(), "raw={raw:?} must be rejected");
        }
    }

    #[test]
    fn parses_simple_requirement() {
        let dep = parse_requirement(
            "requests>=2.0",
            DependencyContext::Runtime,
            DependencyOrigin {
                file: "requirements.txt".to_owned(),
                line: Some(1),
                label: "requirements.txt".to_owned(),
            },
        )
        .expect("parse requirement");

        assert_eq!(dep.name, "requests");
        assert!(!dep.opaque);
    }

    #[test]
    fn extracts_egg_name_from_vcs_url() {
        let dep = parse_requirement(
            "git+https://github.com/example/repo.git#egg=My-Package",
            DependencyContext::Runtime,
            DependencyOrigin {
                file: "requirements.txt".to_owned(),
                line: Some(1),
                label: "requirements.txt".to_owned(),
            },
        )
        .expect("parse requirement");

        assert_eq!(dep.name, "my-package");
        assert!(!dep.opaque);
    }

    #[test]
    fn preserves_url_fragment_in_direct_url() {
        let dep = parse_requirement(
            "pkg @ https://host/p.zip#sha256=deadbeef",
            DependencyContext::Runtime,
            DependencyOrigin {
                file: "requirements.txt".to_owned(),
                line: Some(1),
                label: "requirements.txt".to_owned(),
            },
        )
        .expect("parse requirement");

        assert_eq!(dep.name, "pkg");
        assert!(
            dep.specifier
                .as_deref()
                .is_some_and(|spec| spec.contains("#sha256=deadbeef"))
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn origin() -> DependencyOrigin {
            DependencyOrigin {
                file: "requirements.txt".to_owned(),
                line: Some(1),
                label: "requirements.txt".to_owned(),
            }
        }

        /// Valid PEP 508 distribution names: alnum edges, `._-` separators inside.
        fn valid_name() -> impl Strategy<Value = String> {
            "[A-Za-z0-9]([A-Za-z0-9._-]{0,30}[A-Za-z0-9])?"
        }

        proptest! {
            #[test]
            fn normalize_is_idempotent(name in "\\PC{0,64}") {
                let once = normalize_distribution_name(&name);
                let twice = normalize_distribution_name(&once);
                prop_assert_eq!(once, twice);
            }

            #[test]
            fn normalize_removes_underscores_and_ascii_uppercase(name in "\\PC{0,64}") {
                let normalized = normalize_distribution_name(&name);
                prop_assert!(!normalized.contains('_'));
                prop_assert!(!normalized.chars().any(|ch| ch.is_ascii_uppercase()));
            }

            #[test]
            fn extract_egg_name_never_panics_and_is_normalized(spec in "\\PC{0,128}") {
                if let Some(egg) = extract_egg_name(&spec) {
                    prop_assert!(!egg.is_empty());
                    prop_assert_eq!(normalize_distribution_name(&egg), egg.as_str());
                }
            }

            #[test]
            fn extract_egg_name_finds_appended_fragment(name in valid_name()) {
                let spec = format!("git+https://host/repo.git#egg={name}");
                prop_assert_eq!(
                    extract_egg_name(&spec),
                    Some(normalize_distribution_name(&name))
                );
            }

            #[test]
            fn parse_requirement_never_panics(raw in "\\PC{0,200}") {
                let _ = parse_requirement(&raw, DependencyContext::Runtime, origin());
            }

            #[test]
            fn parse_requirement_name_is_normalized_and_opaque_iff_empty(raw in "\\PC{0,200}") {
                if let Ok(dep) = parse_requirement(&raw, DependencyContext::Runtime, origin()) {
                    prop_assert_eq!(
                        normalize_distribution_name(&dep.name),
                        dep.name.as_str()
                    );
                    prop_assert_eq!(dep.name.is_empty(), dep.opaque);
                }
            }

            #[test]
            fn parse_requirement_roundtrips_valid_specs(
                name in valid_name(),
                major in 0u32..100,
                minor in 0u32..100,
            ) {
                let raw = format!("{name}>={major}.{minor}");
                let dep = parse_requirement(&raw, DependencyContext::Runtime, origin())
                    .expect("valid requirement must parse");
                prop_assert_eq!(dep.name, normalize_distribution_name(&name));
                prop_assert!(!dep.opaque);
                prop_assert_eq!(dep.specifier, Some(format!(">={major}.{minor}")));
            }

            #[test]
            fn parse_requirement_preserves_extras(
                name in valid_name(),
                extra in "[a-z][a-z0-9]{0,10}",
            ) {
                let raw = format!("{name}[{extra}]");
                let dep = parse_requirement(&raw, DependencyContext::Runtime, origin())
                    .expect("requirement with extra must parse");
                prop_assert_eq!(dep.extras, vec![extra]);
            }
        }
    }
}
