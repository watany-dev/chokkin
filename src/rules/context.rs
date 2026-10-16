//! Borrowed inputs shared by dependency and symbol analysis.
use std::collections::{BTreeMap, BTreeSet};

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
    /// Root-relative build script → normalized `[build-system].requires` of
    /// its manifest (#735).
    pub build_scripts: &'a BTreeMap<String, BTreeSet<String>>,
    /// The root's workspace members and local `[tool.uv.sources]` trees.
    pub local: &'a crate::sources::LocalTrees,
}
