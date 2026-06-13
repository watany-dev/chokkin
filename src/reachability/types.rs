//! Reachability analysis types (pipeline step 9).

use indexmap::IndexSet;

use crate::config::Confidence;
use crate::graph::{EntryId, FileId, ModuleOrigin};

/// Why a file is excluded from or downgraded in unused-file candidacy (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreachableReason {
    /// No path from any entry root was found.
    NotReachable,
    /// `__init__.py` files are excluded from YOK001.
    ExcludedInit,
    /// Stub files are excluded from YOK001.
    ExcludedStub,
    /// Test-context files are excluded in library mode.
    ExcludedTestContext,
    /// Non-runtime context excluded when `production = true`.
    ExcludedProductionContext,
    /// Matched a framework-used glob from a plugin.
    FrameworkUsed,
}

/// A module import recorded for dependency reconciliation (Step 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedModule {
    /// Full dotted module from the import statement.
    pub full_module: String,
    /// Top-level import name.
    pub import_root: String,
    /// Classification from import resolution.
    pub origin: ModuleOrigin,
    /// Source file path.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
}

/// One file that is not reachable from entry roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableFile {
    /// Graph file identifier.
    pub file: FileId,
    /// Root-relative path using `/` separators.
    pub path: String,
    /// Exclusion or downgrade reasons for Step 12.
    pub reasons: Vec<UnreachableReason>,
    /// Upper bound on issue confidence for Step 12.
    pub max_confidence: Confidence,
}

/// One step in a reachability trace path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceStep {
    /// Entry root that started the path.
    Entry {
        /// Entry node id.
        entry: EntryId,
        /// Human-readable label.
        label: String,
    },
    /// Traversal through a project file.
    File {
        /// Graph file id.
        file: FileId,
        /// Root-relative path.
        path: String,
    },
    /// Import edge to the next file.
    Import {
        /// Imported module name.
        module: String,
        /// 1-based source line.
        line: u32,
    },
    /// Plugin configuration module reference.
    PluginRef {
        /// Referenced module name.
        module: String,
        /// Human-readable origin label.
        label: String,
    },
    /// Literal dynamic import.
    DynamicImport {
        /// Resolved module name.
        module: String,
        /// 1-based source line.
        line: u32,
    },
}

/// Shortest known path from an entry root to a target file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracePath {
    /// Target file id.
    pub target: FileId,
    /// Steps from entry to target (inclusive).
    pub steps: Vec<TraceStep>,
}

/// Internal predecessor link for BFS and `--trace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReachPredecessor {
    /// Parent file on the shortest known path.
    pub from: Option<FileId>,
    /// How this file was first reached.
    pub step: TraceStep,
}

/// Outcome of reachability analysis.
#[allow(clippy::partial_pub_fields)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityReport {
    /// Files reachable from entry roots or framework globs.
    pub reachable: IndexSet<FileId>,
    /// Candidate unused files with confidence metadata.
    pub unreachable: Vec<UnreachableFile>,
    /// Stdlib and third-party modules seen during traversal (Step 10 input).
    pub used_modules: Vec<UsedModule>,
    /// Files matched by framework-used globs.
    pub framework_used: IndexSet<FileId>,
    /// Shortest-path predecessors for [`super::trace::trace_to_file`].
    pub(super) predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
}

impl ReachabilityReport {
    /// Empty report for unit tests and early pipeline stages.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            reachable: IndexSet::new(),
            unreachable: Vec::new(),
            used_modules: Vec::new(),
            framework_used: IndexSet::new(),
            predecessors: indexmap::IndexMap::new(),
        }
    }
}
