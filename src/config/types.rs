//! Configuration types for the chokkin analyzer.

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

    /// Parse a `[tool.chokkin].mode` value.
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

    /// Parse a `[tool.chokkin].confidence` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "certain" => Some(Self::Certain),
            "likely" => Some(Self::Likely),
            "maybe" => Some(Self::Maybe),
            _ => None,
        }
    }

    /// Numeric rank for floor comparisons (`Certain` is strongest).
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Certain => 2,
            Self::Likely => 1,
            Self::Maybe => 0,
        }
    }

    /// Returns true when `self` meets or exceeds `floor`.
    #[must_use]
    pub const fn meets_floor(self, floor: Self) -> bool {
        self.rank() >= floor.rank()
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Known chokkin plugins (§5, §9).
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
    /// TOML key under `[tool.chokkin.plugins]`.
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
    ///
    /// At most one `:` is allowed, separating a relative path from a symbol.
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() {
            return Err("entry path must not be empty");
        }
        if value == "." || value == ".." {
            return Err("entry path must not be . or ..");
        }

        let (path, symbol) = match value.rsplit_once(':') {
            Some((_, symbol)) if symbol.contains('/') || symbol.contains('\\') => {
                return Err("entry path must not contain ':'");
            },
            Some((path, symbol)) => {
                if symbol.is_empty() {
                    return Err("entry symbol must not be empty");
                }
                if path.is_empty() || path == "." || path == ".." {
                    return Err("entry path must not be empty");
                }
                if path.contains(':') {
                    return Err("entry path must not contain ':'");
                }
                (path, Some(symbol))
            },
            None => (value, None),
        };

        Ok(Self {
            path: path.to_owned(),
            symbol: symbol.map(str::to_owned),
        })
    }
}

/// Dependency group name mappings (§5 `[tool.chokkin.dependencies]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGroupsConfig {
    /// Group names treated as dev context.
    pub dev_groups: Vec<String>,
    /// Group names treated as runtime context.
    pub runtime_groups: Vec<String>,
    /// Group names treated as type-checking context.
    pub type_groups: Vec<String>,
}

/// Per-workspace overrides under `[tool.chokkin.workspaces.<id>]`.
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

/// Effective chokkin configuration after defaults and file layers are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChokkinConfig {
    /// Explicit entry roots.
    pub entry: Vec<EntrySpec>,
    /// Project file globs (unexpanded).
    pub project: Vec<String>,
    /// Analysis mode.
    pub mode: ProjectMode,
    /// Whether production-only analysis is enabled.
    pub production: bool,
    /// Explicit target Python version from config layers, if any.
    pub target_version: Option<TargetVersion>,
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
    /// `.chokkin.toml` at the project root, if present.
    pub dot_chokkin_toml: Option<PathBuf>,
    /// `chokkin.toml` at the project root, if present.
    pub chokkin_toml: Option<PathBuf>,
    /// Whether `[tool.chokkin]` in `pyproject.toml` contributed.
    pub pyproject_tool_chokkin: bool,
}

/// Configuration loaded for a discovered project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    /// Project root from discovery step 1.
    pub root: ProjectRoot,
    /// Merged effective configuration.
    pub effective: ChokkinConfig,
    /// Files that contributed to `effective`.
    pub sources: ConfigSources,
    /// Raw `[tool.uv.workspace]` hint from root `pyproject.toml`, if present.
    pub uv_workspace: Option<UvWorkspaceHint>,
    /// Resolved workspace members discovered below the project root.
    pub workspace_members: Vec<ResolvedWorkspaceMember>,
}

/// Raw `[tool.uv.workspace]` members from `pyproject.toml` (unexpanded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvWorkspaceHint {
    /// Member glob patterns as written in `pyproject.toml`.
    pub members: Vec<String>,
}

/// Source that declared a workspace member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceMemberSource {
    /// `[tool.uv.workspace].members`.
    Uv,
    /// `[tool.chokkin.workspaces.<id>]`.
    Chokkin,
}

/// Workspace member resolved relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspaceMember {
    /// Stable member id. Explicit chokkin workspaces use the table id; uv
    /// workspaces use the member directory basename.
    pub id: String,
    /// Member directory path relative to the project root using `/`.
    pub path: String,
    /// Root-relative member `pyproject.toml` path when present.
    pub pyproject_toml: Option<String>,
    /// Declaration source.
    pub source: WorkspaceMemberSource,
}

