//! Plugin enablers: turn plugins on from declared dependencies and config files.

use std::fmt;

use crate::config::{ChokkinConfig, PluginId, ResolvedWorkspaceMember};
use crate::manifest::LoadedManifest;

/// Dependency names and directory-relative files that turn a plugin on.
struct Enabler {
    plugin: PluginId,
    dependencies: &'static [&'static str],
    files: &'static [&'static str],
}

// GitHub Actions has no enabler: almost every repository has workflows, and
// their `run:` commands would surface CHK008 for tools nobody declares.
const ENABLERS: &[Enabler] = &[
    Enabler {
        plugin: PluginId::Pytest,
        dependencies: &["pytest"],
        files: &[],
    },
    Enabler {
        plugin: PluginId::Django,
        dependencies: &["django"],
        files: &[],
    },
    Enabler {
        plugin: PluginId::Fastapi,
        dependencies: &["fastapi"],
        files: &[],
    },
    Enabler {
        plugin: PluginId::Flask,
        dependencies: &["flask"],
        files: &[],
    },
    Enabler {
        plugin: PluginId::Celery,
        dependencies: &["celery"],
        files: &[],
    },
    Enabler {
        plugin: PluginId::Tox,
        dependencies: &["tox"],
        files: &["tox.ini"],
    },
    Enabler {
        plugin: PluginId::Nox,
        dependencies: &["nox"],
        files: &["noxfile.py"],
    },
    Enabler {
        plugin: PluginId::PreCommit,
        dependencies: &["pre-commit"],
        files: &[".pre-commit-config.yaml"],
    },
    Enabler {
        plugin: PluginId::Sphinx,
        dependencies: &["sphinx"],
        files: &["docs/conf.py"],
    },
    Enabler {
        plugin: PluginId::MkDocs,
        dependencies: &["mkdocs"],
        files: &["mkdocs.yml", "mkdocs.yaml"],
    },
    Enabler {
        plugin: PluginId::Alembic,
        dependencies: &["alembic"],
        files: &["alembic.ini"],
    },
];

/// One place the enablers look: the project root or a workspace member.
#[derive(Debug, Clone, Copy)]
pub struct EnablerScope<'a> {
    /// Workspace member, or `None` for the project root.
    pub member: Option<&'a ResolvedWorkspaceMember>,
    /// Manifest extracted from the scope's directory.
    pub manifest: &'a LoadedManifest,
}

/// Why a plugin is on or off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginActivationReason {
    /// Hardcoded default (§5).
    Default,
    /// `[tool.chokkin.plugins]` sets the plugin explicitly.
    Config,
    /// A declared dependency enabled the plugin.
    Dependency {
        /// Normalized distribution name.
        name: String,
        /// Workspace member id when the dependency is member-local.
        member: Option<String>,
    },
    /// A config file enabled the plugin.
    File {
        /// Root-relative path using `/` separators.
        path: String,
    },
}

/// Resolved enablement of one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginActivation {
    pub plugin: PluginId,
    pub enabled: bool,
    pub reason: PluginActivationReason,
}

impl fmt::Display for PluginActivation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            PluginActivationReason::Default => f.write_str("default"),
            PluginActivationReason::Config if self.enabled => f.write_str("config"),
            PluginActivationReason::Config => f.write_str("disabled-by: config"),
            PluginActivationReason::Dependency { name, member: None } => {
                write!(f, "enabled-by: dependency {name}")
            },
            PluginActivationReason::Dependency {
                name,
                member: Some(member),
            } => write!(f, "enabled-by: dependency {name} (member {member})"),
            PluginActivationReason::File { path } => write!(f, "enabled-by: file {path}"),
        }
    }
}

