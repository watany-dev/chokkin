//! CHK002/CHK003 inside PEP 723 scripts, checked against the script block.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::used::{sorted_paths, within};
use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::{InlineScript, normalize_distribution_name};
use crate::parser::{ParseSummary, ParsedModule};
use crate::resolver::{ImportMap, ResolutionIndex, ResolvedImport};
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity, import_evidence,
    imported_in,
};
use crate::sources::DiscoveredFile;

/// Third-party imports made directly by a script file; project reconciliation
/// must not see them.
pub(super) fn is_script_third_party(import: &ResolvedImport, script_paths: &HashSet<&str>) -> bool {
    import.origin == ModuleOrigin::ThirdParty && script_paths.contains(import.file.as_str())
}

/// Per-script CHK002 and CHK003 candidates for reachable scripts.
#[allow(clippy::too_many_arguments)]
pub(super) fn detect_script_dependency_issues(
    scripts: &[InlineScript],
    resolution: &ResolutionIndex,
    reachable: &HashSet<&str>,
    files: &[DiscoveredFile],
    parse: &ParseSummary,
    import_map: &ImportMap,
    strict: bool,
) -> Vec<IssueCandidate> {
    let scripts: Vec<&InlineScript> = scripts
        .iter()
        .filter(|script| reachable.contains(script.path.as_str()))
        .collect();
    if scripts.is_empty() {
        return Vec::new();
    }
    let index = ScriptIndex::new(resolution, files, parse);
    let mut candidates = Vec::new();
    for script in scripts {
        let declared: BTreeSet<String> = script
            .dependencies
            .iter()
            .filter(|dep| !dep.opaque)
            .map(|dep| normalize_distribution_name(&dep.name))
            .collect();
        let mut local_files = HashSet::new();
        let mut imports = Vec::new();
        for &import in index.imports_of(&script.path) {
            let mut found = false;
            index.local_files(&script.path, &import.import_root, |file| {
                found = true;
                local_files.insert(file);
            });
            if !found {
                imports.push(import);
            }
        }
        // The block also installs what the helper modules next to the script
        // import, so their imports count as uses of the script's dependencies.
        let parsed = index.modules.get(script.path.as_str()).copied();
        let mut used: BTreeSet<String> = local_files
            .iter()
            .flat_map(|file| index.imports_of(file))
            .copied()
            .chain(imports.iter().copied())
            .filter_map(|import| used_distribution(import, &declared, import_map))
            .collect();
        imports.retain(|import| {
            import.origin == ModuleOrigin::ThirdParty && import.distribution.is_some()
        });
        candidates.extend(missing_in_script(script, &imports, &declared, import_map));
        // A tool the script runs as a command (`ruff format …`) is used
        // without being imported.
        used.extend(
            parsed
                .iter()
                .flat_map(|module| &module.shell_commands)
                .map(String::as_str)
                .map(normalize_distribution_name)
                .filter(|name| declared.contains(name)),
        );
        // A file run with `sys.executable` shares the block's environment, and
        // its imports cannot be followed.
        if !parsed.is_some_and(|module| module.runs_python_file) {
            candidates.extend(unused_in_script(script, &used, strict));
        }
    }
    candidates
}

/// Lookups the per-script checks share, built once instead of rescanning every
/// import, file, and module for each script (#945).
struct ScriptIndex<'a> {
    /// Imports by importing file, in resolution order.
    imports: HashMap<&'a str, Vec<&'a ResolvedImport>>,
    /// Inventory paths, sorted so a package's files form one range.
    paths: Vec<&'a str>,
    /// The first parsed module of each script.
    modules: HashMap<&'a str, &'a ParsedModule>,
}

impl<'a> ScriptIndex<'a> {
    fn new(
        resolution: &'a ResolutionIndex,
        files: &'a [DiscoveredFile],
        parse: &'a ParseSummary,
    ) -> Self {
        let mut imports: HashMap<&str, Vec<&ResolvedImport>> = HashMap::new();
        for import in &resolution.imports {
            imports
                .entry(import.file.as_str())
                .or_default()
                .push(import);
        }
        let paths = sorted_paths(files);
        // Reversed so the first module of a path wins.
        let modules = parse
            .modules
            .iter()
            .rev()
            .map(|module| (module.path.as_str(), module))
            .collect();
        Self {
            imports,
            paths,
            modules,
        }
    }