/// CLI flags that override file config (§2). Unset fields do not override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    /// Override `production` when set.
    pub production: Option<bool>,
    /// Override strict mode when set.
    pub strict: Option<bool>,
    /// Override minimum confidence when set.
    pub confidence_floor: Option<Confidence>,
    /// When true, report issues but return exit code 0.
    pub no_exit_code: Option<bool>,
    /// When set, only emit issues for these rule codes (`CHK00x`).
    pub include_rules: Option<Vec<String>>,
    /// When set, suppress issues for these rule codes (`CHK00x`).
    pub exclude_rules: Option<Vec<String>>,
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
    fn entry_spec_rejects_colon_in_path() {
        // Windows drive letters and ambiguous multi-colon entries are rejected (§7.2).
        EntrySpec::parse(r"C:\foo.py").unwrap_err();
        EntrySpec::parse("a:b:c").unwrap_err();
    }

    #[test]
    fn entry_spec_rejects_empty_symbol() {
        EntrySpec::parse("manage.py:").unwrap_err();
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Reference model for `TargetVersion::parse`: `py3` + 2-3 ASCII digits.
        fn is_valid_target_version(value: &str) -> bool {
            value.strip_prefix("py3").is_some_and(|suffix| {
                (2..=3).contains(&suffix.len()) && suffix.chars().all(|ch| ch.is_ascii_digit())
            })
        }

        proptest! {
            #[test]
            fn target_version_matches_reference_model(value in "\\PC{0,12}") {
                prop_assert_eq!(
                    TargetVersion::parse(&value).is_some(),
                    is_valid_target_version(&value)
                );
            }

            #[test]
            fn target_version_accepts_generated_valid_forms(suffix in "[0-9]{2,3}") {
                let value = format!("py3{suffix}");
                let parsed = TargetVersion::parse(&value).expect("valid form must parse");
                prop_assert_eq!(parsed.as_str(), value.as_str());
            }

            #[test]
            fn entry_spec_parse_never_panics(value in "\\PC{0,80}") {
                let _ = EntrySpec::parse(&value);
            }

            #[test]
            fn entry_spec_ok_invariants(value in "\\PC{0,80}") {
                if let Ok(spec) = EntrySpec::parse(&value) {
                    prop_assert!(!spec.path.is_empty());
                    prop_assert!(spec.path != "." && spec.path != "..");
                    prop_assert!(!spec.path.contains(':'));
                    if let Some(symbol) = &spec.symbol {
                        prop_assert!(!symbol.is_empty());
                    }
                }
            }

            #[test]
            fn entry_spec_roundtrips_path_and_symbol(
                path in "[a-z][a-z0-9_/.]{0,30}",
                symbol in "[A-Za-z_][A-Za-z0-9_]{0,12}",
            ) {
                prop_assume!(path != "." && path != "..");

                let plain = EntrySpec::parse(&path).expect("plain path must parse");
                prop_assert_eq!(&plain.path, &path);
                prop_assert_eq!(plain.symbol, None);

                let with_symbol =
                    EntrySpec::parse(&format!("{path}:{symbol}")).expect("path:symbol must parse");
                prop_assert_eq!(with_symbol.path, path);
                prop_assert_eq!(with_symbol.symbol, Some(symbol));
            }

            #[test]
            fn mode_and_confidence_parse_only_known_values(value in "\\PC{0,12}") {
                prop_assert_eq!(
                    ProjectMode::parse(&value).is_some(),
                    matches!(value.as_str(), "auto" | "app" | "library")
                );
                prop_assert_eq!(
                    Confidence::parse(&value).is_some(),
                    matches!(value.as_str(), "certain" | "likely" | "maybe")
                );
            }

            #[test]
            fn leading_separator_is_always_absolute(rest in "[a-z0-9/._-]{0,20}") {
                let posix = format!("/{rest}");
                let windows = format!("\\{rest}");
                prop_assert!(is_absolute_path_str(&posix));
                prop_assert!(is_absolute_path_str(&windows));
            }
        }

        #[test]
        fn plugin_id_keys_roundtrip() {
            for plugin in PluginId::all() {
                assert_eq!(PluginId::from_key(plugin.as_key()), Some(*plugin));
            }
        }

        #[test]
        fn mode_and_confidence_as_str_roundtrip() {
            for mode in [ProjectMode::Auto, ProjectMode::App, ProjectMode::Library] {
                assert_eq!(ProjectMode::parse(mode.as_str()), Some(mode));
            }
            for confidence in [Confidence::Certain, Confidence::Likely, Confidence::Maybe] {
                assert_eq!(Confidence::parse(confidence.as_str()), Some(confidence));
            }
        }
    }
}
