//! Plugin hint types (pipeline step 5).

use crate::config::{EntrySpec, PluginId};
use crate::sources::FileContext;

use super::warnings::PluginsWarning;

/// Where a plugin discovered a reference (for `--explain` / diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceOrigin {
    /// Root-relative path using `/` separators.
    pub file: String,
    /// 1-based line when known.
    pub line: Option<u32>,
    /// Human-readable label, e.g. `INSTALLED_APPS` or `tool.pytest.ini_options.testpaths`.
    pub label: String,
}

/// Additional entry root from a plugin (§9.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Entry path and optional symbol.
    pub spec: EntrySpec,
    /// Assigned file context.
    pub context: FileContext,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Module name referenced from config (§9.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleReference {
    /// Dotted module name.
    pub module: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// `module:symbol` reference (uvicorn, gunicorn, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReference {
    /// Dotted module name.
    pub module: String,
    /// Symbol within the module.
    pub symbol: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// CLI binary usage (§9.3, CHK008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryUsage {
    /// Binary name, e.g. `pytest` or `uvicorn`.
    pub binary: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Glob of files treated as framework-used (excluded from unused-file candidates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkUsedGlob {
    /// Root-relative glob pattern.
    pub pattern: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Override file context assigned in Step 4 (§10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextOverride {
    /// Root-relative file or glob path.
    pub path: String,
    /// Context to apply.
    pub context: FileContext,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Output from one enabled plugin extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginContribution {
    /// Which plugin produced this contribution.
    pub plugin: PluginId,
    /// Additional entry roots.
    pub entries: Vec<PluginEntry>,
    /// Module references from configuration.
    pub module_refs: Vec<ModuleReference>,
    /// `module:symbol` references.
    pub symbol_refs: Vec<SymbolReference>,
    /// CLI binary usages.
    pub binary_usages: Vec<BinaryUsage>,
    /// Framework-used file globs.
    pub framework_used_globs: Vec<FrameworkUsedGlob>,
    /// File context overrides.
    pub file_context_overrides: Vec<FileContextOverride>,
}

impl PluginContribution {
    /// Empty contribution for a plugin with no findings.
    #[must_use]
    pub fn empty(plugin: PluginId) -> Self {
        Self {
            plugin,
            entries: Vec::new(),
            module_refs: Vec::new(),
            symbol_refs: Vec::new(),
            binary_usages: Vec::new(),
            framework_used_globs: Vec::new(),
            file_context_overrides: Vec::new(),
        }
    }
}

/// Aggregated plugin hints from all enabled extractors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHints {
    /// One record per enabled plugin that ran.
    pub contributions: Vec<PluginContribution>,
    /// CLI binaries discovered from generic config scanning (Phase 1.5 §4.A).
    pub config_binary_usages: Vec<BinaryUsage>,
    /// Distributions used via config without a distinct CLI name.
    pub config_used_distributions: Vec<String>,
    /// Non-fatal warnings from plugin extraction.
    pub warnings: Vec<PluginsWarning>,
}

impl PluginHints {
    /// Iterate all plugin entry roots.
    pub fn entries(&self) -> impl Iterator<Item = &PluginEntry> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.entries.iter())
    }

    /// Iterate all module references.
    pub fn module_refs(&self) -> impl Iterator<Item = &ModuleReference> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.module_refs.iter())
    }

    /// Iterate all binary usages.
    pub fn all_binary_usages(&self) -> impl Iterator<Item = &BinaryUsage> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.binary_usages.iter())
            .chain(self.config_binary_usages.iter())
    }

    /// Distributions marked used from config scanning only.
    pub fn config_used_distributions(&self) -> &[String] {
        &self.config_used_distributions
    }

    /// Iterate all symbol references.
    pub fn symbol_refs(&self) -> impl Iterator<Item = &SymbolReference> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.symbol_refs.iter())
    }

    /// Iterate all framework-used globs.
    pub fn framework_used_globs(&self) -> impl Iterator<Item = &FrameworkUsedGlob> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.framework_used_globs.iter())
    }
}
