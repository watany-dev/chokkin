//! PEP 508 parsing helpers.

use std::str::FromStr;

use pep508_rs::{Requirement, VerbatimUrl};

use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Normalize a distribution name to lowercase hyphen form (PEP 503).
#[must_use]
pub fn normalize_distribution_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch == '_' {
                '-'
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect()
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

    if let Ok(requirement) = Requirement::<VerbatimUrl>::from_str(trimmed) {
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
}
