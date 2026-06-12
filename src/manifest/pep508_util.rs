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

    let requirement = Requirement::<VerbatimUrl>::from_str(trimmed).map_err(|_| {
        ManifestWarning::InvalidRequirementLine {
            file: origin.file.clone(),
            line: origin.line.unwrap_or(0),
            raw: raw.to_owned(),
        }
    })?;

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

    Ok(DeclaredDependency {
        name,
        extras,
        marker,
        specifier,
        context,
        origin,
        opaque,
    })
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
}
