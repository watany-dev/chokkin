//! CHK008 unlisted binary dependency detection.

use std::collections::BTreeSet;

use rustc_hash::FxHashSet as HashSet;

use crate::config::Confidence;
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::plugins::{BinaryUsage, PluginHints};
use crate::resolver::ResolutionIndex;
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity,
    WorkspaceDependencyBoundary,
};

use super::used::DeclaredIndex;

/// Wrappers that install a binary's distribution as their own dependency, so
/// declaring the wrapper provides the binary even without a lockfile (#680).
const WRAPPER_PROVIDERS: &[(&str, &str)] = &[
    // The pre-1.0 distribution ships the same `dg` script.
    ("dagster-dg", "dagster-dg-cli"),
    // dbt adapters depend on dbt-core, which ships `dbt`; an adapter missing
    // here still reports CHK008 for `dbt` (#1057).
    ("dbt-athena", "dbt-core"),
    ("dbt-athena-community", "dbt-core"),
    ("dbt-bigquery", "dbt-core"),
    ("dbt-clickhouse", "dbt-core"),
    ("dbt-databricks", "dbt-core"),
    ("dbt-duckdb", "dbt-core"),
    ("dbt-fabric", "dbt-core"),
    ("dbt-postgres", "dbt-core"),
    ("dbt-redshift", "dbt-core"),
    ("dbt-snowflake", "dbt-core"),
    ("dbt-spark", "dbt-core"),
    ("dbt-sqlserver", "dbt-core"),
    ("dbt-trino", "dbt-core"),
    ("mkdocs-material", "mkdocs"),
    ("pre-commit-uv", "pre-commit"),
    // prek is a drop-in pre-commit that reads `.pre-commit-config.yaml` (#723).
    ("prek", "pre-commit"),
    ("pytest-cov", "coverage"),
    ("tox-uv", "tox"),
];

/// Distributions and binaries that make a binary usage provided (#680).
pub(super) struct BinaryProviders<'a> {
    /// Declared by the root, a workspace member, a requirements file outside
    /// the fixed names, tox `deps` or nox `session.install(...)`, or pulled in
    /// directly by one of those.
    distributions: BTreeSet<&'a str>,
    /// The project's own distributions (root and workspace members): installing
    /// the project installs its scripts, so a repo running its own CLI (black
    /// formatting itself) provides it (#1241).
    own_distributions: Vec<String>,
    /// Installed by remote pre-commit hooks and local hooks with their own
    /// `language` env; they cover config-section usages.
    hook_binaries: HashSet<&'a str>,
    /// `prek` runs somewhere, so it, not `pre-commit`, runs the hooks of
    /// `.pre-commit-config.yaml` (pdm's `prek run`, #1241).
    prek_runs_hooks: bool,
}

