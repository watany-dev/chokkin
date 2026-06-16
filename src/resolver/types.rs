//! Resolver types for import → distribution resolution.

use std::collections::BTreeMap;

use crate::graph::ModuleOrigin;
use crate::parser::ImportContext;

/// Confidence in a resolved import → distribution mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveConfidence {
    /// Stdlib, first-party, venv metadata, or unique bundled match.
    Certain,
    /// User-provided `package_module_map`.
    Likely,
    /// Canonicalize fallback or ambiguous candidates.
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
    /// 1-based line number.
    pub line: u32,
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
        /// 1-based line number.
        line: u32,
    },
    /// Project `.venv` metadata could not be read.
    VenvUnreadable {
        /// Virtualenv path.
        path: String,
        /// Failure reason.
        reason: String,
    },
}

/// Transitive dependency data from lockfiles (Step 10 input).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransitiveIndex {
    /// Direct dependency edges: distribution → dependencies.
    pub edges: BTreeMap<String, Vec<String>>,
}

impl TransitiveIndex {
    /// Empty index when no lockfile is available.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            edges: BTreeMap::new(),
        }
    }
}

/// Full resolution output for steps 8–12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionIndex {
    /// Resolved imports from parse output and plugin refs.
    pub imports: Vec<ResolvedImport>,
    /// Non-fatal warnings.
    pub warnings: Vec<ResolveWarning>,
    /// Lockfile transitive edges.
    pub transitive: TransitiveIndex,
    /// Merged binary name → distribution map.
    pub binary_resolutions: BTreeMap<String, String>,
}

impl ResolutionIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            imports: Vec::new(),
            warnings: Vec::new(),
            transitive: TransitiveIndex::empty(),
            binary_resolutions: BTreeMap::new(),
        }
    }
}

/// Extract the top-level import name from a dotted module path.
#[must_use]
pub fn import_root(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}
