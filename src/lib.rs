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
mod parallel;
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
    AnalysisReport, analyze_project, probe_project, trace_output, write_diagnostics,
    write_probe_report, write_probe_warnings,
};
pub use reporters::{
    RenderContext, config_label_from_sources, error_chain, escape_control, escape_control_lines,
    render_baseline_report, render_fix_report, render_issues,
};
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
        DependencyContext, DependencyOrigin, ExtrasState, LoadedManifest, LockfileKind,
        ManifestError, ManifestWarning, discover_inline_scripts, extract_manifest,
        extract_manifest_with_cache, resolve_target_version,
    };
    pub use crate::parser::{
        Exports, ImportContext, ImportKind, ParseSeverity, ParseSummary, ParsedModule,
        extract_ignores, parse_file, parse_project_sources_with_cache,
    };
    pub use crate::pipeline::{
        AnalyzeOptions, ProbeWarning, SkipReason, WorkspaceMemberInputs, diagnostic_lines,
    };
    pub use crate::plugins::{
        PluginContribution, PluginExtractRequest, PluginHints, PluginsWarning,
        extract_plugin_hints_with_parse,
    };
    pub use crate::reachability::{
        ReachabilityReport, TracePath, TraceStep, UsedModule, analyze_reachability,
        apply_public_surface, trace_to_file,
    };
    pub use crate::reporters::{Diagnostic, FileCounts, ReporterId};
    pub use crate::resolver::{
        ImportMap, ResolutionIndex, ResolveConfidence, ResolveWarning, ScopedDeclarations,
        StdlibRange, VenvIndex, apply_resolution_to_graph, build_binary_map, import_root,
        resolve_imports_for_analysis,
    };
    pub use crate::rules::deps::reconcile_with_context;
    pub use crate::rules::symbols::analyze_with_context;
    pub use crate::rules::{
        DependencyReport, DependencyRuleContext, Gap, Issue, IssueCandidate, IssueLocation,
        IssueReport, IssueSubject, IssueSummary, MemberManifest, Origin, RuleContext, RuleId,
        Severity, SuppressReason, SuppressedIssue, WorkspaceDependencyBoundary, collect_gaps,
        emit_issues, issue_fingerprint,
    };
    pub use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, LocalTrees,
        ProjectLayout, PublicSurface, SourcesError, SourcesWarning, discover_sources,
    };

    /// Case count for a proptest block tuned to `tuned` cases.
    ///
    /// `ProptestConfig::with_cases` ignores `PROPTEST_CASES`, so a tuned block
    /// scales by `PROPTEST_CASES / 256` (proptest's default) instead: the
    /// nightly `make pbt-deep` run deepens every block, tuned or not (#861).
    pub fn proptest_cases(tuned: u32) -> u32 {
        crate::scale_cases(tuned, std::env::var("PROPTEST_CASES").ok().as_deref())
    }
}

/// `tuned` scaled by `cases / 256`, at least 1; unset or unparsable `cases`
/// leaves it as is.
fn scale_cases(tuned: u32, cases: Option<&str>) -> u32 {
    let cases = cases
        .and_then(|cases| cases.parse::<u64>().ok())
        .unwrap_or(256);
    u32::try_from(u64::from(tuned) * cases / 256).map_or(u32::MAX, |scaled| scaled.max(1))
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

    #[test]
    fn tuned_proptest_cases_scale_with_proptest_cases() {
        assert_eq!(scale_cases(40, None), 40);
        assert_eq!(scale_cases(40, Some("256")), 40);
        assert_eq!(scale_cases(40, Some("4096")), 640);
        assert_eq!(scale_cases(40, Some("64")), 10);
        assert_eq!(scale_cases(40, Some("1")), 1);
        assert_eq!(scale_cases(40, Some("deep")), 40);
        assert_eq!(scale_cases(u32::MAX, Some("4096")), u32::MAX);
        let env = std::env::var("PROPTEST_CASES").ok();
        assert_eq!(
            internals::proptest_cases(40),
            scale_cases(40, env.as_deref())
        );
    }
}
