//! CHK008 unlisted binary dependency detection.

use std::collections::{BTreeSet, HashSet};

use crate::config::Confidence;
use crate::manifest::LoadedManifest;
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
    /// Installed by remote pre-commit hooks; they cover config-section usages.
    hook_binaries: HashSet<&'a str>,
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
        Self {
            distributions,
            hook_binaries: plugins
                .config_provided_binaries
                .iter()
                .map(String::as_str)
                .collect(),
        }
    }

    fn provides(&self, usage: &BinaryUsage, distribution: &str) -> bool {
        self.distributions.contains(distribution)
            || (usage.config_section && self.hook_binaries.contains(usage.binary.as_str()))
    }
}

/// Detect CLI binaries used in config but not declared as dependencies.
pub(super) fn detect_unlisted_binaries(
    providers: &BinaryProviders<'_>,
    resolution: &ResolutionIndex,
    plugins: &PluginHints,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();

    for usage in plugins.all_binary_usages() {
        let Some(distribution) = resolution.binary_resolutions.get(&usage.binary) else {
            continue;
        };
        if providers.provides(usage, distribution) {
            continue;
        }
        if !reported.insert(distribution.clone()) {
            continue;
        }

        candidates.push(IssueCandidate {
            rule: RuleId::Chk008,
            subject: IssueSubject::Binary {
                name: usage.binary.clone(),
            },
            severity: Severity::Warning,
            confidence: Confidence::Certain,
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
                details: vec![match usage.origin.line {
                    Some(line) => format!("binary usage in {}:{line}", usage.origin.file),
                    None => format!("binary usage in {}", usage.origin.file),
                }],
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
        }
    }

    fn hints(usages: Vec<BinaryUsage>) -> PluginHints {
        PluginHints {
            contributions: Vec::new(),
            config_binary_usages: usages,
            config_used_distributions: Vec::new(),
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
            config_module_refs: Vec::new(),
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
}
