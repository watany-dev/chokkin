//! Configuration types for the yokei analyzer.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::discovery::ProjectRoot;

/// Project analysis mode (§5, §8). `Auto` is resolved in a later pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectMode {
    /// Infer app vs library from manifests and layout.
    #[default]
    Auto,
    /// Application project: aggressive unused-file detection.
    App,
    /// Library project: conservative unused-file detection.
    Library,
}

impl ProjectMode {
    /// Stable identifier for reporters and `--explain` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::App => "app",
            Self::Library => "library",
        }
    }

    /// Parse a `[tool.yokei].mode` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "app" => Some(Self::App),
            "library" => Some(Self::Library),
            _ => None,
        }
    }
}

impl fmt::Display for ProjectMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Minimum confidence for emitted issues (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Confidence {
    /// Only `certain` issues.
    Certain,
    /// `certain` and `likely` issues.
    #[default]
    Likely,
    /// All issues including `maybe`.
    Maybe,
}

impl Confidence {
    /// Stable identifier for reporters and `--explain` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Certain => "certain",
            Self::Likely => "likely",
            Self::Maybe => "maybe",
        }
    }

    /// Parse a `[tool.yokei].confidence` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "certain" => Some(Self::Certain),
            "likely" => Some(Self::Likely),
            "maybe" => Some(Self::Maybe),
            _ => None,
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Known yokei plugins (§5, §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluginId {
    /// pytest test discovery and fixtures.
    Pytest,
    /// Django settings and apps.
    Django,
    /// `FastAPI` routes and uvicorn references.
    Fastapi,
    /// Celery tasks and autodiscover.
    Celery,
    /// tox environments.
    Tox,
    /// nox sessions.
    Nox,
    /// pre-commit hooks.
    PreCommit,
    /// GitHub Actions workflows.
    GithubActions,
}

impl PluginId {
    /// TOML key under `[tool.yokei.plugins]`.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Pytest => "pytest",
            Self::Django => "django",
            Self::Fastapi => "fastapi",
            Self::Celery => "celery",
            Self::Tox => "tox",
            Self::Nox => "nox",
            Self::PreCommit => "pre_commit",
            Self::GithubActions => "github_actions",
        }
    }

    /// Parse a plugin table key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "pytest" => Some(Self::Pytest),
            "django" => Some(Self::Django),
            "fastapi" => Some(Self::Fastapi),
            "celery" => Some(Self::Celery),
            "tox" => Some(Self::Tox),
            "nox" => Some(Self::Nox),
            "pre_commit" => Some(Self::PreCommit),
            "github_actions" => Some(Self::GithubActions),
            _ => None,
        }
    }

    /// All known plugins in stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Pytest,
            Self::Django,
            Self::Fastapi,
            Self::Celery,
            Self::Tox,
            Self::Nox,
            Self::PreCommit,
            Self::GithubActions,
        ]
    }
}

/// Parsed Python target version (§5 `target_version`), e.g. `py311`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetVersion(String);

impl TargetVersion {
    /// Default target version used when config does not override it.
    #[must_use]
    pub fn default_py311() -> Self {
        Self("py311".to_owned())
    }

    /// Parse and validate a `py3XX` version string.
    pub fn parse(value: &str) -> Option<Self> {
        if !value.starts_with("py3") {
            return None;
        }
        let suffix = &value[3..];
        if suffix.len() < 2 || suffix.len() > 3 {
            return None;
        }
        if !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    /// Borrow the underlying version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Entry root: file path, optionally `path:symbol` for WSGI/ASGI callables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    /// Path relative to project root (no `:` suffix).
    pub path: String,
    /// Optional symbol after `:`, e.g. `application` in `asgi.py:application`.
    pub symbol: Option<String>,
}

impl EntrySpec {
    /// Parse an entry string such as `src/pkg/asgi.py:app`.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() {
            return Err("entry path must not be empty");
        }
        if value == "." || value == ".." {
            return Err("entry path must not be . or ..");
        }

        if let Some((path, symbol)) = split_entry_symbol(value) {
            if path.is_empty() || path == "." || path == ".." {
                return Err("entry path must not be empty");
            }
            return Ok(Self {
                path: path.to_owned(),
                symbol: Some(symbol.to_owned()),
            });
        }

        Ok(Self {
            path: value.to_owned(),
            symbol: None,
        })
    }
}

fn split_entry_symbol(value: &str) -> Option<(&str, &str)> {
    let (path, symbol) = value.rsplit_once(':')?;
    if symbol.contains('/') || symbol.contains('\\') {
        return None;
    }
    Some((path, symbol))
}

/// Dependency group name mappings (§5 `[tool.yokei.dependencies]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGroupsConfig {
    /// Group names treated as dev context.
    pub dev_groups: Vec<String>,
    /// Group names treated as runtime context.
    pub runtime_groups: Vec<String>,
    /// Group names treated as type-checking context.
    pub type_groups: Vec<String>,
}

