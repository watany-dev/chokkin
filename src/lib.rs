//! `chokkin` finds unused files, dependencies, and public symbols in Python
//! projects by building a project-wide reachability graph.
//!
//! chokkin ships as a CLI; the library exists to keep `main.rs` a thin
//! dispatcher and is not a public API (ADR 0004). Pipeline modules are
//! crate-private so rustc's `dead_code` lint covers them.
//! See `docs/dev/spec.ja.md` for the full specification.

mod baseline;
mod cache;
mod cli;
mod config;
mod discovery;
mod entry;
mod fix;
mod graph;
mod init;
mod manifest;
mod parser;
mod path_util;
mod pipeline;
mod plugins;
mod reachability;
mod reporters;
mod resolver;
mod rules;
mod sources;

pub use baseline::BaselineReport;
pub use cli::{CliArgs, parse_cli_args};
pub use config::RuntimeOverrides;
pub use init::init_project;
pub use pipeline::{
    AnalysisReport, analyze_project, probe_project, trace_output, write_probe_report,
    write_probe_warnings,
};
pub use reporters::{RenderContext, config_label_from_sources, render_fix_report, render_issues};
pub use rules::explain_issue;

/// Items re-exported only for `tests/` and `benches/`; not a stable API.
#[doc(hidden)]
pub mod internals {
    pub use crate::baseline::{apply_baseline, write_baseline};
    pub use crate::cache::{
        CacheKeyHasher, CacheOptions, SourceFingerprint, stable_hex_hash, stable_list_hash,
    };
    pub use crate::config::{
        ChokkinConfig, Confidence, ConfigError, ConfigSources, EntrySpec, LoadedConfig, PluginId,
        ProjectMode, SeverityLevel, TargetVersion, default_config, load_config,
    };
    pub use crate::discovery::{DiscoveryError, ProjectRoot, RootMarker, discover_project_root};
    pub use crate::entry::{EntryOrigin, EntryPlan, EntryWarning, build_entry_roots};
    pub use crate::graph::{
        GraphEdge, GraphError, ModuleOrigin, ProjectGraph, add_parsed_imports, build_graph_skeleton,
    };
    pub use crate::manifest::{
        DependencyContext, DependencyOrigin, LoadedManifest, LockfileKind, ManifestError,
        ManifestWarning, discover_inline_scripts, extract_manifest, extract_manifest_with_cache,
        resolve_target_version,
    };
    pub use crate::parser::{
        ImportContext, ImportKind, ParseSeverity, ParseSummary, ParsedModule, extract_ignores,
        parse_file, parse_project_sources_with_cache,
    };
    pub use crate::pipeline::{AnalyzeOptions, WorkspaceMemberInputs};
    pub use crate::plugins::{
        PluginContribution, PluginExtractRequest, PluginHints, PluginsWarning,
        extract_plugin_hints_with_parse,
    };
    pub use crate::reachability::{
        ReachabilityReport, TracePath, TraceStep, UsedModule, analyze_reachability,
        apply_public_surface, trace_to_file,
    };
    pub use crate::reporters::{FileCounts, ReporterId};
    pub use crate::resolver::{
        ImportMap, ResolutionIndex, ResolveConfidence, ResolveWarning, ScopedDeclarations,
        StdlibRange, VenvIndex, apply_resolution_to_graph, build_binary_map, import_root,
        resolve_imports_for_analysis,
    };
    pub use crate::rules::deps::reconcile_with_context;
    pub use crate::rules::symbols::analyze_with_context;
    pub use crate::rules::{
        DependencyReport, DependencyRuleContext, Issue, IssueCandidate, IssueLocation, IssueReport,
        IssueSubject, IssueSummary, Origin, RuleContext, RuleId, Severity, SuppressReason,
        SuppressedIssue, WorkspaceDependencyBoundary, emit_issues, issue_fingerprint,
    };
    pub use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
        PublicSurface, SourcesError, SourcesWarning, discover_sources,
    };
}

/// The version of chokkin, taken from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit codes reported by the CLI, fixed for CI usage.
///
/// ```text
/// 0: no reportable issues
/// 1: issues found
/// 2: CLI/config error
/// 3: internal error
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitStatus {
    /// No reportable issues.
    Success = 0,
    /// Reportable issues were found.
    IssuesFound = 1,
    /// Invalid CLI invocation or configuration.
    UsageError = 2,
    /// Unexpected internal failure.
    InternalError = 3,
}

impl ExitStatus {
    /// Returns the numeric process exit code.
    pub fn code(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::IssuesFound.code(), 1);
        assert_eq!(ExitStatus::UsageError.code(), 2);
        assert_eq!(ExitStatus::InternalError.code(), 3);
    }
}
