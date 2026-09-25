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
    // CHK008 carries the binary name, but §18 names the (already normalized)
    // distribution it maps to.
    binary_distributions: BTreeMap<String, String>,
}

impl IgnoreMatcher {
    /// Build matchers from config, parsed modules, and resolved imports.
    ///
    /// Invalid glob patterns are skipped (config validation should catch most).
    /// `resolution` supplies the distribution names that dependency-rule
    /// ignores match against (§18); pass `ResolutionIndex::default()` when it is
    /// not available.
    pub fn build(
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
            binary_distributions: resolution.binary_resolutions.clone(),
        }
    }

    /// Why a pre-issue candidate is suppressed, or `None` when it is not.
    pub fn matches_candidate(&self, candidate: &IssueCandidate) -> Option<SuppressReason> {
        let file = file_path_for(&candidate.subject, &candidate.origins);
        if self.matches_config(candidate.rule, &candidate.subject, file.as_deref()) {
            return Some(SuppressReason::Config);
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

    /// Distribution the candidate is really about, for `Import` and `Binary`
    /// subjects.
    fn distribution_for(&self, subject: &IssueSubject) -> Option<String> {
        match subject {
            IssueSubject::Import { module, file, line } => self
                .distributions
                .get(&(file.clone(), *line, module.clone()))
                .cloned(),
            IssueSubject::Binary { name } => self.binary_distributions.get(name).cloned(),
            _ => None,
        }
    }

    fn matches_directives(
        &self,
        rule: RuleId,
        file: Option<&str>,
        line: Option<u32>,
    ) -> Option<SuppressReason> {
        let directives = self.directives.get(file?)?;

        let code = rule.as_code();
        for directive in directives {
            if !directive.codes.iter().any(|entry| entry == code) {
                continue;
            }
            if directive.file_level {
                return Some(SuppressReason::FileLevel);
            }
            if line == Some(directive.line) {
                return Some(SuppressReason::Inline);
            }
        }
        None
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
        // The binary name itself stays accepted for backward compatibility.
        IssueSubject::Binary { name } if rule == RuleId::Chk008 => {
            glob_match(pattern, name) || distribution.is_some_and(|dist| glob_match(pattern, dist))
        },
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
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
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
        assert_eq!(
            matcher.matches_candidate(&candidate),
            Some(SuppressReason::Config)
        );
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
            ..ResolutionIndex::default()
        }
    }

    fn ignores(patterns: &[&str], rule: RuleId, module: &str, distribution: &str) -> bool {
        let mut config = default_config();
        config.ignore.insert(
            rule.as_code().to_owned(),
            patterns.iter().map(|p| (*p).to_owned()).collect(),
        );
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &resolution_for(module, distribution),
        );
        matcher.matches_candidate(&import_candidate(rule, module)) == Some(SuppressReason::Config)
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
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
        assert_eq!(
            matcher.matches_candidate(&import_candidate(RuleId::Chk003, "yaml")),
            None
        );
    }

    fn ignores_binary(patterns: &[&str], binary: &str, distribution: Option<&str>) -> bool {
        let mut config = default_config();
        config.ignore.insert(
            "CHK008".to_owned(),
            patterns.iter().map(|p| (*p).to_owned()).collect(),
        );
        let mut resolution = ResolutionIndex::default();
        if let Some(distribution) = distribution {
            resolution
                .binary_resolutions
                .insert(binary.to_owned(), distribution.to_owned());
        }
        let matcher = IgnoreMatcher::build(&config, &ParseSummary::default(), &resolution);
        let candidate = IssueCandidate {
            rule: RuleId::Chk008,
            subject: IssueSubject::Binary {
                name: binary.to_owned(),
            },
            severity: Severity::Warning,
            confidence: crate::config::Confidence::Certain,
            message: "unlisted binary".to_owned(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData::default(),
        };
        matcher.matches_candidate(&candidate) == Some(SuppressReason::Config)
    }

    /// §18: CHK008 ignores name the distribution the binary maps to; the binary
    /// name keeps working for backward compatibility.
    #[test]
    fn config_ignore_matches_binary_distribution_or_name() {
        assert!(ignores_binary(&["sphinx"], "sphinx-build", Some("sphinx")));
        assert!(ignores_binary(
            &["sphinx-build"],
            "sphinx-build",
            Some("sphinx")
        ));
        assert!(!ignores_binary(&["pytest"], "sphinx-build", Some("sphinx")));
        assert!(!ignores_binary(&["sphinx"], "sphinx-build", None));
    }

    #[test]
    fn inline_ignore_matches_same_line() {
        let config = default_config();
        let mut parse = ParseSummary::default();
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
            decorator_sites: Vec::new(),
            diagnostics: Vec::new(),
        });
        let matcher = IgnoreMatcher::build(&config, &parse, &ResolutionIndex::default());
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
        assert_eq!(
            matcher.matches_candidate(&candidate),
            Some(SuppressReason::Inline)
        );
    }

    #[test]
    fn inline_ignore_matches_symbol_issue() {
        let config = default_config();
        let mut parse = ParseSummary::default();
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
            decorator_sites: Vec::new(),
            diagnostics: Vec::new(),
        });
        let matcher = IgnoreMatcher::build(&config, &parse, &ResolutionIndex::default());
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
        assert_eq!(
            matcher.matches_candidate(&candidate),
            Some(SuppressReason::Inline)
        );
    }

    #[test]
    fn config_ignore_matches_symbol_file_path_pattern() {
        let mut config = default_config();
        config.ignore.insert(
            "CHK006".to_owned(),
            vec!["src/acme/api.py:dead_*".to_owned()],
        );
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
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

        assert_eq!(
            matcher.matches_candidate(&candidate),
            Some(SuppressReason::Config)
        );
    }

    #[test]
    fn config_ignore_keeps_symbol_module_path_fallback() {
        let mut config = default_config();
        config
            .ignore
            .insert("CHK006".to_owned(), vec!["acme/api:dead_*".to_owned()]);
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
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

        assert_eq!(
            matcher.matches_candidate(&candidate),
            Some(SuppressReason::Config)
        );
    }
}
