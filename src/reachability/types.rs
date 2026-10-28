//! Reachability analysis types (pipeline step 9).

use std::collections::{HashMap, HashSet};

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::graph::{FileId, ModuleOrigin};

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
    /// Upper bound on issue confidence for Step 12.
    pub max_confidence: Confidence,
    /// Mode the file was scored under; a library workspace member's own
    /// mode can differ from the project's, and a library file its wheel does
    /// not ship is scored as in app mode (R-05).
    pub mode: ProjectMode,
    /// Why an app member's orphan is scored as a library orphan: the wheel of
    /// a member a metapackage root distributes ships it (#1060), for
    /// `--trace` and `--explain`.
    pub confidence_reason: Option<String>,
    /// A test-context file that is neither a conftest nor named like a test
    /// module, in a directory with no reachable file: input a test harness
    /// loads by path or directory convention rather than a helper the tests
    /// beside it forgot to import (#1268).
    pub test_fixture: bool,
    /// Where a config file of the file's own package names one of its public
    /// classes or its dotted module path (`config.yml:1051` names
    /// `TaskHandlerWithCustomFormatter`): users select it in their own
    /// settings, which the tree does not hold (#1258).
    pub config_mention: Option<String>,
}

impl UnreachableFile {
    /// Whether a caller of the library may import the file: an orphan still
    /// judged as library after the wheel surfaces re-scored the rest (R-05).
    #[must_use]
    pub(crate) fn callers_may_import(&self) -> bool {
        self.mode == ProjectMode::Library
    }
}

/// One step in a reachability trace path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceStep {
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
#[expect(
    clippy::partial_pub_fields,
    reason = "the result sets are public data; the private fields are lookup state built from them"
)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReachabilityReport {
    /// Files reachable from entry roots or framework globs.
    pub reachable: IndexSet<FileId>,
    /// Reachable files that some path reaches without passing a
    /// function-local or `TYPE_CHECKING` import (#610). Imports out of test,
    /// docs and dev files reach only library files here (#614).
    pub eager: IndexSet<FileId>,
    /// Eager files that some path reaches without passing an optional
    /// import either (#614), counting paths that start at a library
    /// subpackage's public surface or in a test, docs or dev file.
    pub exposed: IndexSet<FileId>,
    /// Exposed files that some such path reaches from a root the package or
    /// its host loads (#802).
    pub certain: IndexSet<FileId>,
    /// Candidate unused files with confidence metadata.
    pub unreachable: Vec<UnreachableFile>,
    /// Skipped files no root reaches, scored like [`Self::unreachable`] but
    /// never reported: nothing was read from them (#550).
    pub unread: Vec<UnreachableFile>,
    /// Stdlib and third-party modules seen during traversal (Step 10 input).
    pub used_modules: Vec<UsedModule>,
    /// Files matched by framework-used globs.
    pub framework_used: IndexSet<FileId>,
    /// Shortest-path predecessors for [`super::trace::trace_to_file`].
    pub(super) predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
    /// The file each plugin module reference, keyed by its origin file and
    /// module, resolves to: the one the host loads under that name (#779).
    pub(crate) plugin_ref_files: HashMap<(String, String), String>,
    /// Files a reached file names in a list of module names, which a loader
    /// imports by name and may then call by a conventional name (#1257).
    pub(crate) listed_files: HashSet<String>,
}