impl<'a> BinaryProviders<'a> {
    pub(super) fn new(
        declared: &'a DeclaredIndex<'_>,
        manifest: &'a LoadedManifest,
        workspace_boundaries: &[WorkspaceDependencyBoundary<'a>],
        plugins: &'a PluginHints,
    ) -> Self {
        let mut distributions: BTreeSet<&'a str> = declared
            .keys()
            .map(String::as_str)
            .chain(
                manifest
                    .sources
                    .extra_requirements
                    .iter()
                    .map(String::as_str),
            )
            .chain(
                plugins
                    .config_declared_distributions
                    .iter()
                    .map(String::as_str),
            )
            .collect();
        for boundary in workspace_boundaries {
            let member = boundary.manifest;
            distributions.extend(member.dependencies.iter().map(|dep| dep.name.as_str()));
            distributions.extend(member.sources.extra_requirements.iter().map(String::as_str));
        }
        let transitive: Vec<&'a str> = distributions
            .iter()
            .flat_map(|name| manifest.lockfile.edges.get(*name).into_iter().flatten())
            .map(String::as_str)
            .chain(
                WRAPPER_PROVIDERS
                    .iter()
                    .filter(|(wrapper, _)| distributions.contains(wrapper))
                    .map(|(_, provided)| *provided),
            )
            .collect();
        distributions.extend(transitive);
        let own_distributions = std::iter::once(manifest)
            .chain(
                workspace_boundaries
                    .iter()
                    .map(|boundary| boundary.manifest),
            )
            .filter_map(|manifest| manifest.metadata.name.as_deref())
            .map(normalize_distribution_name)
            .collect();
        Self {
            distributions,
            own_distributions,
            hook_binaries: plugins
                .config_provided_binaries
                .iter()
                .map(String::as_str)
                .collect(),
            prek_runs_hooks: plugins
                .all_binary_usages()
                .any(|usage| usage.binary == "prek"),
        }
    }

    fn provides(&self, usage: &BinaryUsage, distribution: &str) -> bool {
        self.distributions.contains(distribution)
            || self.own_distributions.iter().any(|own| own == distribution)
            || (usage.config_section && self.hook_binaries.contains(usage.binary.as_str()))
            || (self.prek_runs_hooks
                && usage.binary == "pre-commit"
                && usage.origin.file == ".pre-commit-config.yaml")
    }
}

/// Detect CLI binaries used in config but not declared as dependencies.
pub(super) fn detect_unlisted_binaries(
    providers: &BinaryProviders<'_>,
    resolution: &ResolutionIndex,
    plugins: &PluginHints,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut reported = HashSet::default();
    let unprovided: Vec<(&BinaryUsage, &String)> = plugins
        .all_binary_usages()
        .filter(|usage| !usage.inferred)
        .filter_map(|usage| {
            let distribution = resolution.binary_resolutions.get(&usage.binary)?;
            (!providers.provides(usage, distribution)).then_some((usage, distribution))
        })
        .collect();
    // A leftover config section is weak evidence that the tool runs at all
    // (transformers' `[tool.coverage]`), so only a command makes the finding
    // certain; the command's origin is the one reported (#1260).
    let run_by_command: HashSet<&str> = unprovided
        .iter()
        .filter(|(usage, _)| !usage.config_section)
        .map(|(_, distribution)| distribution.as_str())
        .collect();

    for (usage, distribution) in unprovided {
        let run = run_by_command.contains(distribution.as_str());
        if run && usage.config_section {
            continue;
        }
        if !reported.insert(distribution.clone()) {
            continue;
        }
        let origin_detail = match usage.origin.line {
            Some(line) => format!("binary usage in {}:{line}", usage.origin.file),
            None => format!("binary usage in {}", usage.origin.file),
        };
        let mut details = vec![origin_detail];
        if !run {
            details.push("only a tool config section uses it; no command runs it".to_owned());
        }

        candidates.push(IssueCandidate {
            rule: RuleId::Chk008,
            subject: IssueSubject::Binary {
                name: usage.binary.clone(),
            },
            severity: Severity::Warning,
            confidence: if run {
                Confidence::Certain
            } else {
                Confidence::Likely
            },
            message: format!(
                "binary {} resolves to {distribution} but it is not declared in the manifest",
                usage.binary
            ),
            workspace_member: None,
            origins: vec![Origin::Binary(usage.origin.clone())],
            explain: ExplainData {
                summary: format!(
                    "{} requires declared dependency {distribution}",
                    usage.binary
                ),
                details,
            },
        });
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        DeclaredDependency, DependencyContext, DependencyOrigin, LockfileGraph, ManifestSources,
        ProjectMetadata,
    };
    use crate::plugins::ReferenceOrigin;
    use crate::rules::deps::used::build_declared_index;

    fn manifest(declared: &[&str]) -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: declared
                .iter()
                .map(|name| DeclaredDependency {
                    name: (*name).to_owned(),
                    extras: Vec::new(),
                    marker: None,
                    specifier: None,
                    context: DependencyContext::Group("dev".to_owned()),
                    origin: DependencyOrigin {
                        file: "pyproject.toml".to_owned(),
                        line: Some(1),
                        label: "dependency-groups.dev[0]".to_owned(),
                    },
                    opaque: false,
                    included_via: Vec::new(),
                    unportable_key: None,
                })
                .collect(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn usage(binary: &str, config_section: bool) -> BinaryUsage {
        BinaryUsage {
            binary: binary.to_owned(),
            origin: ReferenceOrigin {
                file: "Makefile".to_owned(),
                line: Some(1),
                label: "recipe".to_owned(),
            },
            config_section,
            inferred: false,
        }
    }

    fn hints(usages: Vec<BinaryUsage>) -> PluginHints {
        PluginHints {
            contributions: Vec::new(),
            config_binary_usages: usages,
            config_used_distributions: Vec::new(),
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
            config_requirement_names: Vec::new(),
            unread_requirements: Vec::new(),
            config_module_refs: Vec::new(),
            config_used_symbols: Vec::new(),
            config_script_entries: Vec::new(),
            config_command_targets: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn command_hints(binaries: &[&str]) -> PluginHints {
        hints(binaries.iter().map(|binary| usage(binary, false)).collect())
    }

    fn resolution(pairs: &[(&str, &str)]) -> ResolutionIndex {
        ResolutionIndex {
            binary_resolutions: pairs
                .iter()
                .map(|(binary, distribution)| ((*binary).to_owned(), (*distribution).to_owned()))
                .collect(),
            ..ResolutionIndex::default()
        }
    }

    fn detect(
        manifest: &LoadedManifest,
        members: &[WorkspaceDependencyBoundary<'_>],
        resolution: &ResolutionIndex,
        plugins: &PluginHints,
    ) -> Vec<String> {
        let declared = build_declared_index(manifest);
        let providers = BinaryProviders::new(&declared, manifest, members, plugins);
        detect_unlisted_binaries(&providers, resolution, plugins)
            .iter()
            .filter_map(|candidate| match &candidate.subject {
                IssueSubject::Binary { name } if candidate.rule == RuleId::Chk008 => {
                    Some(name.clone())
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn undeclared_binary_emits_chk008() {
        let found = detect(
            &manifest(&[]),
            &[],
            &resolution(&[("pytest", "pytest")]),
            &command_hints(&["pytest"]),
        );
        assert_eq!(found, ["pytest"]);
    }

    #[test]
    fn a_config_section_alone_is_likely_and_a_command_makes_it_certain() {
        let resolution = resolution(&[("coverage", "coverage"), ("pylint", "pylint")]);
        let mut command = usage("coverage", false);
        command.origin.line = Some(7);
        let plugins = hints(vec![
            usage("coverage", true),
            usage("pylint", true),
            command,
        ]);
        let manifest = manifest(&[]);
        let declared = build_declared_index(&manifest);
        let providers = BinaryProviders::new(&declared, &manifest, &[], &plugins);
        let found: Vec<_> = detect_unlisted_binaries(&providers, &resolution, &plugins)
            .into_iter()
            .filter_map(
                |candidate| match (&candidate.subject, &candidate.origins[..]) {
                    (IssueSubject::Binary { name }, [Origin::Binary(origin)]) => Some((
                        name.clone(),
                        candidate.confidence,
                        origin.line,
                        candidate.explain.details.len(),
                    )),
                    _ => None,
                },
            )
            .collect();
        assert_eq!(
            found,
            [
                ("pylint".to_owned(), Confidence::Likely, Some(1), 2),
                ("coverage".to_owned(), Confidence::Certain, Some(7), 1),
            ]
        );
    }

    #[test]
    fn inferred_usage_emits_no_chk008() {
        let found = detect(
            &manifest(&[]),
            &[],
            &resolution(&[("pytest", "pytest")]),
            &hints(vec![BinaryUsage {
                inferred: true,
                ..usage("pytest", false)
            }]),
        );
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn binaries_of_same_distribution_emit_one_chk008() {
        let found = detect(
            &manifest(&[]),
            &[],
            &resolution(&[("pytest", "pytest"), ("py.test", "pytest")]),
            &command_hints(&["pytest", "py.test", "pytest"]),
        );
        assert_eq!(found, ["pytest"]);
    }

    #[test]
    fn remote_hook_covers_config_sections_but_not_commands() {
        let mut plugins = hints(vec![
            usage("ruff", true),
            usage("flake8", true),
            usage("flake8", false),
        ]);
        plugins.config_provided_binaries = vec!["ruff".to_owned(), "flake8".to_owned()];
        let found = detect(
            &manifest(&[]),
            &[],
            &resolution(&[("ruff", "ruff"), ("flake8", "flake8")]),
            &plugins,
        );
        assert_eq!(found, ["flake8"]);
    }

    #[test]
    fn tox_nox_member_and_requirements_declarations_provide_binaries() {
        let mut root = manifest(&[]);
        root.sources.extra_requirements.insert("sphinx".to_owned());
        let member = manifest(&["mypy"]);
        let mut plugins = command_hints(&["coverage", "sphinx-build", "mypy", "black"]);
        plugins.config_declared_distributions = vec!["coverage".to_owned()];
        let found = detect(
            &root,
            &[WorkspaceDependencyBoundary {
                member_id: "devel-common",
                manifest: &member,
                files: &[],
                imports: &[],
            }],
            &resolution(&[
                ("coverage", "coverage"),
                ("sphinx-build", "sphinx"),
                ("mypy", "mypy"),
                ("black", "black"),
            ]),
            &plugins,
        );
        assert_eq!(found, ["black"]);
    }

    #[test]
    fn declared_wrappers_and_lockfile_edges_provide_binaries() {
        let mut root = manifest(&["mkdocs-material", "tox-uv"]);
        root.lockfile
            .edges
            .insert("tox-uv".to_owned(), vec!["tox".to_owned()]);
        let found = detect(
            &root,
            &[],
            &resolution(&[("mkdocs", "mkdocs"), ("tox", "tox"), ("nox", "nox")]),
            &command_hints(&["mkdocs", "tox", "nox"]),
        );
        assert_eq!(found, ["nox"]);
    }

    #[test]
    fn prek_pytest_cov_and_tox_uv_provide_without_a_lockfile() {
        let found = detect(
            &manifest(&["prek", "pytest-cov", "tox-uv"]),
            &[],
            &resolution(&[
                ("pre-commit", "pre-commit"),
                ("coverage", "coverage"),
                ("tox", "tox"),
                ("nox", "nox"),
            ]),
            &command_hints(&["pre-commit", "coverage", "tox", "nox"]),
        );
        assert_eq!(found, ["nox"]);
    }

    #[test]
    fn the_projects_own_distribution_provides_its_binary() {
        let mut root = manifest(&[]);
        root.metadata.name = Some("Black".to_owned());
        let mut member = manifest(&[]);
        member.metadata.name = Some("my_cli".to_owned());
        let found = detect(
            &root,
            &[WorkspaceDependencyBoundary {
                member_id: "cli",
                manifest: &member,
                files: &[],
                imports: &[],
            }],
            &resolution(&[("black", "black"), ("my-cli", "my-cli"), ("nox", "nox")]),
            &command_hints(&["black", "my-cli", "nox"]),
        );
        assert_eq!(found, ["nox"]);
    }

    #[test]
    fn running_prek_provides_the_pre_commit_config_but_not_pre_commit_commands() {
        let config = BinaryUsage {
            origin: ReferenceOrigin {
                file: ".pre-commit-config.yaml".to_owned(),
                line: None,
                label: ".pre-commit-config.yaml".to_owned(),
            },
            ..usage("pre-commit", false)
        };
        let resolution = resolution(&[("pre-commit", "pre-commit"), ("prek", "prek")]);
        let only_config = hints(vec![config.clone()]);
        assert_eq!(
            detect(&manifest(&[]), &[], &resolution, &only_config),
            ["pre-commit"]
        );
        let with_prek = hints(vec![config.clone(), usage("prek", false)]);
        assert_eq!(
            detect(&manifest(&[]), &[], &resolution, &with_prek),
            ["prek"]
        );
        let with_command = hints(vec![
            config,
            usage("prek", false),
            usage("pre-commit", false),
        ]);
        assert_eq!(
            detect(&manifest(&[]), &[], &resolution, &with_command),
            ["prek", "pre-commit"]
        );
    }

    #[test]
    fn dbt_adapters_and_the_old_dg_distribution_provide_their_clis() {
        let found = detect(
            &manifest(&["dbt-snowflake", "dagster-dg"]),
            &[],
            &resolution(&[
                ("dbt", "dbt-core"),
                ("dg", "dagster-dg-cli"),
                ("nox", "nox"),
            ]),
            &command_hints(&["dbt", "dg", "nox"]),
        );
        assert_eq!(found, ["nox"]);
    }

    /// How the tool distribution is provided in `decisions_follow_the_formal_model`.
    #[derive(Clone, Copy, Debug)]
    enum Provider {
        None,
        Declared,
        PipInstall,
        LockEdge,
        Wrapper,
        OwnExact,
        OwnSpelled,
    }

    /// One input of `decisions_follow_the_formal_model`: the provider, which
    /// tool usages exist (bit 0 a command, 1 the second binary, 2 a config
    /// section, 3 `prek` running), a pytest-style usage (`Some(inferred)`),
    /// a hook (`Some((local, isolated))`), the root `.pre-commit-config.yaml`
    /// and a `pre-commit` command.
    #[derive(Clone, Copy, Debug)]
    struct FormalCase {
        provider: Provider,
        bits: u8,
        pytest: Option<bool>,
        hook: Option<(bool, bool)>,
        config_file: bool,
        pre_commit_command: bool,
    }

    fn formal_usage(binary: &str, file: &str, config_section: bool, inferred: bool) -> BinaryUsage {
        BinaryUsage {
            binary: binary.to_owned(),
            origin: ReferenceOrigin {
                file: file.to_owned(),
                line: None,
                label: file.to_owned(),
            },
            config_section,
            inferred,
        }
    }

    /// The usages and hook binaries of `case`, as the model's `build`.
    fn formal_usages(case: FormalCase) -> (Vec<BinaryUsage>, Vec<&'static str>) {
        let mut usages = Vec::new();
        let mut hook_binaries = Vec::new();
        if case.bits & 1 != 0 {
            usages.push(formal_usage("tox", "Makefile", false, false));
        }
        if case.bits & 2 != 0 {
            usages.push(formal_usage("tox-x", "noxfile.py", false, false));
        }
        if case.bits & 4 != 0 {
            usages.push(formal_usage("tox", "pyproject.toml", true, false));
        }
        if let Some(inferred) = case.pytest {
            usages.push(formal_usage("tox", "tests", false, inferred));
        }
        if let Some((local, isolated)) = case.hook {
            let hook = formal_usage("tox", ".pre-commit-config.yaml", isolated, false);
            if local {
                usages.push(hook.clone());
            }
            usages.push(hook);
            if isolated {
                hook_binaries.push("tox");
            }
        }
        if case.bits & 8 != 0 {
            usages.push(formal_usage("prek", "Makefile", false, false));
        }
        if case.config_file {
            usages.push(formal_usage(
                "pre-commit",
                ".pre-commit-config.yaml",
                true,
                false,
            ));
        }
        if case.pre_commit_command {
            usages.push(formal_usage("pre-commit", "tox.ini", false, false));
        }
        (usages, hook_binaries)
    }

    fn formal_resolve(binary: &str) -> &'static str {
        match binary {
            "tox" | "tox-x" => "tox",
            "pre-commit" => "pre-commit",
            "prek" => "prek",
            other => panic!("unresolved {other}"),
        }
    }

    /// A CHK008 as the model reports it: binary, reported file, confidence.
    type FormalVerdict = (String, String, Confidence);

    /// `docs/dev/formal/binary_providers_model.py` `detect`, transcribed:
    /// (binary, reported file, confidence) for the unprovided usages.
    fn formal_detect(
        usages: &[BinaryUsage],
        provided: &BTreeSet<&str>,
        own: &[&str],
        hook_binaries: &[&str],
    ) -> Vec<FormalVerdict> {
        let prek_runs_hooks = usages.iter().any(|usage| usage.binary == "prek");
        let is_provided = |usage: &BinaryUsage, dist: &str| {
            provided.contains(dist)
                || own
                    .iter()
                    .any(|name| normalize_distribution_name(name) == dist)
                || (usage.config_section && hook_binaries.contains(&usage.binary.as_str()))
                || (prek_runs_hooks
                    && usage.binary == "pre-commit"
                    && usage.origin.file == ".pre-commit-config.yaml")
        };
        let unprovided: Vec<(&BinaryUsage, &str)> = usages
            .iter()
            .filter(|usage| !usage.inferred)
            .map(|usage| (usage, formal_resolve(&usage.binary)))
            .filter(|(usage, dist)| !is_provided(usage, dist))
            .collect();
        let run_by_command: BTreeSet<&str> = unprovided
            .iter()
            .filter(|(usage, _)| !usage.config_section)
            .map(|(_, dist)| *dist)
            .collect();
        let mut reported = BTreeSet::new();
        let mut found = Vec::new();
        for (usage, dist) in unprovided {
            let run = run_by_command.contains(dist);
            if (run && usage.config_section) || !reported.insert(dist) {
                continue;
            }
            let confidence = if run {
                Confidence::Certain
            } else {
                Confidence::Likely
            };
            found.push((usage.binary.clone(), usage.origin.file.clone(), confidence));
        }
        found
    }

    /// `detect_unlisted_binaries` on `case` as a real manifest and hints, and
    /// the model's verdict on it.
    fn formal_run(case: FormalCase) -> (Vec<FormalVerdict>, Vec<FormalVerdict>) {
        let (usages, hook_binaries) = formal_usages(case);
        let (declared, edge, own): (&[&str], bool, &[&str]) = match case.provider {
            Provider::None | Provider::PipInstall => (&[], false, &[]),
            Provider::Declared => (&["tox"], false, &[]),
            Provider::LockEdge => (&["framework"], true, &[]),
            Provider::Wrapper => (&["tox-uv"], false, &[]),
            Provider::OwnExact => (&[], false, &["tox"]),
            Provider::OwnSpelled => (&[], false, &["Tox"]),
        };
        let mut root = manifest(declared);
        if edge {
            root.lockfile
                .edges
                .insert("framework".to_owned(), vec!["tox".to_owned()]);
        }
        root.metadata.name = own.first().map(|name| (*name).to_owned());
        let mut plugins = hints(usages.clone());
        let pip_install = matches!(case.provider, Provider::PipInstall);
        if pip_install {
            plugins.config_declared_distributions.push("tox".to_owned());
        }
        plugins.config_provided_binaries = hook_binaries.iter().map(|b| (*b).to_owned()).collect();
        let resolution = resolution(&[
            ("tox", "tox"),
            ("tox-x", "tox"),
            ("pre-commit", "pre-commit"),
            ("prek", "prek"),
        ]);
        let declared_index = build_declared_index(&root);
        let providers = BinaryProviders::new(&declared_index, &root, &[], &plugins);
        let found = detect_unlisted_binaries(&providers, &resolution, &plugins)
            .into_iter()
            .map(|candidate| {
                let IssueSubject::Binary { name } = candidate.subject else {
                    panic!("not a binary subject");
                };
                let Some(Origin::Binary(origin)) = candidate.origins.first().cloned() else {
                    panic!("no binary origin");
                };
                (name, origin.file, candidate.confidence)
            })
            .collect();
        // The model's `provider_set`: declarations, their lock edges and
        // wrappers.
        let mut provided: BTreeSet<&str> = declared.iter().copied().collect();
        if pip_install || edge || matches!(case.provider, Provider::Wrapper) {
            provided.insert("tox");
        }
        (
            found,
            formal_detect(&usages, &provided, own, &hook_binaries),
        )
    }

    /// The CHK008 decision `docs/dev/formal/binary_providers_model.py`
    /// checks is the one `detect_unlisted_binaries` implements: every way
    /// the tool is provided × its command, second binary, config section and
    /// pytest-style usage × a pre-commit hook in its own or the project's env
    /// × `prek`, the root `.pre-commit-config.yaml` and a `pre-commit`
    /// command. The hooks' isolation itself is `config_scan.rs`
    /// `hook_isolation_follows_the_formal_model`.
    #[test]
    fn decisions_follow_the_formal_model() {
        let providers = [
            Provider::None,
            Provider::Declared,
            Provider::PipInstall,
            Provider::LockEdge,
            Provider::Wrapper,
            Provider::OwnExact,
            Provider::OwnSpelled,
        ];
        let hooks = [
            None,
            Some((false, true)),
            Some((true, true)),
            Some((true, false)),
        ];
        let pytests = [None, Some(true), Some(false)];
        // Each index decodes to one case, the provider varying fastest.
        for index in 0..7 * 16 * 3 * 4 * 4_usize {
            let case = FormalCase {
                provider: providers[index % 7],
                bits: u8::try_from(index / 7 % 16).expect("bits"),
                pytest: pytests[index / (7 * 16) % 3],
                hook: hooks[index / (7 * 16 * 3) % 4],
                config_file: index / (7 * 16 * 3 * 4) % 2 != 0,
                pre_commit_command: index / (7 * 16 * 3 * 4 * 2) != 0,
            };
            let (found, expected) = formal_run(case);
            assert_eq!(found, expected, "{case:?}");
        }
    }
}
