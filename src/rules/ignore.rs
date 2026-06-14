//! Ignore rule matching for issue emission (§18).

use std::collections::BTreeMap;

use globset::Glob;

use crate::config::ChokkinConfig;
use crate::parser::{IgnoreDirective, ParseSummary};
use crate::rules::types::{Issue, IssueCandidate, IssueSubject, Origin, RuleId, SuppressReason};

/// Compiled ignore matchers for config and source directives.
#[derive(Debug)]
pub struct IgnoreMatcher {
    config: BTreeMap<RuleId, Vec<String>>,
    directives: BTreeMap<String, Vec<IgnoreDirective>>,
}

/// Outcome of ignore evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreMatch {
    /// Issue is not ignored.
    None,
    /// Matched config ignore pattern.
    Config,
    /// Matched inline directive on the same line.
    Inline,
    /// Matched file-level directive.
    FileLevel,
}

impl IgnoreMatch {
    /// Maps to [`SuppressReason`] when ignored.
    pub const fn reason(self) -> Option<SuppressReason> {
        match self {
            Self::None => None,
            Self::Config => Some(SuppressReason::Config),
            Self::Inline => Some(SuppressReason::Inline),
            Self::FileLevel => Some(SuppressReason::FileLevel),
        }
    }
}

impl IgnoreMatcher {
    /// Build matchers from config and parsed modules.
    ///
    /// Invalid glob patterns are skipped (config validation should catch most).
    pub fn build(config: &ChokkinConfig, parse: &ParseSummary) -> Self {
        let mut config_rules = BTreeMap::new();
        for (code, patterns) in &config.ignore {
            let Some(rule) = RuleId::parse_code(code) else {
                continue;
            };
            if patterns.is_empty() {
                continue;
            }
            config_rules.insert(rule, patterns.clone());
        }

        let mut directives = BTreeMap::new();
        for module in &parse.modules {
            if module.ignores.is_empty() {
                continue;
            }
            directives.insert(module.path.clone(), module.ignores.clone());
        }

        Self {
            config: config_rules,
            directives,
        }
    }

    /// Whether a pre-issue candidate should be suppressed.
    pub fn matches_candidate(&self, candidate: &IssueCandidate) -> IgnoreMatch {
        if self.matches_config(candidate.rule, &candidate.subject) {
            return IgnoreMatch::Config;
        }
        let file = file_path_for(&candidate.subject, &candidate.origins);
        self.matches_directives(candidate.rule, file.as_deref(), candidate_line(candidate))
    }

    /// Whether a final issue should be suppressed.
    #[allow(dead_code)] // used by Phase 1 CLI re-filtering
    pub fn matches_issue(&self, issue: &Issue) -> IgnoreMatch {
        if self.matches_config(issue.rule, &issue.subject) {
            return IgnoreMatch::Config;
        }
        let file = issue
            .location
            .file
            .as_deref()
            .or_else(|| subject_file_path(&issue.subject));
        let line = issue.location.line;
        self.matches_directives(issue.rule, file, line)
    }
}

impl IgnoreMatcher {
    fn matches_config(&self, rule: RuleId, subject: &IssueSubject) -> bool {
        let Some(patterns) = self.config.get(&rule) else {
            return false;
        };
        patterns
            .iter()
            .any(|pattern| config_pattern_matches(rule, pattern, subject))
    }

    fn matches_directives(
        &self,
        rule: RuleId,
        file: Option<&str>,
        line: Option<u32>,
    ) -> IgnoreMatch {
        let Some(path) = file else {
            return IgnoreMatch::None;
        };
        let Some(directives) = self.directives.get(path) else {
            return IgnoreMatch::None;
        };

        let code = rule.as_code();
        for directive in directives {
            if !directive.codes.iter().any(|entry| entry == code) {
                continue;
            }
            if directive.file_level {
                return IgnoreMatch::FileLevel;
            }
            if line == Some(directive.line) {
                return IgnoreMatch::Inline;
            }
        }
        IgnoreMatch::None
    }
}

fn candidate_line(candidate: &IssueCandidate) -> Option<u32> {
    for origin in &candidate.origins {
        if let Origin::Import { line, .. } = origin {
            return Some(*line);
        }
    }
    match &candidate.subject {
        IssueSubject::Import { line, .. } => Some(*line),
        _ => None,
    }
}

fn file_path_for(subject: &IssueSubject, origins: &[Origin]) -> Option<String> {
    for origin in origins {
        if let Origin::Import { file, .. } = origin {
            return Some(file.clone());
        }
    }
    subject_file_path(subject).map(str::to_owned)
}

fn subject_file_path(subject: &IssueSubject) -> Option<&str> {
    match subject {
        IssueSubject::File { path } => Some(path.as_str()),
        IssueSubject::Import { file, .. } => Some(file.as_str()),
        _ => None,
    }
}

