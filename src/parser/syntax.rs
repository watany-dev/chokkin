//! Target-version syntax feature gates.

use crate::config::TargetVersion;

/// Syntax features gated by `target_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxFeature {
    /// `match` statement (3.10+).
    MatchStatement,
    /// `type` alias statement (3.12+).
    TypeAliasStatement,
    /// PEP 695 generic functions/classes (3.12+).
    #[allow(dead_code)]
    Pep695Generics,
}

/// Returns whether `target` supports `feature`.
#[must_use]
pub fn supports_syntax(target: &TargetVersion, feature: SyntaxFeature) -> bool {
    let minor = target.minor();
    match feature {
        SyntaxFeature::MatchStatement => minor >= 10,
        SyntaxFeature::TypeAliasStatement | SyntaxFeature::Pep695Generics => minor >= 12,
    }
}

/// Human-readable requirement label for diagnostics.
#[must_use]
pub fn feature_requirement(feature: SyntaxFeature) -> &'static str {
    match feature {
        SyntaxFeature::MatchStatement => "py310",
        SyntaxFeature::TypeAliasStatement | SyntaxFeature::Pep695Generics => "py312",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py311_supports_match_not_pep695() {
        let target = TargetVersion::default_py311();
        assert!(supports_syntax(&target, SyntaxFeature::MatchStatement));
        assert!(!supports_syntax(&target, SyntaxFeature::Pep695Generics));
    }
}