    fn imports_of(&self, file: &str) -> &[&'a ResolvedImport] {
        self.imports.get(file).map_or(&[], Vec::as_slice)
    }

    /// Calls `found` with each file of the module or package `import_root`
    /// next to `script`: the script runs with its own directory first on
    /// `sys.path`, so they shadow any distribution of the same name.
    fn local_files(&self, script: &str, import_root: &str, mut found: impl FnMut(&'a str)) {
        let dir = script.rsplit_once('/').map_or("", |(dir, _)| dir);
        let prefix = if dir.is_empty() {
            import_root.to_owned()
        } else {
            format!("{dir}/{import_root}")
        };
        for suffix in [".py", ".pyi"] {
            let module = format!("{prefix}{suffix}");
            if let Ok(index) = self.paths.binary_search(&module.as_str()) {
                found(self.paths[index]);
            }
        }
        for path in within(&self.paths, |path| path, &prefix) {
            found(path);
        }
    }
}

/// The declared distribution an import uses. Besides the resolved
/// distribution, any distribution the map gives for the root counts
/// (`pydantic_ai` from `pydantic-ai-slim`), and so does an unknown or
/// first-party root the block declares by name (a script shipped with the
/// project it imports, installed from the index by `uv run`). The project's own
/// `github` package does not shadow `PyGithub` either, since `uv run` executes
/// the script in its own environment.
fn used_distribution(
    import: &ResolvedImport,
    declared: &BTreeSet<String>,
    import_map: &ImportMap,
) -> Option<String> {
    if import.origin == ModuleOrigin::Stdlib {
        return None;
    }
    let by_name = (import.origin != ModuleOrigin::ThirdParty).then(|| import.import_root.clone());
    let mapped = import_map
        .candidates(&import.import_root)
        .map(|(distributions, _)| distributions)
        .unwrap_or_default();
    import
        .distribution
        .iter()
        .cloned()
        .chain(by_name)
        .chain(mapped)
        .map(|name| normalize_distribution_name(&name))
        .find(|name| declared.contains(name))
}

fn import_distribution(import: &ResolvedImport) -> Option<String> {
    import
        .distribution
        .as_deref()
        .map(normalize_distribution_name)
}

fn missing_in_script(
    script: &InlineScript,
    imports: &[&ResolvedImport],
    declared: &BTreeSet<String>,
    import_map: &ImportMap,
) -> Vec<IssueCandidate> {
    let mut reported = BTreeSet::new();
    let mut candidates = Vec::new();
    for import in imports
        .iter()
        .filter(|import| !import.optional && !import.platform_guarded)
    {
        let Some(name) = import_distribution(import) else {
            continue;
        };
        if used_distribution(import, declared, import_map).is_some()
            || !reported.insert(name.clone())
        {
            continue;
        }
        candidates.push(IssueCandidate {
            rule: RuleId::Chk003,
            subject: IssueSubject::ScriptDistribution {
                script: script.path.clone(),
                name: name.clone(),
            },
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: format!(
                "{} but the PEP 723 script block does not declare it",
                imported_in(&name, import)
            ),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: import.file.clone(),
                line: import.line,
                module: import.full_module.clone(),
            }],
            explain: ExplainData {
                summary: format!("{name} is missing from the `# /// script` dependencies"),
                details: vec![
                    import_evidence(import),
                    "script imports are checked against the script block, not the project manifest"
                        .to_owned(),
                ],
            },
        });
    }
    candidates
}

