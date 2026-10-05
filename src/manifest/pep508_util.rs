//! PEP 508 parsing helpers.

use super::pep508::{Requirement, looks_like_archive};
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

/// Parse a requirements-file line into a declared dependency, the way pip reads it.
pub fn parse_requirement(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> Result<DeclaredDependency, ManifestWarning> {
    parse_with(raw, context, origin, parse_pip_requirement)
}

/// Parse a requirement from a PEP 508 manifest field (`pyproject.toml`,
/// `setup.cfg`, `setup.py`).
///
/// Unlike [`parse_requirement`], a name ending in an archive extension
/// (`foo.tlz`, `foo.whl[x]`) is a distribution name here, as in `packaging`.
pub fn parse_pep508_requirement(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> Result<DeclaredDependency, ManifestWarning> {
    parse_with(raw, context, origin, super::pep508::parse_requirement)
}

fn parse_with(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
    parse: fn(&str) -> Option<Requirement>,
) -> Result<DeclaredDependency, ManifestWarning> {
    let trimmed = raw.trim();
    let (name, extras, marker, specifier, opaque) = if let Some(requirement) = parse(trimmed) {
        (
            normalize_distribution_name(&requirement.name),
            requirement.extras,
            requirement.marker,
            requirement.version_or_url,
            false,
        )
    } else if is_direct_reference(trimmed) {
        // `name @ url` that failed the grammar (e.g. a space in the URL) is
        // invalid, not an opaque URL line.
        return Err(invalid(raw, origin));
    } else if let Some(name) = extract_egg_name(trimmed) {
        (name, Vec::new(), None, Some(trimmed.to_owned()), false)
    } else if is_url_like(trimmed) {
        (
            String::new(),
            Vec::new(),
            None,
            Some(trimmed.to_owned()),
            true,
        )
    } else {
        return Err(invalid(raw, origin));
    };
    Ok(DeclaredDependency {
        name,
        extras,
        marker,
        specifier,
        context,
        origin,
        opaque,
        included_via: Vec::new(),
    })
}

fn invalid(raw: &str, origin: DependencyOrigin) -> ManifestWarning {
    ManifestWarning::InvalidRequirementLine {
        file: origin.file,
        line: origin.line,
        label: origin.label,
        raw: raw.to_owned(),
    }
}

fn egg_name_fallback(trimmed: &str) -> Option<String> {
    if is_direct_reference(trimmed) {
        return None;
    }
    extract_egg_name(trimmed)
}

/// `name [extras] @ ...`, the PEP 508 direct-reference form.
fn is_direct_reference(trimmed: &str) -> bool {
    trimmed.split_once('@').is_some_and(|(head, _)| {
        let name = head.split('[').next().unwrap_or(head).trim();
        leading_name_token(name) == name && is_strict_pep508_name(name)
    })
}

/// pip reads a bare `name.whl` / `name.tar.gz` (no version or URL) as a file.
fn parse_pip_requirement(trimmed: &str) -> Option<Requirement> {
    super::pep508::parse_requirement(trimmed).filter(|requirement| {
        requirement.version_or_url.is_some() || !looks_like_archive(&requirement.name)
    })
}

/// Distribution name of a manifest-field requirement, read the way
/// [`parse_pep508_requirement`] reads it; `None` for opaque or invalid entries.
#[must_use]
pub fn pep508_distribution_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    super::pep508::parse_requirement(trimmed)
        .map(|requirement| normalize_distribution_name(&requirement.name))
        .or_else(|| egg_name_fallback(trimmed))
}

/// Distribution name of a requirements-file line, read the way
/// [`parse_requirement`] reads it.
#[must_use]
pub(super) fn requirement_name(trimmed: &str) -> Option<String> {
    parse_pip_requirement(trimmed)
        .map(|requirement| normalize_distribution_name(&requirement.name))
        .or_else(|| egg_name_fallback(trimmed))
}

/// Leading run of PEP 508 name characters (`[A-Za-z0-9._-]`).
#[must_use]
pub(super) fn leading_name_token(spec: &str) -> &str {
    let end = spec
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(spec.len());
    &spec[..end]
}

/// Strict PEP 508 name check: alphanumeric edges (separators inside only).
#[must_use]
pub(super) fn is_strict_pep508_name(name: &str) -> bool {
    match name.as_bytes() {
        [] => false,
        [single] => single.is_ascii_alphanumeric(),
        [first, .., last] => first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric(),
    }
}

#[must_use]
pub(super) fn is_url_like(spec: &str) -> bool {
    spec.contains("://")
        || ["git+", "hg+", "bzr+", "svn+"].iter().any(|prefix| {
            spec.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_like_matches_each_scheme_form() {
        for (spec, expected) in [
            ("https://host/p.zip", true),
            ("file:///abs/p", true),
            ("git+ssh://git@host/r.git", true),
            ("git+git@host:r.git", true),
            ("hg+http", true),
            ("bzr+lp:project", true),
            ("svn+svn", true),
            ("requests", false),
            ("./pkg", false),
            ("pkg+git", false),
        ] {
            assert_eq!(is_url_like(spec), expected, "{spec:?}");
        }
    }

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
        // Regression: the lenient name token `pkg_` must not reach extras parsing.
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
    fn rejects_blank_requirement() {
        for raw in ["", "   "] {
            let result = parse_requirement(
                raw,
                DependencyContext::Runtime,
                DependencyOrigin {
                    file: "requirements.txt".to_owned(),
                    line: Some(3),
                    label: "requirements.txt".to_owned(),
                },
            );
            assert_eq!(
                result.err(),
                Some(ManifestWarning::InvalidRequirementLine {
                    file: "requirements.txt".to_owned(),
                    line: Some(3),
                    label: "requirements.txt".to_owned(),
                    raw: raw.to_owned(),
                })
            );
        }
    }

    #[test]
    fn pep508_manifest_accepts_archive_like_bare_name() {
        // Regression: pip's reading rejects `A.tlz` as a file reference.
        for (raw, expected) in [
            ("A.tlz", "a-tlz"),
            ("pkg.whl", "pkg-whl"),
            ("x.tar", "x-tar"),
        ] {
            let dep = parse_pep508_requirement(
                raw,
                DependencyContext::Group("dev".to_owned()),
                DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: None,
                    label: "dependency-groups.dev[0]".to_owned(),
                },
            )
            .expect("bare PEP 508 name must parse");
            assert_eq!(dep.name, expected);
            assert!(!dep.opaque);
            assert_eq!(dep.specifier, None);
        }
    }

    #[test]
    fn pip_reading_treats_archive_like_bare_name_as_file() {
        let origin = DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: Some(1),
            label: "requirements.txt".to_owned(),
        };
        for raw in ["foo.tar.gz", "foo.whl", "foo.whl[tests]"] {
            assert!(
                parse_requirement(raw, DependencyContext::Runtime, origin.clone()).is_err(),
                "{raw:?}"
            );
            assert_eq!(requirement_name(raw), None, "{raw:?}");
        }
        let dep = parse_pep508_requirement("foo.whl[tests]", DependencyContext::Runtime, origin)
            .expect("PEP 508 reads it as a name");
        assert_eq!(dep.name, "foo-whl");
        assert_eq!(dep.extras, ["tests"]);
        assert_eq!(
            pep508_distribution_name("foo.whl[tests]").as_deref(),
            Some("foo-whl")
        );
    }

    #[test]
    fn invalid_manifest_field_keeps_its_label() {
        let result = parse_pep508_requirement(
            "pkg >=",
            DependencyContext::Runtime,
            DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: None,
                label: "project.dependencies[2]".to_owned(),
            },
        );
        assert_eq!(
            result.err(),
            Some(ManifestWarning::InvalidRequirementLine {
                file: "pyproject.toml".to_owned(),
                line: None,
                label: "project.dependencies[2]".to_owned(),
                raw: "pkg >=".to_owned(),
            })
        );
    }

    #[test]
    fn malformed_direct_reference_is_not_an_opaque_url() {
        let origin = DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: Some(1),
            label: "requirements.txt".to_owned(),
        };
        for raw in [
            "foo @ https://h/a b.whl",
            "foo[x] @ https://h/a b.whl#egg=foo",
        ] {
            assert!(
                parse_requirement(raw, DependencyContext::Runtime, origin.clone()).is_err(),
                "{raw:?}"
            );
        }
        let dep = parse_requirement(
            "git+https://h/r.git@v1#egg=foo",
            DependencyContext::Runtime,
            origin,
        )
        .expect("VCS URL with a ref keeps its egg name");
        assert_eq!(dep.name, "foo");
    }

    #[test]
    fn url_like_prefix_is_case_insensitive() {
        assert!(is_url_like("Git+ssh@host/repo"));
        assert!(is_url_like("HTTPS://h/x"));
        assert!(!is_url_like("gi"));
        assert!(!is_url_like("pkg>=1"));
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
