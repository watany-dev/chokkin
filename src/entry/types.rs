//! Entry root construction types (pipeline step 8).

use crate::config::{EntrySpec, PluginId, ProjectMode};
use crate::resolver::ResolveConfidence;
use crate::sources::FileContext;

/// Resolved project analysis mode after `mode = auto` inference (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMode {
    /// Effective app / library mode.
    pub mode: ProjectMode,
    /// Confidence in the mode resolution.
    pub confidence: ResolveConfidence,
}

/// Where an entry root was discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOrigin {
    /// `[tool.chokkin].entry` or workspace override.
    Config,
    /// `[project.scripts]` / `[project.gui-scripts]` / `[project.entry-points]`.
    Manifest {
        /// Entry-point name within the group.
        name: String,
        /// `console` | `gui` | other group name.
        group: String,
    },
    /// Plugin extractor (`PluginEntry`).
    Plugin {
        /// Contributing plugin.
        plugin: PluginId,
        /// Human-readable origin label.
        label: String,
    },
    /// §8 automatic path rules (`manage.py`, `__main__.py`, …).
    Auto {
        /// Rule identifier, e.g. `manage.py` or `scripts/**`.
        rule: String,
    },
    /// `module:symbol` reference resolved to a file (uvicorn, gunicorn, …).
    SymbolRef {
        /// Dotted module name.
        module: String,
        /// Symbol within the module.
        symbol: String,
        /// Human-readable origin label.
        label: String,
    },
}

/// One merged entry root after deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRoot {
    /// File path and optional symbol.
    pub spec: EntrySpec,
    /// Assigned file context.
    pub context: FileContext,
    /// Discovery sources merged for this path.
    pub origins: Vec<EntryOrigin>,
}

/// Outcome of entry root construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPlan {
    /// Resolved project mode.
    pub mode: ResolvedMode,
    /// Merged entry roots sorted by path.
    pub roots: Vec<EntryRoot>,
    /// Non-fatal conditions.
    pub warnings: Vec<EntryWarning>,
}

/// Non-fatal entry construction warnings.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryWarning {
    /// Configured or resolved entry path is not in discovered sources.
    MissingEntryPath {
        /// Root-relative path.
        path: String,
    },
    /// A manifest or plugin `module:symbol` target could not be mapped to a file.
    UnresolvedModuleTarget {
        /// Dotted module name.
        module: String,
        /// Human-readable origin.
        origin: String,
    },
    /// Multiple uv workspace members detected; treated as app mode (v0.1).
    WorkspaceMode {
        /// Number of workspace members.
        member_count: usize,
    },
}

/// Candidate entry before merge and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EntryCandidate {
    pub spec: EntrySpec,
    pub context: FileContext,
    pub origin: EntryOrigin,
}
