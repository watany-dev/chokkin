//! Borrowed inputs shared by dependency and symbol analysis.
use crate::{
    graph::ProjectGraph, parser::ParseSummary, reachability::ReachabilityReport,
    resolver::ResolutionIndex, sources::DiscoveredSources,
};

/// Earlier pipeline outputs read by dependency (step 10) and symbol (step 11) rules.
pub struct RuleContext<'a> {
    /// Step 7 import resolution.
    pub resolution: &'a ResolutionIndex,
    /// Step 9 reachability.
    pub reachability: &'a ReachabilityReport,
    /// Project graph after resolution and entry wiring.
    pub graph: &'a ProjectGraph,
    /// Discovered source files.
    pub sources: &'a DiscoveredSources,
    /// Step 6 parse output.
    pub parse: &'a ParseSummary,
}

/// Settings specific to dependency rules; symbol rules do not consume them.
pub struct DependencyRuleContext<'a> {
    /// Shared rule inputs.
    pub rules: &'a RuleContext<'a>,
    /// Effective chokkin configuration.
    pub config: &'a crate::config::ChokkinConfig,
    /// Strict mode (`--strict`).
    pub strict: bool,
}
