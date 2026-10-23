//! Ignore rule matching for issue emission (§18).

use std::collections::BTreeMap;

use globset::{Glob, GlobSet};

use crate::config::ChokkinConfig;
use crate::parser::{IgnoreDirective, ParseSummary};
use crate::resolver::ResolutionIndex;
use crate::rules::types::{IssueCandidate, IssueSubject, Origin, RuleId, SuppressReason};
use crate::sources::build_glob_set;

/// Compiled ignore matchers for config and source directives.
#[derive(Debug)]
pub(super) struct IgnoreMatcher {
    config: BTreeMap<RuleId, Vec<String>>,
    directives: BTreeMap<String, Vec<IgnoreDirective>>,
    // CHK008 carries the binary name, but §18 names the (already normalized)
    // distribution it maps to.
    binary_distributions: BTreeMap<String, String>,
    vendored: GlobSet,
}

impl IgnoreMatcher {
    /// Build matchers from config, parsed modules, and resolved imports.
    ///
    /// Invalid glob patterns are skipped (config validation should catch most).
    /// `resolution` supplies the binary → distribution names that CHK008
    /// ignores match against (§18); pass `ResolutionIndex::default()` when it is
    /// not available.
    pub(super) fn build(
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

        Self {
            config: config_rules,
            directives,
            binary_distributions: resolution.binary_resolutions.clone(),
            vendored: build_glob_set(&config.vendored).unwrap_or_else(|_| GlobSet::empty()),
        }
    }

    /// Why a pre-issue candidate is suppressed, or `None` when it is not.
    pub(super) fn matches_candidate(&self, candidate: &IssueCandidate) -> Option<SuppressReason> {
        let file = file_path_for(&candidate.subject, &candidate.origins);
        if file
            .as_deref()
            .is_some_and(|path| self.vendored.is_match(path))
        {
            return Some(SuppressReason::Vendored);
        }
        if self.matches_config(candidate.rule, &candidate.subject, file.as_deref()) {
            return Some(SuppressReason::Config);
        }
        self.matches_directives(candidate.rule, file.as_deref(), &candidate_lines(candidate))
    }

    /// Whether a directive or the vendored globs silence `rule` at one
    /// import site, before rules merge sites into one finding (#1107).
    pub(super) fn ignores_site(&self, rule: RuleId, file: &str, line: Option<u32>) -> bool {
        let lines: Vec<u32> = line.into_iter().collect();
        self.vendored.is_match(file) || self.matches_directives(rule, Some(file), &lines).is_some()
    }
}

impl IgnoreMatcher {
    fn matches_config(&self, rule: RuleId, subject: &IssueSubject, file: Option<&str>) -> bool {
        let Some(patterns) = self.config.get(&rule) else {
            return false;
        };
        let distribution = self.distribution_for(subject);
        patterns
            .iter()
            .any(|pattern| config_pattern_matches(rule, pattern, subject, file, distribution))
    }

    /// Distribution the candidate is really about, for `Import` and `Binary`
    /// subjects.
    fn distribution_for<'s>(&'s self, subject: &'s IssueSubject) -> Option<&'s str> {
        match subject {
            IssueSubject::Import { distribution, .. } => distribution.as_deref(),
            IssueSubject::Binary { name } => {
                self.binary_distributions.get(name).map(String::as_str)
            },
            _ => None,
        }
    }

    fn matches_directives(
        &self,
        rule: RuleId,
        file: Option<&str>,
        lines: &[u32],
    ) -> Option<SuppressReason> {
        let directives = self.directives.get(file?)?;

        let code = rule.as_code();
        let matching = directives
            .iter()
            .filter(|directive| directive.codes.iter().any(|entry| entry == code));
        if matching.clone().any(|directive| directive.file_level) {
            return Some(SuppressReason::FileLevel);
        }
        // A merged CHK010 covers several import lines; ignoring one of them
        // must not hide the others.
        let ignored = |line: &u32| matching.clone().any(|directive| directive.line == *line);
        (!lines.is_empty() && lines.iter().all(ignored)).then_some(SuppressReason::Inline)
    }
}

