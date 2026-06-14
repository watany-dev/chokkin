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

/// Resolved dependency graph from a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LockfileGraph {
    /// Package name to direct dependency names.
    pub edges: BTreeMap<String, Vec<String>>,
    /// Lockfile `requires-python` when present.
    pub requires_python: Option<String>,
}

/// Which manifest files contributed to extraction.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ManifestSources {
    /// `pyproject.toml` contributed project metadata or dependencies.
    pub pyproject_toml: bool,
    /// Root-relative requirements file paths that contributed.
    pub requirements_files: Vec<String>,
    /// `setup.cfg` contributed.
    pub setup_cfg: bool,
    /// `setup.py` contributed (static parse succeeded).
    pub setup_py: bool,
    /// `uv.lock` contributed.
    pub uv_lock: bool,
    /// Poetry sections were detected and skipped.
    pub skipped_poetry: bool,
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
    /// Version constraints from `-c` requirements files.
    pub constraints: Vec<DeclaredDependency>,
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