/// Resolve every plugin's enablement: explicit config, then enablers, then defaults.
///
/// `config.plugins` must still hold defaults for plugins outside
/// `config.explicit_plugins`. Scopes are checked in order, so pass the project
/// root first to report root-level reasons ahead of member-level ones.
#[must_use]
pub fn resolve_plugin_activations(
    config: &ChokkinConfig,
    scopes: &[EnablerScope<'_>],
) -> Vec<PluginActivation> {
    PluginId::all()
        .iter()
        .map(|&plugin| {
            let configured = config.plugins.get(&plugin).copied().unwrap_or(false);
            if config.explicit_plugins.contains(&plugin) {
                return PluginActivation {
                    plugin,
                    enabled: configured,
                    reason: PluginActivationReason::Config,
                };
            }
            find_enabler(plugin, scopes).map_or(
                PluginActivation {
                    plugin,
                    enabled: configured,
                    reason: PluginActivationReason::Default,
                },
                |reason| PluginActivation {
                    plugin,
                    enabled: true,
                    reason,
                },
            )
        })
        .collect()
}

fn find_enabler(plugin: PluginId, scopes: &[EnablerScope<'_>]) -> Option<PluginActivationReason> {
    let enabler = ENABLERS.iter().find(|enabler| enabler.plugin == plugin)?;
    scopes.iter().find_map(|scope| match_scope(enabler, scope))
}

fn match_scope(enabler: &Enabler, scope: &EnablerScope<'_>) -> Option<PluginActivationReason> {
    if let Some(dependency) = scope
        .manifest
        .dependencies
        .iter()
        .find(|dependency| enabler.dependencies.contains(&dependency.name.as_str()))
    {
        return Some(PluginActivationReason::Dependency {
            name: dependency.name.clone(),
            member: scope.member.map(|member| member.id.clone()),
        });
    }
    let dir = scope.manifest.root.path.as_path();
    let file = enabler.files.iter().find(|file| dir.join(file).is_file())?;
    let path = scope.member.map_or_else(
        || (*file).to_owned(),
        |member| format!("{}/{file}", member.path.trim_end_matches('/')),
    );
    Some(PluginActivationReason::File { path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};

    fn manifest_at(dir: &std::path::Path, dependencies: &[&str]) -> LoadedManifest {
        let root = ProjectRoot {
            path: dir.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: dir.to_path_buf(),
        };
        LoadedManifest {
            root,
            metadata: crate::manifest::ProjectMetadata::default(),
            dependencies: dependencies
                .iter()
                .map(|name| DeclaredDependency {
                    name: (*name).to_owned(),
                    extras: Vec::new(),
                    marker: None,
                    specifier: None,
                    context: DependencyContext::Runtime,
                    origin: DependencyOrigin {
                        file: "pyproject.toml".to_owned(),
                        line: None,
                        label: "project.dependencies".to_owned(),
                    },
                    opaque: false,
                    included_via: Vec::new(),
                })
                .collect(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: crate::manifest::LockfileGraph::default(),
            sources: crate::manifest::ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn activation(activations: &[PluginActivation], plugin: PluginId) -> &PluginActivation {
        activations
            .iter()
            .find(|activation| activation.plugin == plugin)
            .expect("plugin activation")
    }

    #[test]
    fn dependency_enables_default_off_plugin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_at(temp.path(), &["flask"]);
        let activations = resolve_plugin_activations(
            &crate::default_config(),
            &[EnablerScope {
                member: None,
                manifest: &manifest,
            }],
        );

        let flask = activation(&activations, PluginId::Flask);
        assert!(flask.enabled);
        assert_eq!(flask.to_string(), "enabled-by: dependency flask");
        let pytest = activation(&activations, PluginId::Pytest);
        assert!(pytest.enabled);
        assert_eq!(pytest.to_string(), "default");
        assert!(!activation(&activations, PluginId::Celery).enabled);
    }

    #[test]
    fn config_file_enables_plugin() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("mkdocs.yml"), "site_name: demo\n").expect("write");
        let manifest = manifest_at(temp.path(), &[]);
        let activations = resolve_plugin_activations(
            &crate::default_config(),
            &[EnablerScope {
                member: None,
                manifest: &manifest,
            }],
        );

        let mkdocs = activation(&activations, PluginId::MkDocs);
        assert!(mkdocs.enabled);
        assert_eq!(mkdocs.to_string(), "enabled-by: file mkdocs.yml");
    }

    #[test]
    fn explicit_false_beats_enablers() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("alembic.ini"), "[alembic]\n").expect("write");
        let manifest = manifest_at(temp.path(), &["flask", "alembic"]);
        let mut config = crate::default_config();
        for plugin in [PluginId::Flask, PluginId::Alembic, PluginId::Pytest] {
            config.plugins.insert(plugin, false);
            config.explicit_plugins.insert(plugin);
        }
        let activations = resolve_plugin_activations(
            &config,
            &[EnablerScope {
                member: None,
                manifest: &manifest,
            }],
        );

        for plugin in [PluginId::Flask, PluginId::Alembic, PluginId::Pytest] {
            let record = activation(&activations, plugin);
            assert!(!record.enabled, "{plugin:?}");
            assert_eq!(record.to_string(), "disabled-by: config");
        }
    }

    #[test]
    fn explicit_true_reports_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_at(temp.path(), &[]);
        let mut config = crate::default_config();
        config.plugins.insert(PluginId::Tox, true);
        config.explicit_plugins.insert(PluginId::Tox);
        let activations = resolve_plugin_activations(
            &config,
            &[EnablerScope {
                member: None,
                manifest: &manifest,
            }],
        );

        let tox = activation(&activations, PluginId::Tox);
        assert!(tox.enabled);
        assert_eq!(tox.to_string(), "config");
    }

    #[test]
    fn workspace_member_enables_plugin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let member_dir = temp.path().join("packages").join("docs");
        std::fs::create_dir_all(member_dir.join("docs")).expect("mkdir");
        std::fs::write(member_dir.join("docs").join("conf.py"), "").expect("write");
        let root_manifest = manifest_at(temp.path(), &[]);
        let worker_manifest =
            manifest_at(&temp.path().join("packages").join("worker"), &["celery"]);
        let docs_manifest = manifest_at(&member_dir, &[]);
        let worker = ResolvedWorkspaceMember {
            id: "worker".to_owned(),
            path: "packages/worker".to_owned(),
            pyproject_toml: None,
        };
        let docs = ResolvedWorkspaceMember {
            id: "docs".to_owned(),
            path: "packages/docs".to_owned(),
            pyproject_toml: None,
        };
        let activations = resolve_plugin_activations(
            &crate::default_config(),
            &[
                EnablerScope {
                    member: None,
                    manifest: &root_manifest,
                },
                EnablerScope {
                    member: Some(&worker),
                    manifest: &worker_manifest,
                },
                EnablerScope {
                    member: Some(&docs),
                    manifest: &docs_manifest,
                },
            ],
        );

        assert_eq!(
            activation(&activations, PluginId::Celery).to_string(),
            "enabled-by: dependency celery (member worker)"
        );
        assert_eq!(
            activation(&activations, PluginId::Sphinx).to_string(),
            "enabled-by: file packages/docs/docs/conf.py"
        );
    }
}
