//! Ignore rule matching for issue emission (§18).

use std::collections::BTreeMap;

use globset::Glob;

use crate::config::ChokkinConfig;
use crate::parser::{IgnoreDirective, ParseSummary};
use crate::resolver::ResolutionIndex;
use crate::rules::types::{IssueCandidate, IssueSubject, Origin, RuleId, SuppressReason};

/// Import site (file, line, dotted module) used to recover a distribution name.
type ImportSite = (String, u32, String);

/// Compiled ignore matchers for config and source directives.
#[derive(Debug)]
pub struct IgnoreMatcher {
    config: BTreeMap<RuleId, Vec<String>>,
    directives: BTreeMap<String, Vec<IgnoreDirective>>,
    // CHK003/CHK004 carry an `Import` subject, but §18 matches dependency rules
    // on the distribution name, which only the resolver knows.
    distributions: BTreeMap<ImportSite, String>,
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
        Self::build_with_resolution(config, parse, &ResolutionIndex::empty())
    }

    /// Build matchers, resolving distribution names for dependency-rule ignores.
    pub(crate) fn build_with_resolution(
        config: &ChokkinConfig,
        parse: &ParseSummary,
        resolution: &ResolutionIndex,
    ) -> Self {
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

        let mut distributions = BTreeMap::new();
        for import in &resolution.imports {
            if let Some(distribution) = &import.distribution {
                distributions.insert(
                    (import.file.clone(), import.line, import.full_module.clone()),
                    distribution.clone(),
                );
            }
        }

        Self {
            config: config_rules,
            directives,
            distributions,
        }
    }

    /// Whether a pre-issue candidate should be suppressed.
    pub fn matches_candidate(&self, candidate: &IssueCandidate) -> IgnoreMatch {
        let file = file_path_for(&candidate.subject, &candidate.origins);
        if self.matches_config(candidate.rule, &candidate.subject, file.as_deref()) {
            return IgnoreMatch::Config;
        }
        self.matches_directives(candidate.rule, file.as_deref(), candidate_line(candidate))
    }
}

impl IgnoreMatcher {
    fn matches_config(&self, rule: RuleId, subject: &IssueSubject, file: Option<&str>) -> bool {
        let Some(patterns) = self.config.get(&rule) else {
            return false;
        };
        let distribution = self.distribution_for(subject);
        patterns.iter().any(|pattern| {
            config_pattern_matches(rule, pattern, subject, file, distribution.as_deref())
        })
    }