fn config_pattern_matches(rule: RuleId, pattern: &str, subject: &IssueSubject) -> bool {
    if let Some((path_pattern, symbol_pattern)) = pattern.split_once(':') {
        return symbol_pattern_matches(rule, path_pattern, symbol_pattern, subject);
    }

    match subject {
        IssueSubject::File { path } if is_file_rule(rule) => glob_match(pattern, path),
        IssueSubject::Distribution { name } if is_distribution_rule(rule) => {
            glob_match(pattern, name)
        },
        IssueSubject::Binary { name } if rule == RuleId::Chk008 => glob_match(pattern, name),
        IssueSubject::Import { module, file, .. } => {
            glob_match(pattern, file) || glob_match(pattern, module)
        },
        IssueSubject::Symbol { module, name } if is_symbol_rule(rule) => symbol_pattern_matches(
            rule,
            pattern,
            "*",
            &IssueSubject::Symbol {
                module: module.clone(),
                name: name.clone(),
            },
        ),
        _ => false,
    }
}

fn symbol_pattern_matches(
    rule: RuleId,
    path_pattern: &str,
    symbol_pattern: &str,
    subject: &IssueSubject,
) -> bool {
    if !is_symbol_rule(rule) {
        return false;
    }
    let IssueSubject::Symbol { module, name } = subject else {
        return false;
    };
    let path = module.replace('.', "/");
    glob_match(path_pattern, &path) && glob_match(symbol_pattern, name)
}

fn is_file_rule(rule: RuleId) -> bool {
    matches!(rule, RuleId::Chk001 | RuleId::Chk010)
}

fn is_distribution_rule(rule: RuleId) -> bool {
    matches!(
        rule,
        RuleId::Chk002
            | RuleId::Chk003
            | RuleId::Chk004
            | RuleId::Chk005
            | RuleId::Chk008
            | RuleId::Chk009
    )
}

fn is_symbol_rule(rule: RuleId) -> bool {
    matches!(rule, RuleId::Chk006 | RuleId::Chk007)
}

fn glob_match(pattern: &str, value: &str) -> bool {
    Glob::new(pattern)
        .ok()
        .and_then(|glob| glob.compile_matcher().is_match(value).then_some(()))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::parser::ParseSummary;
    use crate::rules::types::{ExplainData, Severity};

    #[test]
    fn config_ignore_matches_distribution() {
        let mut config = default_config();
        config
            .ignore
            .insert("CHK002".to_owned(), vec!["boto3".to_owned()]);
        let matcher = IgnoreMatcher::build(&config, &ParseSummary::empty());
        let candidate = IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::Distribution {
                name: "boto3".to_owned(),
            },
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "unused".to_owned(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData::default(),
        };
        assert_eq!(matcher.matches_candidate(&candidate), IgnoreMatch::Config);
    }

    #[test]
    fn inline_ignore_matches_same_line() {
        let config = default_config();
        let mut parse = ParseSummary::empty();
        parse.modules.push(crate::parser::ParsedModule {
            path: "src/acme/main.py".to_owned(),
            imports: Vec::new(),
            dynamic_imports: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            ignores: vec![IgnoreDirective {
                file_level: false,
                codes: vec!["CHK003".to_owned()],
                line: 4,
            }],
            has_opaque_dynamic_import: false,
            diagnostics: Vec::new(),
        });
        let matcher = IgnoreMatcher::build(&config, &parse);
        let candidate = IssueCandidate {
            rule: RuleId::Chk003,
            subject: IssueSubject::Import {
                module: "missing".to_owned(),
                file: "src/acme/main.py".to_owned(),
                line: 4,
            },
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: "src/acme/main.py".to_owned(),
                line: 4,
                module: "missing".to_owned(),
            }],
            explain: ExplainData::default(),
        };
        assert_eq!(matcher.matches_candidate(&candidate), IgnoreMatch::Inline);
    }

    #[test]
    fn inline_ignore_matches_symbol_issue() {
        let config = default_config();
        let mut parse = ParseSummary::empty();
        parse.modules.push(crate::parser::ParsedModule {
            path: "src/acme/api.py".to_owned(),
            imports: Vec::new(),
            dynamic_imports: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            ignores: vec![IgnoreDirective {
                file_level: false,
                codes: vec!["CHK006".to_owned()],
                line: 12,
            }],
            has_opaque_dynamic_import: false,
            diagnostics: Vec::new(),
        });
        let matcher = IgnoreMatcher::build(&config, &parse);
        let candidate = IssueCandidate {
            rule: RuleId::Chk006,
            subject: IssueSubject::Symbol {
                module: "acme.api".to_owned(),
                name: "dead_api".to_owned(),
            },
            severity: Severity::Warning,
            confidence: crate::config::Confidence::Likely,
            message: "unused export".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: "src/acme/api.py".to_owned(),
                line: 12,
                module: "acme.api".to_owned(),
            }],
            explain: ExplainData::default(),
        };
        assert_eq!(matcher.matches_candidate(&candidate), IgnoreMatch::Inline);
    }
}