fn candidate_lines(candidate: &IssueCandidate) -> Vec<u32> {
    let imports = candidate
        .origins
        .iter()
        .filter_map(|origin| match origin {
            Origin::Import { line, .. } => Some(*line),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !imports.is_empty() {
        // A site without a line takes no inline directive, so neither does
        // the candidate it belongs to.
        return imports
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
    }
    match &candidate.subject {
        IssueSubject::Import { line, .. } => line.iter().copied().collect(),
        IssueSubject::ScriptDistribution { .. } => candidate
            .origins
            .iter()
            .filter_map(|origin| match origin {
                Origin::Manifest(origin) => origin.line,
                _ => None,
            })
            .take(1)
            .collect(),
        _ => Vec::new(),
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
        IssueSubject::ScriptDistribution { script, .. } => Some(script.as_str()),
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
    // Checked before the `path:symbol` split: script targets contain `:`.
    if let IssueSubject::ScriptDistribution { script, name } = subject {
        return is_distribution_rule(rule)
            && (distribution_glob_match(pattern, name)
                || glob_match(
                    &normalize_script_pattern(pattern),
                    &format!("script:{script}:{name}"),
                ));
    }
    if let Some((path_pattern, symbol_pattern)) = pattern.split_once(':') {
        return symbol_pattern_matches(path_pattern, symbol_pattern, subject, file);
    }

    match subject {
        // Each subject kind is only ever emitted by its own rules (File: CHK001,
        // Distribution: dependency rules, Binary: CHK008, Symbol: CHK006/007),
        // so the subject alone decides; only `Import` is shared across rules.
        IssueSubject::File { path } => glob_match(pattern, path),
        IssueSubject::Distribution { name } => distribution_glob_match(pattern, name),
        // The binary name itself stays accepted for backward compatibility.
        IssueSubject::Binary { name } => {
            glob_match(pattern, name)
                || distribution.is_some_and(|dist| distribution_glob_match(pattern, dist))
        },
        // §18 matches dependency rules on the distribution name even when the
        // candidate points at an import site, so a path glob must not match.
        IssueSubject::Import { .. } if is_distribution_rule(rule) => {
            distribution.is_some_and(|name| distribution_glob_match(pattern, name))
        },
        IssueSubject::Import { module, file, .. } => {
            glob_match(pattern, file) || glob_match(pattern, module)
        },
        IssueSubject::Symbol { .. } => symbol_pattern_matches(pattern, "*", subject, file),
        IssueSubject::ScriptDistribution { .. } => false,
    }
}

fn symbol_pattern_matches(
    path_pattern: &str,
    symbol_pattern: &str,
    subject: &IssueSubject,
    file: Option<&str>,
) -> bool {
    let IssueSubject::Symbol { module, name } = subject else {
        return false;
    };
    let module_path = module.replace('.', "/");
    let path_matches = file.is_some_and(|path| glob_match(path_pattern, path))
        || glob_match(path_pattern, &module_path);
    path_matches && glob_match(symbol_pattern, name)
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

/// Match a pattern against a (PEP 503 normalized) distribution name, so
/// that `typing_extensions` or `Boto3` ignores what the manifest declared
/// under that spelling (#1106).
fn distribution_glob_match(pattern: &str, name: &str) -> bool {
    glob_match(&normalize_distribution_pattern(pattern), name)
}

/// PEP 503 normalization of the literal parts of a glob: runs of `-`, `_`
/// and `.` collapse into one `-`, and letters are lowercased. A `[...]`
/// class is copied as written: lowercasing it would turn `[!A-Z]` into its
/// opposite.
fn normalize_distribution_pattern(pattern: &str) -> String {
    let mut normalized = String::with_capacity(pattern.len());
    let mut pending_separator = false;
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        if matches!(ch, '-' | '_' | '.') {
            pending_separator = true;
            continue;
        }
        if pending_separator {
            normalized.push('-');
            pending_separator = false;
        }
        if ch != '[' {
            normalized.push(ch.to_ascii_lowercase());
            continue;
        }
        normalized.push('[');
        // `]` right after `[` or `[!` is a member, not the end.
        let mut leading = true;
        for member in chars.by_ref() {
            normalized.push(member);
            if member == ']' && !leading {
                break;
            }
            leading = leading && member == '!';
        }
    }
    if pending_separator {
        normalized.push('-');
    }
    normalized
}

/// A `script:<path>:<name>` pattern with its name part normalized as a
/// distribution pattern; the path is matched as written.
fn normalize_script_pattern(pattern: &str) -> String {
    pattern.rsplit_once(':').map_or_else(
        || pattern.to_owned(),
        |(target, name)| format!("{target}:{}", normalize_distribution_pattern(name)),
    )
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

    fn symbol_candidate(file: &str) -> IssueCandidate {
        IssueCandidate {
            rule: RuleId::Chk006,
            subject: IssueSubject::Symbol {
                module: "pip._vendor.rich".to_owned(),
                name: "dead".to_owned(),
            },
            severity: Severity::Warning,
            confidence: crate::config::Confidence::Likely,
            message: "unused".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: file.to_owned(),
                line: Some(1),
                module: "pip._vendor.rich".to_owned(),
            }],
            explain: ExplainData::default(),
        }
    }

    #[test]
    fn default_vendored_dirs_suppress_issues_at_any_depth() {
        let matcher = IgnoreMatcher::build(
            &default_config(),
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
        for file in [
            "src/pip/_vendor/rich/console.py",
            "sklearn/externals/array_api_compat/common.py",
            "acme/vendored/six.py",
            "third_party/lib.py",
        ] {
            assert_eq!(
                matcher.matches_candidate(&symbol_candidate(file)),
                Some(SuppressReason::Vendored),
                "{file}"
            );
        }
        assert_eq!(
            matcher.matches_candidate(&symbol_candidate("src/pip/_internal/cli.py")),
            None
        );
    }

    #[test]
    fn empty_vendored_config_reports_vendored_dirs() {
        let mut config = default_config();
        config.vendored.clear();
        let matcher = IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        );
        assert_eq!(
            matcher.matches_candidate(&symbol_candidate("src/pip/_vendor/rich/console.py")),
            None
        );
    }

    const APP: &str = "src/acme/app.py";

    fn import_candidate(rule: RuleId, module: &str, distribution: Option<&str>) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject: IssueSubject::Import {
                module: module.to_owned(),
                file: APP.to_owned(),
                line: Some(1),
                distribution: distribution.map(str::to_owned),
            },
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: APP.to_owned(),
                line: Some(1),
                module: module.to_owned(),
            }],
            explain: ExplainData::default(),
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
            &ResolutionIndex::default(),
        );
        matcher.matches_candidate(&import_candidate(rule, module, Some(distribution)))
            == Some(SuppressReason::Config)
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
    fn config_ignore_without_distribution_leaves_dependency_candidate() {
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
            matcher.matches_candidate(&import_candidate(RuleId::Chk003, "yaml", None)),
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
            ignores: vec![IgnoreDirective {
                file_level: false,
                codes: vec!["CHK003".to_owned()],
                line: 4,
            }],
            ..crate::parser::ParsedModule::default()
        });
        let matcher = IgnoreMatcher::build(&config, &parse, &ResolutionIndex::default());
        let candidate = IssueCandidate {
            rule: RuleId::Chk003,
            subject: IssueSubject::Import {
                module: "missing".to_owned(),
                file: "src/acme/main.py".to_owned(),
                line: Some(4),
                distribution: None,
            },
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: "src/acme/main.py".to_owned(),
                line: Some(4),
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
            ignores: vec![IgnoreDirective {
                file_level: false,
                codes: vec!["CHK006".to_owned()],
                line: 12,
            }],
            ..crate::parser::ParsedModule::default()
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
                line: Some(12),
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
                line: Some(12),
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

    const SCRIPT: &str = "scripts/tool.py";

    fn candidate(rule: RuleId, subject: IssueSubject, origins: Vec<Origin>) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject,
            severity: Severity::Error,
            confidence: crate::config::Confidence::Certain,
            message: "issue".to_owned(),
            workspace_member: None,
            origins,
            explain: ExplainData::default(),
        }
    }

    fn config_matcher(rule: RuleId, patterns: &[&str]) -> IgnoreMatcher {
        let mut config = default_config();
        config.ignore.insert(
            rule.as_code().to_owned(),
            patterns.iter().map(|p| (*p).to_owned()).collect(),
        );
        IgnoreMatcher::build(
            &config,
            &ParseSummary::default(),
            &ResolutionIndex::default(),
        )
    }

    fn directive_matcher(path: &str, directive: IgnoreDirective) -> IgnoreMatcher {
        directives_matcher(path, vec![directive])
    }

    fn directives_matcher(path: &str, ignores: Vec<IgnoreDirective>) -> IgnoreMatcher {
        let mut parse = ParseSummary::default();
        parse.modules.push(crate::parser::ParsedModule {
            path: path.to_owned(),
            ignores,
            ..crate::parser::ParsedModule::default()
        });
        IgnoreMatcher::build(&default_config(), &parse, &ResolutionIndex::default())
    }

    fn file_subject(path: &str) -> IssueSubject {
        IssueSubject::File {
            path: path.to_owned(),
        }
    }

    fn unresolved_import(module: &str) -> IssueSubject {
        IssueSubject::Import {
            module: module.to_owned(),
            file: APP.to_owned(),
            line: Some(7),
            distribution: None,
        }
    }

    fn script_dependency(name: &str) -> IssueSubject {
        IssueSubject::ScriptDistribution {
            script: SCRIPT.to_owned(),
            name: name.to_owned(),
        }
    }

    fn dead_api() -> IssueSubject {
        IssueSubject::Symbol {
            module: "acme.api".to_owned(),
            name: "dead_api".to_owned(),
        }
    }

    fn api_origin() -> Vec<Origin> {
        vec![Origin::Import {
            file: "src/acme/api.py".to_owned(),
            line: Some(12),
            module: "acme.api".to_owned(),
        }]
    }

    fn manifest_origin(line: u32) -> Vec<Origin> {
        vec![Origin::Manifest(crate::manifest::DependencyOrigin {
            file: SCRIPT.to_owned(),
            line: Some(line),
            label: "script dependencies".to_owned(),
        })]
    }

    fn suppressed_by_config(matcher: &IgnoreMatcher, candidate: &IssueCandidate) -> bool {
        matcher.matches_candidate(candidate) == Some(SuppressReason::Config)
    }

    /// §18: file rules take a path glob.
    #[test]
    fn config_ignore_matches_chk001_path_glob() {
        let matcher = config_matcher(RuleId::Chk001, &["src/acme/migrations/**/*.py"]);
        let migration = candidate(
            RuleId::Chk001,
            file_subject("src/acme/migrations/v1/0001_init.py"),
            Vec::new(),
        );
        let app = candidate(RuleId::Chk001, file_subject(APP), Vec::new());

        assert!(suppressed_by_config(&matcher, &migration));
        assert!(!suppressed_by_config(&matcher, &app));
    }

    /// CHK010 has no distribution, so its import-site path or module is the
    /// ignore target, and either one alone is enough.
    #[test]
    fn config_ignore_matches_chk010_by_path_or_module() {
        let unresolved = candidate(RuleId::Chk010, unresolved_import("legacy.api"), Vec::new());

        for pattern in ["src/acme/*.py", "legacy.*"] {
            let matcher = config_matcher(RuleId::Chk010, &[pattern]);
            assert!(suppressed_by_config(&matcher, &unresolved), "{pattern}");
        }
        for pattern in ["tests/**/*.py", "other.*"] {
            let matcher = config_matcher(RuleId::Chk010, &[pattern]);
            assert!(!suppressed_by_config(&matcher, &unresolved), "{pattern}");
        }
    }

    /// PEP 723 script dependencies match on the distribution name or the
    /// `script:<path>:<name>` target, and nothing else.
    #[test]
    fn config_ignore_matches_script_dependency_name_or_target() {
        let unused = candidate(
            RuleId::Chk002,
            script_dependency("requests"),
            manifest_origin(3),
        );

        for pattern in ["requests", "script:scripts/tool.py:requests"] {
            let matcher = config_matcher(RuleId::Chk002, &[pattern]);
            assert!(suppressed_by_config(&matcher, &unused), "{pattern}");
        }
        for pattern in ["boto3", "script:scripts/other.py:requests"] {
            let matcher = config_matcher(RuleId::Chk002, &[pattern]);
            assert!(!suppressed_by_config(&matcher, &unused), "{pattern}");
        }
    }

    /// A dependency-rule pattern is PEP 503 normalized before it meets the
    /// (normalized) distribution name, so the manifest's own spelling works
    /// (#1106); CHK008's binary name is matched as written.
    #[test]
    fn config_ignore_normalizes_distribution_patterns() {
        let unused = |name: &str| {
            candidate(
                RuleId::Chk002,
                IssueSubject::Distribution {
                    name: name.to_owned(),
                },
                Vec::new(),
            )
        };
        for (pattern, name) in [
            ("typing_extensions", "typing-extensions"),
            ("Boto3", "boto3"),
            ("Typing.Extensions", "typing-extensions"),
            ("zope__-interface", "zope-interface"),
            ("Google_Cloud_*", "google-cloud-storage"),
            ("{Boto3,typing_extensions}", "typing-extensions"),
            ("py[!A-Z]aml", "pyyaml"),
        ] {
            let matcher = config_matcher(RuleId::Chk002, &[pattern]);
            assert!(suppressed_by_config(&matcher, &unused(name)), "{pattern}");
        }
        let matcher = config_matcher(RuleId::Chk002, &["typing_extensions_x"]);
        assert!(!suppressed_by_config(
            &matcher,
            &unused("typing-extensions")
        ));

        let script = candidate(
            RuleId::Chk002,
            script_dependency("typing-extensions"),
            manifest_origin(3),
        );
        for pattern in ["Typing_Extensions", "script:scripts/*:Typing_Extensions"] {
            let matcher = config_matcher(RuleId::Chk002, &[pattern]);
            assert!(suppressed_by_config(&matcher, &script), "{pattern}");
        }

        let missing = import_candidate(RuleId::Chk003, "yaml", Some("pyyaml"));
        let matcher = config_matcher(RuleId::Chk003, &["PyYAML"]);
        assert!(suppressed_by_config(&matcher, &missing));
    }

    #[test]
    fn distribution_pattern_normalization_keeps_glob_syntax() {
        for (pattern, normalized) in [
            ("typing_extensions", "typing-extensions"),
            ("A._-B", "a-b"),
            ("foo_*", "foo-*"),
            ("*_", "*-"),
            ("[_.]x", "[_.]x"),
            ("[]_]x", "[]_]x"),
            ("[a_B]x", "[a_B]x"),
            ("[!]A]x_y", "[!]A]x-y"),
            ("py[!A-Z]aml", "py[!A-Z]aml"),
            ("{A_b,C.d}", "{a-b,c-d}"),
        ] {
            assert_eq!(
                normalize_distribution_pattern(pattern),
                normalized,
                "{pattern}"
            );
        }
    }

    /// A symbol-rule pattern without `:` is a path glob covering every symbol.
    #[test]
    fn config_ignore_matches_symbol_path_without_symbol_glob() {
        let matcher = config_matcher(RuleId::Chk006, &["src/acme/api.py"]);
        let unused = candidate(RuleId::Chk006, dead_api(), api_origin());

        assert!(suppressed_by_config(&matcher, &unused));
    }

    /// §18: CHK006 path globs may use wildcards, with or without a symbol glob.
    #[test]
    fn config_ignore_matches_symbol_wildcard_path_globs() {
        let unused = candidate(RuleId::Chk006, dead_api(), api_origin());

        for pattern in ["src/**/*.py", "src/acme/*.py:dead_*"] {
            let matcher = config_matcher(RuleId::Chk006, &[pattern]);
            assert!(suppressed_by_config(&matcher, &unused), "{pattern}");
        }
        for pattern in ["tests/**/*.py", "src/**/*.pyi", "src/acme/*.py:live_*"] {
            let matcher = config_matcher(RuleId::Chk006, &[pattern]);
            assert!(!suppressed_by_config(&matcher, &unused), "{pattern}");
        }
    }

    /// `path:symbol` needs both halves to match.
    #[test]
    fn config_ignore_rejects_symbol_when_name_or_path_differs() {
        let unused = candidate(RuleId::Chk006, dead_api(), api_origin());

        for pattern in ["src/acme/api.py:public_*", "src/acme/other.py:dead_*"] {
            let matcher = config_matcher(RuleId::Chk006, &[pattern]);
            assert!(!suppressed_by_config(&matcher, &unused), "{pattern}");
        }
    }

    /// A CHK010 merged over several import lines is silenced only when every
    /// line carries the directive (#585).
    #[test]
    fn inline_ignore_needs_every_merged_import_line() {
        let directive = |line| IgnoreDirective {
            file_level: false,
            codes: vec!["CHK010".to_owned()],
            line,
        };
        let origin = |line| Origin::Import {
            file: APP.to_owned(),
            line: Some(line),
            module: "legacy".to_owned(),
        };
        let unresolved = candidate(
            RuleId::Chk010,
            unresolved_import("legacy"),
            vec![origin(7), origin(12)],
        );

        assert_eq!(
            directive_matcher(APP, directive(7)).matches_candidate(&unresolved),
            None
        );
        assert_eq!(
            directives_matcher(APP, vec![directive(7), directive(12)])
                .matches_candidate(&unresolved),
            Some(SuppressReason::Inline)
        );
    }

    /// Without an import origin, the inline line comes from the import subject.
    #[test]
    fn inline_ignore_uses_import_subject_line_without_origins() {
        let matcher = directive_matcher(
            APP,
            IgnoreDirective {
                file_level: false,
                codes: vec!["CHK010".to_owned()],
                line: 7,
            },
        );
        let unresolved = candidate(RuleId::Chk010, unresolved_import("legacy"), Vec::new());

        assert_eq!(
            matcher.matches_candidate(&unresolved),
            Some(SuppressReason::Inline)
        );
    }

    /// An unused script dependency is silenced on its declaration line inside
    /// the `# /// script` block.
    #[test]
    fn inline_ignore_uses_script_dependency_manifest_line() {
        let matcher = directive_matcher(
            SCRIPT,
            IgnoreDirective {
                file_level: false,
                codes: vec!["CHK002".to_owned()],
                line: 3,
            },
        );
        let on_line = candidate(
            RuleId::Chk002,
            script_dependency("requests"),
            manifest_origin(3),
        );
        let other_line = candidate(
            RuleId::Chk002,
            script_dependency("requests"),
            manifest_origin(4),
        );

        assert_eq!(
            matcher.matches_candidate(&on_line),
            Some(SuppressReason::Inline)
        );
        assert_eq!(matcher.matches_candidate(&other_line), None);
    }

    /// Without origins, the file-level directive is looked up in the file the
    /// subject itself names.
    #[test]
    fn file_level_ignore_uses_subject_file_without_origins() {
        let cases = [
            (RuleId::Chk001, file_subject(APP), APP),
            (RuleId::Chk010, unresolved_import("legacy"), APP),
            (RuleId::Chk002, script_dependency("requests"), SCRIPT),
        ];
        for (rule, subject, path) in cases {
            let matcher = directive_matcher(
                path,
                IgnoreDirective {
                    file_level: true,
                    codes: vec![rule.as_code().to_owned()],
                    line: 1,
                },
            );
            assert_eq!(
                matcher.matches_candidate(&candidate(rule, subject, Vec::new())),
                Some(SuppressReason::FileLevel),
                "{rule:?}"
            );
        }
    }

    /// I1 of `docs/dev/formal/ignore_model.py` over generated names and
    /// patterns, plus vendored suppression taking precedence.
    mod props {
        use proptest::prelude::*;

        use super::*;

        /// Reference glob for patterns made of literals and `*` (which, as in
        /// `globset`'s default, also crosses `/`).
        fn reference_glob(pattern: &str, value: &str) -> bool {
            match pattern.split_once('*') {
                None => pattern == value,
                Some((head, rest)) => value.strip_prefix(head).is_some_and(|value| {
                    (0..=value.len())
                        .filter(|cut| value.is_char_boundary(*cut))
                        .any(|cut| reference_glob(rest, &value[cut..]))
                }),
            }
        }

        #[derive(Debug, Clone)]
        enum Kind {
            Distribution(RuleId),
            Import(RuleId, Option<String>),
            Binary(Option<String>),
        }

        #[derive(Debug, Clone)]
        struct Case {
            kind: Kind,
            name: String,
            module: String,
            file: String,
        }

        fn name() -> impl Strategy<Value = String> {
            "[ab]{1,2}(-[ab]{1,2})?"
        }

        fn case_strategy() -> impl Strategy<Value = Case> {
            let kind = prop_oneof![
                prop::sample::select(vec![RuleId::Chk002, RuleId::Chk005, RuleId::Chk009])
                    .prop_map(Kind::Distribution),
                (
                    prop::sample::select(vec![RuleId::Chk003, RuleId::Chk004]),
                    prop::option::of(name())
                )
                    .prop_map(|(rule, dist)| Kind::Import(rule, dist)),
                prop::option::of(name()).prop_map(Kind::Binary),
            ];
            (
                kind,
                name(),
                prop::sample::select(vec!["a", "b", "_vendor", "x/vendored"]),
                "[ab]{1,2}",
            )
                .prop_map(|(kind, name, dir, stem)| Case {
                    kind,
                    module: name.replace('-', "_"),
                    file: format!("src/{dir}/{stem}.py"),
                    name,
                })
        }

        /// Patterns drawn from the case's own names, module and path, so
        /// that near misses (module vs distribution, path globs) are common.
        fn pattern_strategy(case: &Case) -> impl Strategy<Value = String> + use<> {
            let dist = match &case.kind {
                Kind::Import(_, dist) | Kind::Binary(dist) => dist.clone(),
                Kind::Distribution(_) => None,
            };
            let mut seeds = vec![case.name.clone(), case.module.clone(), case.file.clone()];
            seeds.extend(dist);
            (
                prop::sample::select(seeds),
                0usize..4,
                prop::option::of("[ab]"),
            )
                .prop_map(|(seed, shape, symbol)| {
                    let base = match shape {
                        0 => seed,
                        1 => format!("{}*", &seed[..seed.len() / 2]),
                        2 => format!("*{}", &seed[seed.len() / 2..]),
                        _ => "*".to_owned(),
                    };
                    match symbol {
                        Some(symbol) => format!("{base}:{symbol}"),
                        None => base,
                    }
                })
        }

        fn build(case: &Case, pattern: &str) -> (IgnoreMatcher, IssueCandidate) {
            let (rule, subject, binary_dist) = match &case.kind {
                Kind::Distribution(rule) => (
                    *rule,
                    IssueSubject::Distribution {
                        name: case.name.clone(),
                    },
                    None,
                ),
                Kind::Import(rule, dist) => (
                    *rule,
                    IssueSubject::Import {
                        module: case.module.clone(),
                        file: case.file.clone(),
                        line: Some(1),
                        distribution: dist.clone(),
                    },
                    None,
                ),
                Kind::Binary(dist) => (
                    RuleId::Chk008,
                    IssueSubject::Binary {
                        name: case.name.clone(),
                    },
                    dist.clone(),
                ),
            };
            let mut config = default_config();
            config
                .ignore
                .insert(rule.as_code().to_owned(), vec![pattern.to_owned()]);
            let mut resolution = ResolutionIndex::default();
            if let Some(dist) = binary_dist {
                resolution
                    .binary_resolutions
                    .insert(case.name.clone(), dist);
            }
            let matcher = IgnoreMatcher::build(&config, &ParseSummary::default(), &resolution);
            (matcher, candidate(rule, subject, Vec::new()))
        }

        /// The pattern as someone might have spelled it in the manifest:
        /// letters in either case, any separator for another (#1106).
        fn respell(pattern: &str, choices: &[u8]) -> String {
            pattern
                .chars()
                .zip(choices.iter().copied().chain(std::iter::repeat(0)))
                .map(|(ch, choice)| match ch {
                    '-' | '_' | '.' if !pattern.contains('/') => {
                        ['-', '_', '.'][usize::from(choice % 3)]
                    },
                    'a' | 'b' if choice % 2 == 1 => ch.to_ascii_uppercase(),
                    _ => ch,
                })
                .collect()
        }

        fn expected(case: &Case, pattern: &str) -> Option<SuppressReason> {
            let normalized = crate::manifest::normalize_distribution_name(pattern);
            let glob = |value: &str| reference_glob(&normalized, value);
            let vendored = case.file.contains("/_vendor/") || case.file.contains("/vendored/");
            match &case.kind {
                Kind::Import(..) if vendored => Some(SuppressReason::Vendored),
                Kind::Distribution(_) => glob(&case.name).then_some(SuppressReason::Config),
                Kind::Import(_, dist) => dist
                    .as_deref()
                    .is_some_and(glob)
                    .then_some(SuppressReason::Config),
                Kind::Binary(dist) => (reference_glob(pattern, &case.name)
                    || dist.as_deref().is_some_and(glob))
                .then_some(SuppressReason::Config),
            }
        }

        proptest! {
            /// Dependency rules match only the distribution (and, for
            /// CHK008, the binary name), never a module or path, whatever the
            /// pattern, however it spells the name; vendored files are
            /// suppressed before any of that.
            #[test]
            fn dependency_ignores_match_only_distribution_names(
                (case, pattern) in case_strategy().prop_flat_map(|case| {
                    let pattern = pattern_strategy(&case);
                    (Just(case), pattern)
                }),
                choices in prop::collection::vec(any::<u8>(), 0..16),
            ) {
                let pattern = respell(&pattern, &choices);
                let (matcher, candidate) = build(&case, &pattern);
                prop_assert_eq!(matcher.matches_candidate(&candidate), expected(&case, &pattern));
            }

            #[test]
            fn reference_glob_agrees_with_globset(pattern in "[ab*]{0,5}", value in "[ab]{0,5}") {
                prop_assert_eq!(glob_match(&pattern, &value), reference_glob(&pattern, &value));
            }
        }
    }
}
