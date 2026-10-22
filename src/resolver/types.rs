//! Resolver types for import → distribution resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::graph::ModuleOrigin;
use crate::manifest::LockfileGraph;
use crate::parser::ImportContext;

/// Confidence in a resolved import → distribution mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveConfidence {
    /// Stdlib, first-party, venv metadata, or unique bundled match.
    Certain,
    /// User-provided `package_module_map`, or a declared / locked
    /// distribution whose normalized name matches the import root.
    Likely,
    /// Ambiguous candidates, or a root that did not resolve at all.
    Maybe,
}

/// One resolved import reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImport {
    /// Top-level import name (`yaml` from `yaml.loader`).
    pub import_root: String,
    /// Full dotted module from the import statement.
    pub full_module: String,
    /// Source file path.
    pub file: String,
    /// Workspace member id that owns `file`, when the import comes from a member.
    pub workspace_member: Option<String>,
    /// 1-based line number; `None` when a plugin reference has no position.
    pub line: Option<u32>,
    /// Config setting that names the module (`extensions`, `INSTALLED_APPS`,
    /// `pytest_plugins`); `None` for an `import` statement (#830).
    pub reference: Option<String>,
    /// Import context from the parser.
    pub context: ImportContext,
    /// `true` when the import appears inside a `try` block body.
    pub optional: bool,
    /// `true` when the import appears under an `if sys.platform …` guard.
    pub platform_guarded: bool,
    /// Classification origin.
    pub origin: ModuleOrigin,
    /// Normalized PEP 508 distribution name when third-party.
    pub distribution: Option<String>,
    /// Resolution confidence.
    pub confidence: ResolveConfidence,
}

impl ResolvedImport {
    /// Line of the `import` statement; `None` for a config reference, which
    /// no statement-keyed index (optional, deferred) covers.
    #[must_use]
    pub fn statement_line(&self) -> Option<u32> {
        self.line.filter(|_| self.reference.is_none())
    }
}

/// Non-fatal resolver warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveWarning {
    /// Multiple distributions map to the same import root.
    AmbiguousImport {
        /// Import root name.
        import: String,
        /// Candidate distribution names.
        candidates: Vec<String>,
    },
    /// Import could not be resolved to any origin.
    UnresolvedImport {
        /// Import root name.
        import: String,
        /// Source file path.
        file: String,
        /// 1-based line number; `None` when a plugin reference has no position.
        line: Option<u32>,
        /// Config setting that names the module (see `ResolvedImport::reference`).
        reference: Option<String>,
        /// Context of the import statement.
        context: ImportContext,
        /// `true` when a failed import is caught (see `ImportRef::optional`).
        optional: bool,
    },
    /// A `.venv` dist-info names no PEP 508 distribution, so its imports,
    /// scripts and plugins are not credited to it (#973).
    VenvDistributionIgnored {
        /// Root-relative dist-info directory.
        path: String,
        /// The `Name:` field, or the directory name when METADATA has none.
        name: String,
    },
    /// Project `.venv` metadata could not be read.
    VenvUnreadable {
        /// Root-relative virtualenv path.
        path: String,
        /// Failure reason.
        reason: String,
    },
}

/// Full resolution output for steps 8–12.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolutionIndex {
    /// Resolved imports from parse output and plugin refs.
    pub imports: Vec<Arc<ResolvedImport>>,
    /// Non-fatal warnings.
    pub warnings: Vec<ResolveWarning>,
    /// Lockfile transitive edges.
    pub transitive: LockfileGraph,
    /// Merged binary name → distribution map.
    pub binary_resolutions: BTreeMap<String, String>,
    /// Installed distributions that pytest auto-loads through `pytest11` entry points.
    pub pytest_plugin_distributions: BTreeSet<String>,
}

/// Extract the top-level import name from a dotted module path.
#[must_use]
pub fn import_root(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}