fn unused_in_script(
    script: &InlineScript,
    used: &BTreeSet<String>,
    strict: bool,
) -> Vec<IssueCandidate> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for dep in &script.dependencies {
        let name = normalize_distribution_name(&dep.name);
        if dep.opaque
            || (!strict && dep.marker.is_some())
            || used.contains(&name)
            || !seen.insert(name.clone())
        {
            continue;
        }
        // The block declares the environment `uv run` builds, which lists
        // runtime peers the script never imports (marimo's dataframe
        // backends, transformers' `torch`), so no import is not proof (#984).
        candidates.push(IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::ScriptDistribution {
                script: script.path.clone(),
                name: name.clone(),
            },
            severity: if strict {
                Severity::Error
            } else {
                Severity::Warning
            },
            confidence: Confidence::Likely,
            message: format!(
                "{name} is declared in the PEP 723 block of {}, but no import in the script uses it",
                script.path
            ),
            workspace_member: None,
            origins: vec![Origin::Manifest(dep.origin.clone())],
            explain: ExplainData {
                summary: format!("{name} is declared by script {} but not used", script.path),
                details: vec![format!("declaration: {}", dep.origin.label)],
            },
        });
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
    use crate::parser::ImportContext;
    use crate::resolver::ResolveConfidence;
    use crate::sources::{FileContext, FileKind};

    fn import_map() -> ImportMap {
        ImportMap::build(&crate::config::default_config())
    }

    fn dependency(name: &str, line: u32) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "scripts/tool.py".to_owned(),
                line: Some(line),
                label: "script.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
        }
    }

    fn import(module: &str, distribution: &str, line: u32) -> ResolvedImport {
        ResolvedImport {
            import_root: module.to_owned(),
            full_module: module.to_owned(),
            file: "scripts/tool.py".to_owned(),
            workspace_member: None,
            line: Some(line),
            reference: None,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution.to_owned()),
            confidence: ResolveConfidence::Certain,
        }
    }

    #[test]
    fn reports_missing_and_unused_script_dependencies() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("rich", 3), dependency("httpx", 4)],
            requires_python: None,
            target_version: None,
        };
        let resolution = ResolutionIndex {
            imports: vec![import("rich", "rich", 7), import("yaml", "pyyaml", 8)],
            ..ResolutionIndex::default()
        };
        let reachable = HashSet::from(["scripts/tool.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script],
            &resolution,
            &reachable,
            &[],
            &ParseSummary::default(),
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| (candidate.rule, candidate.subject))
        .collect();
        let subject = |name: &str| IssueSubject::ScriptDistribution {
            script: "scripts/tool.py".to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(
            found,
            [
                (RuleId::Chk003, subject("pyyaml")),
                (RuleId::Chk002, subject("httpx")),
            ]
        );
    }

    fn detect(
        dependencies: Vec<DeclaredDependency>,
        imports: Vec<ResolvedImport>,
        strict: bool,
    ) -> Vec<(RuleId, Severity, Confidence, String)> {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies,
            requires_python: None,
            target_version: None,
        };
        let resolution = ResolutionIndex {
            imports,
            ..ResolutionIndex::default()
        };
        detect_script_dependency_issues(
            &[script],
            &resolution,
            &HashSet::from(["scripts/tool.py"]),
            &[],
            &ParseSummary::default(),
            &import_map(),
            strict,
        )
        .into_iter()
        .map(|candidate| {
            let IssueSubject::ScriptDistribution { name, .. } = candidate.subject else {
                panic!("unexpected subject {:?}", candidate.subject);
            };
            (
                candidate.rule,
                candidate.severity,
                candidate.confidence,
                name,
            )
        })
        .collect()
    }

    /// A first-party import can still carry a distribution (`airflow` maps to
    /// `apache-airflow`); it is not a script dependency.
    #[test]
    fn only_unconditional_third_party_imports_are_missing() {
        let imports = vec![
            ResolvedImport {
                origin: ModuleOrigin::FirstParty,
                ..import("airflow", "apache-airflow", 2)
            },
            ResolvedImport {
                optional: true,
                ..import("yaml", "pyyaml", 3)
            },
            ResolvedImport {
                platform_guarded: true,
                ..import("winreg_ext", "winreg-ext", 4)
            },
        ];
        assert_eq!(detect(Vec::new(), imports, false), []);
    }

    #[test]
    fn marker_dependencies_are_unused_only_in_strict_mode() {
        let marked = || DeclaredDependency {
            marker: Some("sys_platform == 'win32'".to_owned()),
            ..dependency("pywin32", 3)
        };
        let opaque = || DeclaredDependency {
            opaque: true,
            ..dependency("git-dep", 4)
        };
        assert_eq!(detect(vec![marked(), opaque()], Vec::new(), false), []);
        assert_eq!(
            detect(vec![marked(), opaque()], Vec::new(), true),
            [(
                RuleId::Chk002,
                Severity::Error,
                Confidence::Likely,
                "pywin32".to_owned()
            )]
        );
    }

    #[test]
    fn scripts_running_another_python_file_report_no_unused() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("httpx", 3)],
            requires_python: None,
            target_version: None,
        };
        let resolution = ResolutionIndex {
            imports: vec![import("yaml", "pyyaml", 8)],
            ..ResolutionIndex::default()
        };
        let parse = ParseSummary {
            modules: vec![crate::parser::ParsedModule {
                path: "scripts/tool.py".to_owned(),
                runs_python_file: true,
                ..crate::parser::ParsedModule::default()
            }],
        };
        let reachable = HashSet::from(["scripts/tool.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script],
            &resolution,
            &reachable,
            &[],
            &parse,
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| candidate.rule)
        .collect();
        assert_eq!(found, [RuleId::Chk003]);
    }

    #[test]
    fn tools_run_as_commands_are_used() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("ruff", 3), dependency("httpx", 4)],
            requires_python: None,
            target_version: None,
        };
        let parse = ParseSummary {
            modules: vec![crate::parser::ParsedModule {
                path: "scripts/tool.py".to_owned(),
                shell_commands: vec!["Ruff".to_owned(), "grep".to_owned()],
                ..crate::parser::ParsedModule::default()
            }],
        };
        let reachable = HashSet::from(["scripts/tool.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script],
            &ResolutionIndex::default(),
            &reachable,
            &[],
            &parse,
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| candidate.subject)
        .collect();
        assert_eq!(
            found,
            [IssueSubject::ScriptDistribution {
                script: "scripts/tool.py".to_owned(),
                name: "httpx".to_owned(),
            }]
        );
    }

    #[test]
    fn unreachable_scripts_are_not_checked() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("httpx", 3)],
            requires_python: None,
            target_version: None,
        };
        let found = detect_script_dependency_issues(
            &[script],
            &ResolutionIndex::default(),
            &HashSet::new(),
            &[],
            &ParseSummary::default(),
            &import_map(),
            false,
        );
        assert_eq!(found, []);
    }

    #[test]
    fn modules_next_to_the_script_are_not_missing() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: Vec::new(),
            requires_python: None,
            target_version: None,
        };
        let resolution = ResolutionIndex {
            imports: vec![
                import("common_utils", "common-utils", 5),
                import("helpers", "helpers", 6),
                import("common", "common", 7),
            ],
            ..ResolutionIndex::default()
        };
        let files: Vec<DiscoveredFile> = [
            "scripts/tool.py",
            "scripts/common_utils.py",
            "scripts/helpers/__init__.py",
            "common.py",
        ]
        .into_iter()
        .map(|path| DiscoveredFile {
            path: path.to_owned(),
            kind: FileKind::Python,
            context: FileContext::Dev,
        })
        .collect();
        let reachable = HashSet::from(["scripts/tool.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script],
            &resolution,
            &reachable,
            &files,
            &ParseSummary::default(),
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| candidate.subject)
        .collect();
        assert_eq!(
            found,
            [IssueSubject::ScriptDistribution {
                script: "scripts/tool.py".to_owned(),
                name: "common".to_owned(),
            }]
        );
    }

    #[test]
    fn helper_module_and_unmapped_imports_use_script_dependencies() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![
                dependency("rich", 3),
                dependency("termcolor", 4),
                dependency("httpx", 5),
                dependency("fastmcp", 6),
                dependency("PyGithub", 7),
                dependency("pydantic-ai-slim", 8),
            ],
            requires_python: None,
            target_version: None,
        };
        let helper_import = ResolvedImport {
            file: "scripts/common_utils.py".to_owned(),
            ..import("rich", "rich", 2)
        };
        let unmapped = ResolvedImport {
            origin: ModuleOrigin::Unknown,
            distribution: None,
            ..import("termcolor", "termcolor", 8)
        };
        let own_project = ResolvedImport {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            ..import("fastmcp", "fastmcp", 9)
        };
        let project_package = ResolvedImport {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            ..import("github", "github", 10)
        };
        let resolution = ResolutionIndex {
            imports: vec![
                import("pydantic_ai", "pydantic-ai", 11),
                import("common_utils", "common-utils", 7),
                unmapped,
                own_project,
                project_package,
                helper_import,
            ],
            ..ResolutionIndex::default()
        };
        let files: Vec<DiscoveredFile> = ["scripts/tool.py", "scripts/common_utils.py"]
            .into_iter()
            .map(|path| DiscoveredFile {
                path: path.to_owned(),
                kind: FileKind::Python,
                context: FileContext::Dev,
            })
            .collect();
        let reachable = HashSet::from(["scripts/tool.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script],
            &resolution,
            &reachable,
            &files,
            &ParseSummary::default(),
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| (candidate.rule, candidate.subject))
        .collect();
        assert_eq!(
            found,
            [(
                RuleId::Chk002,
                IssueSubject::ScriptDistribution {
                    script: "scripts/tool.py".to_owned(),
                    name: "httpx".to_owned(),
                }
            )]
        );
    }

    /// Local files are found by exact module path or package prefix, never by a
    /// longer name sharing the prefix, whatever order the inventory is in.
    #[test]
    fn local_files_match_whole_names_in_any_inventory_order() {
        let script = |path: &str| InlineScript {
            path: path.to_owned(),
            dependencies: vec![dependency("rich", 3), dependency("httpx", 4)],
            requires_python: None,
            target_version: None,
        };
        let at = |file: &str, module: &str, distribution: &str| ResolvedImport {
            file: file.to_owned(),
            ..import(module, distribution, 1)
        };
        let resolution = ResolutionIndex {
            imports: vec![
                at("tool.py", "stubs", "stubs"),
                at("tool.py", "helpers", "helpers"),
                at("tool.py", "util", "util"),
                at("helpers/deep/inner.py", "rich", "rich"),
                at("helpers_extra.py", "httpx", "httpx"),
                at("scripts/run.py", "helpers", "helpers"),
                at("scripts/run.py", "rich", "rich"),
            ],
            ..ResolutionIndex::default()
        };
        let files: Vec<DiscoveredFile> = [
            "utility.py",
            "scripts/run.py",
            "helpers_extra.py",
            "stubs.pyi",
            "tool.py",
            "helpers/deep/inner.py",
            "helpersx/__init__.py",
        ]
        .into_iter()
        .map(|path| DiscoveredFile {
            path: path.to_owned(),
            kind: FileKind::Python,
            context: FileContext::Dev,
        })
        .collect();
        let reachable = HashSet::from(["tool.py", "scripts/run.py"]);
        let found: Vec<_> = detect_script_dependency_issues(
            &[script("tool.py"), script("scripts/run.py")],
            &resolution,
            &reachable,
            &files,
            &ParseSummary::default(),
            &import_map(),
            false,
        )
        .into_iter()
        .map(|candidate| (candidate.rule, candidate.subject))
        .collect();
        let subject = |script: &str, name: &str| IssueSubject::ScriptDistribution {
            script: script.to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(
            found,
            [
                (RuleId::Chk003, subject("tool.py", "util")),
                (RuleId::Chk002, subject("tool.py", "httpx")),
                (RuleId::Chk003, subject("scripts/run.py", "helpers")),
                (RuleId::Chk002, subject("scripts/run.py", "httpx")),
            ]
        );
    }
}
