//! PEP 508 parsing helpers.

use super::pep508::{Requirement, looks_like_archive, strip_python};
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Normalize a distribution name to lowercase hyphen form (PEP 503):
/// runs of `-`, `_`, and `.` collapse into a single `-`.
#[must_use]
pub(crate) fn normalize_distribution_name(name: &str) -> String {
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
pub(super) fn extract_egg_name(spec: &str) -> Option<String> {
    let fragment = spec.split('#').nth(1)?;
    for part in fragment.split('&') {
        if let Some(egg) = part.strip_prefix("egg=") {
            let trimmed = strip_python(egg);
            if !trimmed.is_empty() {
                return Some(normalize_distribution_name(trimmed));
            }
        }
    }
    None
}

/// Parse a requirements-file line into a declared dependency, the way pip reads it.
///
/// A line pip installs from a directory or an archive (`./pkg`, `pkg/sub`,
/// `wheels/foo.whl`, `foo-1.0-py3-none-any.whl`) is an opaque dependency, as a
/// `-e ./path` line is (spec §4, #963). The shape alone decides: pip errors
/// when the directory does not exist, and chokkin does not check.
pub(super) fn parse_requirement(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> Result<DeclaredDependency, ManifestWarning> {
    parse_with(raw, context, origin, parse_pip_requirement, true)
}

/// Parse a requirement from a PEP 508 manifest field (`pyproject.toml`,
/// `setup.cfg`, `setup.py`).
///
/// Unlike [`parse_requirement`], a name ending in an archive extension
/// (`foo.tlz`, `foo.whl[x]`) is a distribution name here, as in `packaging`.
pub(super) fn parse_pep508_requirement(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
) -> Result<DeclaredDependency, ManifestWarning> {
    parse_with(
        raw,
        context,
        origin,
        super::pep508::parse_requirement,
        false,
    )
}

fn parse_with(
    raw: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
    parse: fn(&str) -> Option<Requirement>,
    files: bool,
) -> Result<DeclaredDependency, ManifestWarning> {
    let trimmed = strip_python(raw);
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
    } else if is_url_like(trimmed) || (files && looks_like_pip_file(trimmed)) {
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

pub(super) fn invalid(raw: &str, origin: DependencyOrigin) -> ManifestWarning {
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

/// `name [extras] @ ...`, the PEP 508 direct-reference form. The name only
/// needs name characters, so `foo_ @ url` is a malformed requirement rather
/// than an opaque URL.
fn is_direct_reference(trimmed: &str) -> bool {
    trimmed.split_once('@').is_some_and(|(head, _)| {
        let name = strip_python(head.split('[').next().unwrap_or(head));
        !name.is_empty() && leading_name_token(name) == name
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
pub(crate) fn pep508_distribution_name(raw: &str) -> Option<String> {
    let trimmed = strip_python(raw);
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

/// Distribution an invalid requirement line may have declared (#928): its
/// leading name token, less the separators a PEP 508 name cannot start or end
/// on, so `rich_ >=13` names `rich`. `None` when no name character leads it
/// (`>=1.0`), the line is a path or an archive pip reads as a file
/// ([`looks_like_pip_file`]: `wheels/foo.whl`, `C:\pkg`,
/// `foo-1.0-py3-none-any.whl`; a requirements file reads these as opaque, a
/// manifest field as invalid), or another word follows it
/// (`not a requirement`), which no requirement has, or a `:` follows it, as
/// in a URL (`file:../other`), or it starts with `-`, which pip reads as an
/// option (`-r` alone, `-example-pkg`) and a PEP 508 name cannot start on:
/// such a line names nothing to lower.
#[must_use]
pub(crate) fn invalid_requirement_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let token = leading_name_token(trimmed);
    let after = &trimmed[token.len()..];
    let name = token.trim_matches(['-', '_', '.']);
    let followed_by_word = after
        .trim_start()
        .starts_with(|ch: char| ch.is_ascii_alphanumeric());
    (!name.is_empty()
        && !token.starts_with('-')
        && !looks_like_pip_file(trimmed)
        && !after.starts_with(':')
        && !followed_by_word)
        .then(|| normalize_distribution_name(name))
}

/// pip reads the line as a directory or an archive, not a requirement
/// (`_looks_like_path` and `is_archive_file`, #963): it starts with `.`, a
/// path separator follows its leading name token (`pkg/sub`, `/abs`, the
/// drive `C:\pkg`), or the token is an archive name (`foo.whl`,
/// `foo.whl[tests]`; pip's `_strip_extras` takes `[...]` only at the very
/// end). `pkg >= 1/2` and `foo:bar` are not paths, and `foo.whl>=1` and
/// `foo.whl[tests]>=1` are requirements named `foo.whl`, as in pip.
#[must_use]
pub(super) fn looks_like_pip_file(trimmed: &str) -> bool {
    let token = leading_name_token(trimmed);
    let after = &trimmed[token.len()..];
    trimmed.starts_with('.')
        || after.starts_with(['/', '\\'])
        || after
            .strip_prefix(':')
            .is_some_and(|rest| rest.starts_with(['/', '\\']))
        || (looks_like_archive(token)
            && (after.is_empty() || (after.starts_with('[') && after.ends_with(']'))))
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
    fn invalid_requirement_name_reads_the_leading_name() {
        for (raw, expected) in [
            ("PyYAML>=", Some("pyyaml")),
            (" rich_ >=13", Some("rich")),
            ("click==8.1 \\", Some("click")),
            ("zope.interface[", Some("zope-interface")),
            (">=1.0", None),
            ("./pkg", None),
            ("{{ name }}>=1", None),
            ("not a requirement!!", None),
            ("requests >= 2 ;;", Some("requests")),
            ("wheels/foo-1.0-py3-none-any.whl", None),
            ("C:\\wheels\\foo.whl", None),
            ("foo-1.0-py3-none-any.whl", None),
            ("-r", None),
            ("-example-pkg", None),
            ("file:../other", None),
            ("foo:bar", None),
            ("", None),
        ] {
            assert_eq!(
                invalid_requirement_name(raw).as_deref(),
                expected,
                "{raw:?}"
            );
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
    fn strips_information_separators_like_str_strip() {
        // pip and setuptools strip with `str.strip`, which drops U+001C..U+001F
        // (#890); the opaque path keeps the stripped text.
        let origin = DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: Some(1),
            label: "requirements.txt".to_owned(),
        };
        let dep = parse_requirement(
            "\u{1c}git+https://h/r.git#egg=acme\u{1f}",
            DependencyContext::Runtime,
            origin,
        )
        .expect("valid");
        assert_eq!(dep.name, "acme");
        assert_eq!(
            dep.specifier.as_deref(),
            Some("git+https://h/r.git#egg=acme")
        );
        assert_eq!(
            pep508_distribution_name("git+https://h/r.git#egg=acme\u{1f}").as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn direct_reference_name_ends_at_python_whitespace() {
        // `qux\x1f@ url` is a malformed `name @ url`, not an opaque URL line.
        let origin = DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: Some(1),
            label: "requirements.txt".to_owned(),
        };
        for raw in ["qux\u{1f}@ https://h/x.whl", "qux\u{a0}@ https://h/x.whl"] {
            assert!(
                parse_requirement(raw, DependencyContext::Runtime, origin.clone()).is_err(),
                "{raw:?}"
            );
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
            // #963: an archive pip installs from a file is an opaque dependency.
            let dep = parse_requirement(raw, DependencyContext::Runtime, origin.clone())
                .expect("pip reads it as a file");
            assert!(dep.opaque, "{raw:?}");
            assert_eq!(dep.name, "", "{raw:?}");
            assert_eq!(dep.specifier.as_deref(), Some(raw), "{raw:?}");
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
    fn pip_reading_treats_path_lines_as_opaque_files() {
        // #963: the shapes pip's `_looks_like_path` and `is_archive_file`
        // accept; a PEP 508 manifest field has no path form, so it stays invalid.
        let origin = DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: Some(1),
            label: "requirements.txt".to_owned(),
        };
        for raw in [
            "../../../llama-index-core",
            "./pkg",
            ".",
            ".venv/pkg",
            "pkg/sub",
            "/abs/pkg",
            "wheels/foo-1.0-py3-none-any.whl",
            "C:\\pkg",
            "foo-1.0-py3-none-any.whl",
        ] {
            let dep = parse_requirement(raw, DependencyContext::Runtime, origin.clone())
                .unwrap_or_else(|warning| panic!("{raw:?} must be opaque, got {warning:?}"));
            assert!(dep.opaque, "{raw:?}");
            assert_eq!(dep.name, "", "{raw:?}");
            assert_eq!(dep.specifier.as_deref(), Some(raw), "{raw:?}");
            assert_eq!(requirement_name(raw), None, "{raw:?}");
            if !looks_like_archive(raw) {
                assert!(
                    parse_pep508_requirement(raw, DependencyContext::Runtime, origin.clone())
                        .is_err(),
                    "{raw:?} is not a PEP 508 requirement"
                );
            }
        }
        // Not a path: a separator after the name token is a version, and a
        // bare `name:x` is not a drive.
        for raw in ["pkg >= 1/2", "foo:bar", "not a requirement!!"] {
            assert!(
                parse_requirement(raw, DependencyContext::Runtime, origin.clone()).is_err(),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn looks_like_pip_file_reads_the_line_shape() {
        for (raw, expected) in [
            ("./pkg", true),
            ("../pkg", true),
            (".", true),
            (".foo", true),
            ("pkg/sub", true),
            ("/abs", true),
            ("C:\\pkg", true),
            ("C:/pkg", true),
            ("foo.whl", true),
            ("foo.whl[tests]", true),
            ("foo.tar.gz", true),
            ("foo:bar", false),
            ("pkg >= 1/2", false),
            ("requests", false),
            ("foo.whl>=1", false),
            ("foo.whl[tests]>=1", false),
            ("", false),
        ] {
            assert_eq!(looks_like_pip_file(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_may_be_any_unicode_whitespace() {
        // pip and setuptools strip each line with `str.strip` (#879).
        let dep = parse_pep508_requirement(
            "\u{a0}foo>=1\u{3000}",
            DependencyContext::Runtime,
            DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: None,
                label: "project.dependencies[0]".to_owned(),
            },
        )
        .expect("valid");
        assert_eq!(dep.name, "foo");
        assert_eq!(dep.specifier.as_deref(), Some(">=1"));
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
            // #572: a name `packaging` reads but PEP 508 rejects
            "foo_ @ https://h/x.whl",
            "foo- @ https://h/x.whl",
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
