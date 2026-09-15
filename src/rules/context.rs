//! Borrowed inputs shared by dependency and symbol analysis.
use crate::{
    graph::ProjectGraph, parser::ParseSummary, reachability::ReachabilityReport,
    resolver::ResolutionIndex, sources::DiscoveredSources,
};

pub struct RuleContext<'a> {
    pub(crate) resolution: &'a ResolutionIndex,
    pub(crate) reachability: &'a ReachabilityReport,
    pub(crate) graph: &'a ProjectGraph,
    pub(crate) sources: &'a DiscoveredSources,
    pub(crate) parse: &'a ParseSummary,
}

/// Settings specific to dependency rules; symbol rules do not consume them.
pub struct DependencyRuleContext<'a> {
    pub(crate) rules: &'a RuleContext<'a>,
    pub(crate) config: &'a crate::config::ChokkinConfig,
    pub(crate) strict: bool,
}
