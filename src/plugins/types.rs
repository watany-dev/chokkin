//! Plugin hint types (pipeline step 5).

use crate::config::{EntrySpec, PluginId};
use crate::sources::FileContext;
use serde::{Deserialize, Serialize};

use super::warnings::PluginsWarning;

/// Where a plugin discovered a reference (for `--explain` / diagnostics).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleReference {
    /// Dotted module name.
    pub module: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// `.py` file a command runs by path (#797).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptReference {
    /// Root-relative candidates in priority order; the first discovered one
    /// is the script.
    pub paths: Vec<String>,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Python code a command runs (#913), as far as the command shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandTarget {
    /// A `.py` file run by path: root-relative candidates in priority order.
    Script {
        /// Candidates; the first discovered one is the script.
        paths: Vec<String>,
    },
    /// A module run with `-m` (a package executes its `__main__`).
    Module {
        /// Dotted module name.
        module: String,
    },
    /// A module imported by a server or task runner.
    App {
        /// Dotted module name.
        module: String,
        /// Explicit attribute, or the app names the runner looks up by default.
        symbols: Vec<String>,
        /// A submodule the runner imports when `module` is a package without
        /// the app: Celery falls back to `<package>.celery`.
        fallback: Option<String>,
    },
    /// A possible import read heuristically from `python -c`; this is gap
    /// evidence, not a statically resolved entry (#913).
    CodeModule {
        /// Dotted module name.
        module: String,
    },
    /// A target the command computes (a variable or substitution) or names
    /// in a shape that is not read.
    Opaque {
        /// The word as written.
        text: String,
    },
}

/// One [`CommandTarget`] and the command that runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandTargetRef {
    /// What the command runs.
    pub target: CommandTarget,
    /// The command's file and line.
    pub origin: ReferenceOrigin,
}

/// A config file imports a first-party module whose `sys.path` edits apply to
/// the config's own imports, as Sphinx's `conf.py` does with a shared
/// constants module (#779).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysPathImport {
    /// Root-relative path of the importing file.
    pub importer: String,
    /// Root-relative path of the imported module.
    pub imported: String,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryUsage {
    /// Binary name, e.g. `pytest` or `uvicorn`.
    pub binary: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
    /// The usage is a tool's config section (`[tool.ruff]`, `[mypy]`) or a
    /// remote pre-commit hook rather than a command run in the project's
    /// environment, so a remote hook providing the binary covers it (#680).
    #[serde(default)]
    pub config_section: bool,
}

/// Glob of files treated as framework-used (excluded from unused-file candidates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkUsedGlob {
    /// Root-relative glob pattern.
    pub pattern: String,
    /// Discovery origin.
    pub origin: ReferenceOrigin,
}

/// Configuration a framework reads that the plugin could not find or read,
/// such as the Django settings (#914): what it would have explained is
/// unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadSettings {
    /// Root-relative file the gap is in: the settings module, or the file
    /// that names it.
    pub file: String,
    /// What in `file` could not be found or read and its line, which
    /// `--explain` shows in place of the file: a settings module or star
    /// import, or `DJANGO_SETTINGS_MODULE` set in a form not read.
    pub site: Option<(String, Option<u32>)>,
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
    /// Symbols a tool looks up by name in a module it was given, such as
    /// Celery's `app` in the module `celery -A` names (#851). They are used,
    /// but unlike `symbol_refs` add no entry root: the module reference does.
    pub used_symbols: Vec<SymbolReference>,
    /// CLI binary usages.
    pub binary_usages: Vec<BinaryUsage>,
    /// Framework-used file globs.
    pub framework_used_globs: Vec<FrameworkUsedGlob>,
    /// Imports whose `sys.path` edits the importer's imports see.
    pub sys_path_imports: Vec<SysPathImport>,
    /// Settings the framework reads that could not be found or read; the
    /// gap stage reads it (#914).
    pub unread_settings: Vec<UnreadSettings>,
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
            used_symbols: Vec::new(),
            binary_usages: Vec::new(),
            framework_used_globs: Vec::new(),
            sys_path_imports: Vec::new(),
            unread_settings: Vec::new(),
        }
    }
}

/// Aggregated plugin hints from all enabled extractors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginHints {
    /// One record per enabled plugin that ran.
    pub contributions: Vec<PluginContribution>,
    /// CLI binaries discovered from generic config scanning (Phase 1.5 §4.A).
    pub config_binary_usages: Vec<BinaryUsage>,
    /// Distributions used via config without a distinct CLI name.
    pub config_used_distributions: Vec<String>,
    /// Distributions that tox `deps` and nox `session.install(...)` install
    /// into their own environments; CHK008 counts them as declared (#680).
    pub config_declared_distributions: Vec<String>,
    /// Binaries remote pre-commit hooks install into pre-commit's own
    /// environment; they cover `BinaryUsage::config_section` usages (#680).
    pub config_provided_binaries: Vec<String>,
    /// Module references from generic config scanning (pytest `-p`, mypy plugins, PDM `call`).
    pub config_module_refs: Vec<ModuleReference>,
    /// Scripts CI workflows, local pre-commit hooks and agent skill docs run
    /// by path (#797).
    pub config_script_entries: Vec<ScriptReference>,
    /// Python targets from task files and tool configs: known targets become
    /// entries (#912), unread targets feed the gap stage (#913).
    pub config_command_targets: Vec<CommandTargetRef>,
    /// Non-fatal warnings from plugin extraction.
    pub warnings: Vec<PluginsWarning>,
}

impl PluginHints {
    /// Iterate all module references.
    pub fn module_refs(&self) -> impl Iterator<Item = &ModuleReference> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.module_refs.iter())
            .chain(self.config_module_refs.iter())
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

    /// Iterate all symbols used by name lookup (no entry roots).
    pub fn used_symbols(&self) -> impl Iterator<Item = &SymbolReference> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.used_symbols.iter())
    }

    /// Iterate all imports whose `sys.path` edits the importer sees.
    pub fn sys_path_imports(&self) -> impl Iterator<Item = &SysPathImport> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.sys_path_imports.iter())
    }

    /// Iterate all settings plugins could not find or read.
    pub fn unread_settings(&self) -> impl Iterator<Item = &UnreadSettings> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.unread_settings.iter())
    }

    /// Iterate all framework-used globs.
    pub fn framework_used_globs(&self) -> impl Iterator<Item = &FrameworkUsedGlob> {
        self.contributions
            .iter()
            .flat_map(|contrib| contrib.framework_used_globs.iter())
    }
}
