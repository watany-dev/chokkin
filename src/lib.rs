#![allow(clippy::multiple_crate_versions)] // pep508_rs depends on thiserror 1.x

//! `chokkin` finds unused files, dependencies, and public symbols in Python
//! projects by building a project-wide reachability graph.
//!
//! Pipeline steps 1–5 ([`discovery`], [`config`], [`manifest`], [`sources`],
//! [`plugins`]) are available as library APIs. Steps 1–4 also run via
//! [`pipeline::probe_project`]; the full pipeline runs via
//! [`pipeline::analyze_project`]. Step 6 ([`parser`]) parses Python sources.
//! Step 7 ([`resolver`]) resolves imports to stdlib / first-party / third-party
//! distributions. Step 8 ([`entry`]) builds entry roots for reachability.
//! Step 9 ([`reachability`]) computes reachable files from entry roots.
//! Step 10 ([`rules`]) reconciles declared dependencies against usage.
//! Step 11 ([`rules`]) analyzes public symbol usage and unresolved imports.
//! Step 12 ([`rules`]) emits filtered issues with exit status.
//! Step 13 ([`fix`]) applies safe manifest edits when requested.
//! See `docs/dev/spec.ja.md` for the full specification.

pub mod cli;
pub mod baseline;
pub mod cache;
pub mod config;
pub mod discovery;
pub mod entry;
pub mod fix;
pub mod graph;
pub mod manifest;
pub mod parser;
pub mod pipeline;
pub mod plugins;
pub mod reachability;
pub mod reporters;
pub mod resolver;
pub mod rules;
pub mod sources;

pub use cli::{CliArgs, parse_cli_args};
pub use baseline::{
    BaselineEntry, BaselineError, BaselineFile, BaselineReport, apply_baseline, write_baseline,
};
pub use cache::{
    CacheKeyContext, CacheOptions, DEFAULT_CACHE_DIR, ParseCacheKey, ParseCacheStats,
    ParseCacheStore, SCAN_CACHE_SCHEMA_VERSION, ScanCacheKey, ScanCacheRecord,
    ScanInputFingerprints, SourceFingerprint,
};
pub use config::{
    ChokkinConfig, Confidence, ConfigError, ConfigSources, DependencyGroupsConfig, EntrySpec,
    LoadedConfig, PluginId, ProjectMode, RuntimeOverrides, TargetVersion, UvWorkspaceHint,
    WorkspaceOverride, apply_overrides, default_config, load_config,
};
pub use discovery::{DiscoveryError, ProjectRoot, RootMarker, discover_project_root};
pub use entry::{
    EntryError, EntryOrigin, EntryPlan, EntryRoot, EntryWarning, ResolvedMode, apply_entry_plan,
    build_entry_roots,
};
pub use fix::{
    AppliedFix, FixError, FixOptions, FixReport, SkippedFix, SkippedReason, apply_fixes,
};
pub use graph::{
    DistributionId, DistributionNode, EntryId, EntryNode, FileId, FileNode, GraphEdge, GraphError,
    ModuleId, ModuleNode, ModuleOrigin, ProjectGraph, add_parsed_imports, build_graph_skeleton,
};
pub use manifest::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, LoadedManifest,
    LockfileGraph, ManifestError, ManifestSources, ManifestWarning, ProjectMetadata,
    extract_manifest, resolve_target_version,
};
pub use parser::{
    DynamicImport, IgnoreDirective, ImportContext, ImportKind, ImportRef, ParseDiagnostic,
    ParseError, ParseSeverity, ParseSummary, ParsedModule, SymbolDef, SymbolKind, parse_file,
    parse_project_sources, parse_project_sources_with_cache,
};
pub use pipeline::{
    AnalysisReport, AnalyzeError, AnalyzeOptions, ProbeError, ProbeReport, ProbeWarning,
    WorkspaceMemberInputs, analyze_project, probe_project, trace_output, write_probe_report,
    write_probe_warnings,
};
pub use plugins::{
    BinaryUsage, FileContextOverride, FrameworkUsedGlob, ModuleReference, PluginContribution,
    PluginEntry, PluginHints, PluginsError, PluginsWarning, ReferenceOrigin, SymbolReference,
    extract_plugin_hints, extract_plugin_hints_with_cache,
};
pub use reachability::{
    ReachabilityError, ReachabilityReport, TracePath, TraceStep, UnreachableFile,
    UnreachableReason, UsedModule, analyze_reachability, path_to_module, trace_to_file,
};
pub use reporters::{
    CompactReporter, DefaultReporter, GithubReporter, JsonReporter, MarkdownReporter,
    RenderContext, Reporter, ReporterId, SarifReporter, config_label_from_sources, format_subject,
    render_issues,
};
pub use resolver::{
    ResolutionIndex, ResolveConfidence, ResolveError, ResolveWarning, ResolvedImport,
    TransitiveIndex, apply_resolution_to_graph, import_root, resolve_imports,
};
pub use rules::{
    DependencyReport, ExplainData, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, ReconcileDiagnostic, RuleId, Severity, SuppressReason, SuppressedIssue,
    SymbolId, SymbolReport, WorkspaceDependencyBoundary, analyze_symbols, emit_issues,
    explain_issue, reconcile_dependencies,
};
pub use sources::{
    DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    SourcesError, SourcesWarning, discover_sources,
};

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
    fn version_matches_cargo_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::IssuesFound.code(), 1);
        assert_eq!(ExitStatus::UsageError.code(), 2);
        assert_eq!(ExitStatus::InternalError.code(), 3);
    }
}