/// Per-workspace overrides under `[tool.yokei.workspaces.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOverride {
    /// Member path relative to the project root.
    pub path: String,
    /// Optional entry roots for this member.
    pub entry: Option<Vec<EntrySpec>>,
    /// Optional project globs for this member.
    pub project: Option<Vec<String>>,
    /// Optional analysis mode for this member.
    pub mode: Option<ProjectMode>,
}

/// Effective yokei configuration after defaults and file layers are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YokeiConfig {
    /// Explicit entry roots.
    pub entry: Vec<EntrySpec>,
    /// Project file globs (unexpanded).
    pub project: Vec<String>,
    /// Analysis mode.
    pub mode: ProjectMode,
    /// Whether production-only analysis is enabled.
    pub production: bool,
    /// Target Python version for parsing and stdlib checks.
    pub target_version: TargetVersion,
    /// Whether to respect `.gitignore` during file discovery.
    pub respect_gitignore: bool,
    /// Minimum issue confidence to report.
    pub confidence: Confidence,
    /// Glob patterns excluded from analysis.
    pub exclude: Vec<String>,
    /// Dependency group mappings.
    pub dependencies: DependencyGroupsConfig,
    /// User overrides for distribution → import module names.
    pub package_module_map: BTreeMap<String, Vec<String>>,
    /// User overrides for CLI binary → distribution names.
    pub binary_map: BTreeMap<String, String>,
    /// Plugin enablement flags.
    pub plugins: BTreeMap<PluginId, bool>,
    /// Per-rule ignore patterns (loaded only; matching is a later step).
    pub ignore: BTreeMap<String, Vec<String>>,
    /// Explicit workspace member overrides.
    pub workspaces: BTreeMap<String, WorkspaceOverride>,
}

/// Which config files contributed to the effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSources {
    /// Hardcoded defaults always contribute.
    pub used_defaults: bool,
    /// `.yokei.toml` at the project root, if present.
    pub dot_yokei_toml: Option<PathBuf>,
    /// `yokei.toml` at the project root, if present.
    pub yokei_toml: Option<PathBuf>,
    /// Whether `[tool.yokei]` in `pyproject.toml` contributed.
    pub pyproject_tool_yokei: bool,
}

/// Configuration loaded for a discovered project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    /// Project root from discovery step 1.
    pub root: ProjectRoot,
    /// Merged effective configuration.
    pub effective: YokeiConfig,
    /// Files that contributed to `effective`.
    pub sources: ConfigSources,
    /// Raw `[tool.uv.workspace]` hint from root `pyproject.toml`, if present.
    pub uv_workspace: Option<UvWorkspaceHint>,
}

/// Raw `[tool.uv.workspace]` members from `pyproject.toml` (unexpanded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvWorkspaceHint {
    /// Member glob patterns as written in `pyproject.toml`.
    pub members: Vec<String>,
}

/// CLI flags that override file config (§2). Unset fields do not override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    /// Override `production` when set.
    pub production: Option<bool>,
    /// Override strict mode when set (reserved for CLI integration).
    pub strict: Option<bool>,
    /// Override minimum confidence when set.
    pub confidence_floor: Option<Confidence>,
}

/// Returns true when `path` must be rejected as non-root-relative.
///
/// POSIX leading `/` and Windows leading `\` are treated as absolute on every OS
/// so config validation stays consistent across platforms.
pub(super) fn is_absolute_path_str(path: &str) -> bool {
    Path::new(path).is_absolute() || path.starts_with('/') || path.starts_with('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_spec_parses_symbol_suffix() {
        let spec = EntrySpec::parse("src/pkg/asgi.py:app").expect("parse entry");
        assert_eq!(spec.path, "src/pkg/asgi.py");
        assert_eq!(spec.symbol.as_deref(), Some("app"));
    }

    #[test]
    fn entry_spec_allows_colon_paths_without_symbol_on_unix() {
        // On Unix, `C:\foo.py` is a relative path with backslashes.
        let spec = EntrySpec::parse(r"C:\foo.py").expect("parse on unix");
        assert_eq!(spec.path, r"C:\foo.py");
        assert!(spec.symbol.is_none());
    }

    #[test]
    fn absolute_posix_path_is_rejected() {
        assert!(is_absolute_path_str("/absolute/manage.py"));
        assert!(is_absolute_path_str(r"\absolute\manage.py"));
    }

    #[test]
    #[cfg(windows)]
    fn windows_drive_path_is_rejected() {
        assert!(is_absolute_path_str(r"C:\absolute\manage.py"));
    }

    #[test]
    fn target_version_accepts_py311() {
        assert!(TargetVersion::parse("py311").is_some());
    }

    #[test]
    fn target_version_rejects_invalid() {
        assert!(TargetVersion::parse("python3.11").is_none());
    }
}
