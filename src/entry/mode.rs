//! `mode = auto` resolution (§8).

use crate::config::{ProjectMode, YokeiConfig};
use crate::manifest::LoadedManifest;
use crate::resolver::ResolveConfidence;
use crate::sources::DiscoveredSources;

use super::types::{EntryCandidate, EntryWarning, ResolvedMode};

const APP_ENTRY_FILE_NAMES: &[&str] = &["manage.py", "asgi.py", "wsgi.py", "app.py"];

/// Resolve effective project mode from config, manifest, and discovered entries.
#[must_use]
pub fn resolve_project_mode(
    config: &YokeiConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
    warnings: &mut Vec<EntryWarning>,
) -> ResolvedMode {
    if config.mode != ProjectMode::Auto {
        return ResolvedMode {
            mode: config.mode,
            confidence: ResolveConfidence::Certain,
        };
    }

    if let Some(member_count) = workspace_member_count(config, manifest)
        && member_count > 1
    {
        warnings.push(EntryWarning::WorkspaceMode { member_count });
        return ResolvedMode {
            mode: ProjectMode::App,
            confidence: ResolveConfidence::Likely,
        };
    }

    if has_clear_app_signals(manifest, candidates) {
        return ResolvedMode {
            mode: ProjectMode::App,
            confidence: ResolveConfidence::Certain,
        };
    }

    if is_library_project(manifest, sources) {
        return ResolvedMode {
            mode: ProjectMode::Library,
            confidence: ResolveConfidence::Certain,
        };
    }

    ResolvedMode {
        mode: ProjectMode::App,
        confidence: ResolveConfidence::Likely,
    }
}

fn workspace_member_count(config: &YokeiConfig, manifest: &LoadedManifest) -> Option<usize> {
    if let Some(hint) = &manifest.uv_workspace {
        let count = hint.members.len();
        if count > 1 {
            return Some(count);
        }
    }
    if config.workspaces.len() > 1 {
        return Some(config.workspaces.len());
    }
    None
}

fn has_clear_app_signals(manifest: &LoadedManifest, candidates: &[EntryCandidate]) -> bool {
    if manifest
        .entry_points
        .iter()
        .any(|entry| entry.group == "console" || entry.group == "gui")
    {
        return true;
    }

    candidates.iter().any(|candidate| {
        let file_name = candidate
            .spec
            .path
            .rsplit('/')
            .next()
            .unwrap_or(candidate.spec.path.as_str());
        APP_ENTRY_FILE_NAMES.contains(&file_name)
    })
}

fn is_library_project(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    if manifest.metadata.name.is_none() {
        return false;
    }

    if sources.layout.packages.is_empty() {
        return false;
    }

    sources.files.iter().any(|file| {
        sources.layout.packages.iter().any(|package| {
            file.path == format!("src/{package}/__init__.py")
                || file.path == format!("{package}/__init__.py")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EntrySpec;
    use crate::config::default_config;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        EntryPointDecl, LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata,
    };
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    };

    use super::super::types::EntryOrigin;

    fn empty_manifest() -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn library_sources() -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: vec![DiscoveredFile {
                path: "src/acme/__init__.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn explicit_mode_is_preserved() {
        let mut config = default_config();
        config.mode = ProjectMode::Library;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &empty_manifest(),
            &library_sources(),
            &[],
            &mut warnings,
        );
        assert_eq!(mode.mode, ProjectMode::Library);
        assert_eq!(mode.confidence, ResolveConfidence::Certain);
        assert!(warnings.is_empty());
    }

    #[test]
    fn library_mode_without_app_signals() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &[], &mut warnings);
        assert_eq!(mode.mode, ProjectMode::Library);
    }

    #[test]
    fn app_mode_from_console_scripts() {
        let mut manifest = empty_manifest();
        manifest.entry_points.push(EntryPointDecl {
            name: "acme-cli".to_owned(),
            target: "acme.cli:main".to_owned(),
            group: "console".to_owned(),
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                label: "project.scripts.acme-cli".to_owned(),
                line: None,
            },
        });
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &[], &mut warnings);
        assert_eq!(mode.mode, ProjectMode::App);
        assert_eq!(mode.confidence, ResolveConfidence::Certain);
    }

    #[test]
    fn fallback_app_mode_is_likely() {
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &empty_manifest(),
            &library_sources(),
            &[],
            &mut warnings,
        );
        assert_eq!(mode.mode, ProjectMode::App);
        assert_eq!(mode.confidence, ResolveConfidence::Likely);
    }

    #[test]
    fn manage_py_triggers_app_mode() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let candidates = vec![super::super::types::EntryCandidate {
            spec: EntrySpec {
                path: "manage.py".to_owned(),
                symbol: None,
            },
            context: FileContext::Runtime,
            origin: EntryOrigin::Auto {
                rule: "manage.py".to_owned(),
            },
        }];
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &manifest,
            &library_sources(),
            &candidates,
            &mut warnings,
        );
        assert_eq!(mode.mode, ProjectMode::App);
    }
}
