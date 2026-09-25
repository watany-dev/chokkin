//! Manifest extraction types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::UvWorkspaceHint;
use crate::discovery::ProjectRoot;

use super::warnings::ManifestWarning;

/// Where a dependency was declared (manifest stage).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DependencyContext {
    /// `[project.dependencies]` or runtime `requirements.txt`.
    Runtime,
    /// Named `[dependency-groups]` entry or dev requirements file.
    Group(String),
    /// `[project.optional-dependencies].<extra>`.
    OptionalExtra(String),
    /// `setup.cfg` `extras_require`.
    SetupExtra(String),
}

/// Declaration location for reports, `--explain`, and fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyOrigin {
    /// Root-relative path, e.g. `pyproject.toml`.
    pub file: String,
    /// 1-based line number when available.
    pub line: Option<u32>,
    /// TOML key path or requirements file context.
    pub label: String,
}

/// A declared third-party or path dependency from manifest sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredDependency {
    /// PEP 508 distribution name (normalized to lowercase hyphen form).
    pub name: String,
    /// Requested extras, if any.
    pub extras: Vec<String>,
    /// Environment marker string (evaluation deferred to later steps).
    pub marker: Option<String>,
    /// Version specifier string as written (informational in v0.1).
    pub specifier: Option<String>,
    /// Declaration context.
    pub context: DependencyContext,
    /// Source location.
    pub origin: DependencyOrigin,
    /// URL / VCS without extractable distribution name.
    pub opaque: bool,
    /// PEP 735 `include-group` chains that pull this group requirement into
    /// other groups, each ordered from the including group to the declaring one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_via: Vec<Vec<String>>,
}

/// Console script or entry-point declaration from packaging metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPointDecl {
    /// Distribution-local name, e.g. `acme-cli`.
    pub name: String,
    /// `module:attr` or `module` target string as written.
    pub target: String,
    /// `console` | `gui` | other group name.
    pub group: String,
    /// Source location.
    pub origin: DependencyOrigin,
}

/// Project metadata from packaging manifests.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectMetadata {
    /// `[project].name`.
    pub name: Option<String>,
    /// `[project].version`.
    pub version: Option<String>,
    /// `[project].requires-python`.
    pub requires_python: Option<String>,
    /// `[project].dynamic` entries, e.g. `dependencies`.
    pub dynamic: Vec<String>,
}

/// Where a `[tool.uv.sources]` entry points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UvSourceKind {
    /// `{ path = "..." }` as written, so cached manifests carry no absolute root.
    Path {
        /// Path string from the manifest.
        path: String,
        /// `editable = true`.
        editable: bool,
    },
    /// `{ workspace = true }`.
    Workspace,
    /// `{ git = "..." }`.
    Git(String),
    /// `{ url = "..." }`.
    Url(String),
    /// `{ index = "..." }`.
    Index(String),
    /// Any other shape.
    Other,
}

/// One `[tool.uv.sources]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UvSource {
    /// Normalized distribution name the source applies to.
    pub name: String,
    /// Source kind.
    pub kind: UvSourceKind,
    /// Source location.
    pub origin: DependencyOrigin,
}

/// `[tool.uv] default-groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UvDefaultGroups {
    /// `default-groups = "all"`.
    All,
    /// Explicit group list.
    Groups(Vec<String>),
}

/// `[tool.uv]` settings that are not dependency declarations.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UvToolSettings {
    /// `[tool.uv.sources]` entries.
    pub sources: Vec<UvSource>,
    /// `default-groups`. Install-time default only; `--production` does not read it.
    pub default_groups: Option<UvDefaultGroups>,
}

impl UvToolSettings {
    /// Whether normalized `name` has a `workspace = true` source.
    #[must_use]
    pub fn is_workspace_source(&self, name: &str) -> bool {
        self.sources
            .iter()
            .any(|source| source.name == name && source.kind == UvSourceKind::Workspace)
    }
}

/// Resolved dependency graph from a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LockfileGraph {
    /// Package name to direct dependency names.
    pub edges: BTreeMap<String, Vec<String>>,
}

/// Lockfile formats read into [`LockfileGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockfileKind {
    /// `uv.lock`.
    Uv,
    /// PEP 751 `pylock.toml` / `pylock.<name>.toml`.
    Pylock,
    /// `poetry.lock`.
    Poetry,
    /// `pdm.lock`.
    Pdm,
}

impl LockfileKind {
    /// Short format name for probe output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uv => "uv",
            Self::Pylock => "pylock",
            Self::Poetry => "poetry",
            Self::Pdm => "pdm",
        }
    }
}

/// The lockfile that fed [`LoadedManifest::lockfile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockfileSource {
    /// Lockfile format.
    pub kind: LockfileKind,
    /// Root-relative path, e.g. `pylock.dev.toml`.
    pub path: String,
}

/// Which manifest files contributed to extraction.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ManifestSources {
    /// `pyproject.toml` contributed project metadata or dependencies.
    pub pyproject_toml: bool,
    /// Root-relative requirements file paths that contributed.
    pub requirements_files: Vec<String>,
    /// Root-relative requirements include/constraint paths that were probed
    /// but did not exist; the manifest cache rechecks them on a hit.
    pub requirements_missing: Vec<String>,
    /// `setup.cfg` contributed.
    pub setup_cfg: bool,
    /// `setup.py` contributed (static parse succeeded).
    pub setup_py: bool,
    /// `uv.lock` contributed. Kept for library API compatibility; mirrors
    /// `lockfile` having [`LockfileKind::Uv`].
    pub uv_lock: bool,
    /// Lockfile that contributed the transitive graph.
    pub lockfile: Option<LockfileSource>,
    /// Poetry sections were detected.
    pub poetry: bool,
}

/// Fully extracted manifest for a project root.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedManifest {
    /// Project root from discovery step 1.
    pub root: ProjectRoot,
    /// Packaging metadata.
    pub metadata: ProjectMetadata,
    /// Declared dependencies from all manifest sources.
    pub dependencies: Vec<DeclaredDependency>,
    /// Version constraints from `-c` requirements files and `[tool.uv]`
    /// `constraint-dependencies` / `override-dependencies`; never declarations.
    pub constraints: Vec<DeclaredDependency>,
    /// `[tool.uv]` sources and default groups.
    pub uv: UvToolSettings,
    /// Raw `[tool.uv.workspace]` members copied from config load (hash input).
    pub uv_workspace: Option<UvWorkspaceHint>,
    /// Packaging entry points.
    pub entry_points: Vec<EntryPointDecl>,
    /// Lockfile transitive closure graph.
    pub lockfile: LockfileGraph,
    /// Files that contributed.
    pub sources: ManifestSources,
    /// Non-fatal extraction warnings.
    pub warnings: Vec<ManifestWarning>,
}