    /// Distribution the candidate is really about, for `Import` subjects.
    fn distribution_for(&self, subject: &IssueSubject) -> Option<String> {
        let IssueSubject::Import { module, file, line } = subject else {
            return None;
        };
        self.distributions
            .get(&(file.clone(), *line, module.clone()))
            .cloned()
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

fn config_pattern_matches(
    rule: RuleId,
    pattern: &str,
    subject: &IssueSubject,
    file: Option<&str>,
    distribution: Option<&str>,
) -> bool {
    if let Some((path_pattern, symbol_pattern)) = pattern.split_once(':') {
        return symbol_pattern_matches(rule, path_pattern, symbol_pattern, subject, file);
    }

    match subject {
        IssueSubject::File { path } if is_file_rule(rule) => glob_match(pattern, path),
        IssueSubject::Distribution { name } if is_distribution_rule(rule) => {
            glob_match(pattern, name)
        },
        IssueSubject::Binary { name } if rule == RuleId::Chk008 => glob_match(pattern, name),
        // §18 matches dependency rules on the distribution name even when the
        // candidate points at an import site, so a path glob must not match.
        IssueSubject::Import { .. } if is_distribution_rule(rule) => {
            distribution.is_some_and(|name| glob_match(pattern, name))
        },
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
            file,
        ),
        _ => false,
    }
}

fn symbol_pattern_matches(
    rule: RuleId,
    path_pattern: &str,
    symbol_pattern: &str,
    subject: &IssueSubject,
    file: Option<&str>,
) -> bool {
    if !is_symbol_rule(rule) {
        return false;
    }
    let IssueSubject::Symbol { module, name } = subject else {
        return false;
    };
    let module_path = module.replace('.', "/");
    let path_matches = file.is_some_and(|path| glob_match(path_pattern, path))
        || glob_match(path_pattern, &module_path);
    path_matches && glob_match(symbol_pattern, name)
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

    const APP: &str = "src/acme/app.py";

    fn import_candidate(rule: RuleId, module: &str) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject: IssueSubject::Import {
                module: module.to_owned(),
                file: APP.to_owned(),
                line: 1,
            },
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: APP.to_owned(),
                line: 1,
                module: module.to_owned(),
            }],
            explain: ExplainData::default(),
        }
    }

    fn resolution_for(module: &str, distribution: &str) -> ResolutionIndex {
        ResolutionIndex {
            imports: vec![crate::resolver::ResolvedImport {
                import_root: module.split('.').next().unwrap_or(module).to_owned(),
                full_module: module.to_owned(),
                file: APP.to_owned(),
                workspace_member: None,
                line: 1,
                context: crate::parser::ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                origin: crate::graph::ModuleOrigin::ThirdParty,
                distribution: Some(distribution.to_owned()),
                confidence: crate::resolver::ResolveConfidence::Certain,
            }],
            ..ResolutionIndex::empty()
        }
    }

    fn ignores(patterns: &[&str], rule: RuleId, module: &str, distribution: &str) -> bool {
        let mut config = default_config();
        config.ignore.insert(
            rule.as_code().to_owned(),
            patterns.iter().map(|p| (*p).to_owned()).collect(),
        );
        let matcher = IgnoreMatcher::build_with_resolution(
            &config,
            &ParseSummary::empty(),
            &resolution_for(module, distribution),
        );
        matcher.matches_candidate(&import_candidate(rule, module)) == IgnoreMatch::Config
    }

    /// §18: dependency-rule ignores are distribution-name globs, even though
    /// CHK003/CHK004 candidates point at an import site.
    #[test]
    fn config_ignore_matches_distribution_for_import_subject() {
        assert!(ignores(&["pyyaml"], RuleId::Chk003, "yaml", "pyyaml"));
        assert!(ignores(
            &["google-cloud-*"],
            RuleId::Chk004,
            "google.cloud.storage",
            "google-cloud-storage"
        ));
        assert!(ignores(
            &["setuptools"],
            RuleId::Chk003,
            "pkg_resources",
            "setuptools"
        ));
    }

    /// Module names and path globs are not distribution names, so they must not
    /// silence a dependency rule.
    #[test]
    fn config_ignore_rejects_module_and_path_patterns_for_dependency_rules() {
        assert!(!ignores(&["yaml"], RuleId::Chk003, "yaml", "pyyaml"));
        assert!(!ignores(&["src/acme/*"], RuleId::Chk004, "yaml", "pyyaml"));
    }

    /// Without a resolved distribution there is nothing to match against.
    #[test]
    fn config_ignore_without_resolution_leaves_dependency_candidate() {
        let mut config = default_config();
        config
            .ignore
            .insert("CHK003".to_owned(), vec!["pyyaml".to_owned()]);
        let matcher = IgnoreMatcher::build(&config, &ParseSummary::empty());
        assert_eq!(
            matcher.matches_candidate(&import_candidate(RuleId::Chk003, "yaml")),
            IgnoreMatch::None
        );
    }

    #[test]
    fn inline_ignore_matches_same_line() {
        let config = default_config();
        let mut parse = ParseSummary::empty();
        parse.modules.push(crate::parser::ParsedModule {
            path: "src/acme/main.py".to_owned(),
            imports: Vec::new(),
            dynamic_imports: Vec::new(),
            attribute_accesses: Vec::new(),
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
            attribute_accesses: Vec::new(),
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

    #[test]
    fn config_ignore_matches_symbol_file_path_pattern() {
        let mut config = default_config();
        config.ignore.insert(
            "CHK006".to_owned(),
            vec!["src/acme/api.py:dead_*".to_owned()],
        );
        let matcher = IgnoreMatcher::build(&config, &ParseSummary::empty());
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

        assert_eq!(matcher.matches_candidate(&candidate), IgnoreMatch::Config);
    }

    #[test]
    fn config_ignore_keeps_symbol_module_path_fallback() {
        let mut config = default_config();
        config
            .ignore
            .insert("CHK006".to_owned(), vec!["acme/api:dead_*".to_owned()]);
        let matcher = IgnoreMatcher::build(&config, &ParseSummary::empty());
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
            origins: Vec::new(),
            explain: ExplainData::default(),
        };

        assert_eq!(matcher.matches_candidate(&candidate), IgnoreMatch::Config);
    }
}
