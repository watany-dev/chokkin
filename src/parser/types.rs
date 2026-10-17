//! Parsed Python module types.

use serde::{Deserialize, Serialize};

use crate::sources::FileContext;

/// Whether an import came from `import` or `from … import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    /// `import module`.
    Import,
    /// `from module import name`.
    ImportFrom,
}

/// Context of an import statement for dependency classification (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportContext {
    /// Normal runtime import.
    Runtime,
    /// Import inside a `TYPE_CHECKING` block.
    Type,
    /// Import in a test file (combined with file context in Step 10).
    Test,
    /// Import in an `if __name__ == "__main__":` block; dev usage only when
    /// the file's context is runtime (#681).
    Dev,
}

/// One import statement extracted from a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRef {
    /// Imported module name (normalized dotted name; empty = unresolved relative).
    pub module: String,
    /// `from … import` symbol name when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Local alias (`as` name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// 1-based source line.
    pub line: u32,
    /// Import statement kind.
    pub kind: ImportKind,
    /// Import context for dependency rules.
    pub context: ImportContext,
    /// `true` when the import appears inside a `try` block body.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// `true` when the import appears under an `if sys.platform …` guard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub platform_guarded: bool,
    /// `true` when the import sits in a function body, so it runs only when
    /// the function is called.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
    /// Relative import dot count (`0` = absolute).
    #[serde(default, skip_serializing_if = "is_zero_level")]
    pub relative_level: u8,
    /// In an `except ImportError:` handler, the roots its `try` body imports;
    /// the handler only runs when one of them is missing (#721).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_for: Vec<String>,
}

impl ImportRef {
    /// `from package import name` as `(package, name)`, before any `as`.
    /// `from . import x` carries no `name`: the parser folds `x` into
    /// `module`, so the package is what precedes its last dot. `None` for
    /// `import module`, and for an unresolved `from . import x`, which loses `x`.
    #[must_use]
    pub(crate) fn import_target(&self) -> Option<(&str, &str)> {
        if self.kind != ImportKind::ImportFrom {
            return None;
        }
        match &self.name {
            Some(name) => Some((&self.module, name)),
            None => self.module.rsplit_once('.'),
        }
    }

    /// The name this import binds in the importing module: the `as` name,
    /// else the top-level package of `import a.b`, else the imported name.
    /// `None` for `from m import *`, which binds no statically known name.
    #[must_use]
    pub(crate) fn bound_name(&self) -> Option<&str> {
        if let Some(alias) = &self.alias {
            return Some(alias);
        }
        match self.kind {
            ImportKind::Import => self.module.split('.').next(),
            ImportKind::ImportFrom => self
                .import_target()
                .map(|(_, name)| name)
                .filter(|&name| name != "*"),
        }
    }
}

/// Serialization helper: `relative_level` is `0` for the common absolute import.
// serde's `skip_serializing_if` hands the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_level(level: &u8) -> bool {
    *level == 0
}

/// A literal dynamic import (`importlib.import_module("…")` or `__import__("…")`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct DynamicImport {
    /// Resolved module name from a string literal.
    pub module: String,
    /// 1-based source line.
    pub line: u32,
    /// Same as [`ImportRef::optional`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Same as [`ImportRef::platform_guarded`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub platform_guarded: bool,
    /// Same as [`ImportRef::deferred`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
    /// Prefix imports only: the same scope binds the loaded module to a name
    /// and reads it with `getattr(module, <computed name>)`, so any class the
    /// loaded modules define may be fetched by name (#728).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub computed_getattr: bool,
}

/// Attribute access against an imported module binding (`module.attr`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeAccess {
    /// Receiver expression as a dotted name (`acme.utils` or local alias).
    pub receiver: String,
    /// Accessed attribute name.
    pub name: String,
    /// 1-based source line.
    pub line: u32,
}

/// Kind of top-level symbol definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    /// `def` / `async def`.
    Function,
    /// `class`.
    Class,
    /// Module-level assignment.
    Variable,
}

/// One decorator occurrence, at any nesting level.
///
/// Plugins need the decorator's own line, which `SymbolDef` cannot carry: it
/// records the definition line and only for module-level symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoratorSite {
    /// Normalized dotted decorator name (`app.route`, `shared_task`, …); a
    /// subscript receiver keeps only its base (`apps[].route`).
    pub name: String,
    /// 1-based line of the decorator itself.
    pub line: u32,
    /// Written as a call (`@app.route("/")`) rather than bare (`@app.route`).
    pub is_call: bool,
}

/// A top-level symbol definition for Step 11.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolDef {
    /// Symbol name.
    pub name: String,
    /// Definition kind.
    pub kind: SymbolKind,
    /// 1-based definition line.
    pub line: u32,
    /// Whether the symbol is considered public.
    pub is_public: bool,
    /// Normalized names of every statically named decorator (`app.get`,
    /// `functools.lru_cache`, …); consumers pick the ones they recognize.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    /// Defined inside a `TYPE_CHECKING` block.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_type_checking: bool,
    /// The name is read somewhere in its own module (annotations included).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub used_in_module: bool,
}

/// Inline or file-level ignore directive (§18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoreDirective {
    /// `true` for `# chokkin: file-ignore[…]` at file head.
    pub file_level: bool,
    /// Rule codes such as `CHK003`.
    pub codes: Vec<String>,
    /// 1-based line number (`0` for file-level).
    pub line: u32,
}

/// Severity of a non-fatal parse diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseSeverity {
    /// Syntax or unsupported construct.
    Error,
    /// Recoverable warning.
    Warning,
}

/// Non-fatal parse diagnostic; analysis continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseDiagnostic {
    /// 1-based line number when known.
    pub line: u32,
    /// Human-readable message.
    pub message: String,
    /// Diagnostic severity.
    pub severity: ParseSeverity,
}

/// What a module's `__all__` reads as (ADR 0005).
///
/// The parser reads literal lists and literal additions; every other way of
/// binding or growing `__all__` makes what it read incomplete instead of
/// being dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "names", rename_all = "snake_case")]
pub enum Exports {
    /// The module never binds `__all__`.
    #[default]
    Absent,
    /// Every assignment and addition was read.
    Known(Vec<String>),
    /// A literal list was read, but later additions or a rebinding in a
    /// block were not: `__all__` has these names and possibly others.
    Partial(Vec<String>),
    /// `__all__` was bound in a form the parser cannot read.
    Unknown,
}

impl Exports {
    /// Names read from a literal `__all__`; empty when there is no list to
    /// read, which consumers treat like a module without `__all__`.
    #[must_use]
    pub fn names(&self) -> &[String] {
        match self {
            Self::Known(names) | Self::Partial(names) => names,
            Self::Absent | Self::Unknown => &[],
        }
    }

    /// Whether `name` is in [`Self::names`].
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names().iter().any(|export| export == name)
    }

    /// Whether `from m import *` may bind `name` from this module: a list the
    /// parser could not read whole may hold any name (#951), and an empty
    /// one counts as none, like a module without `__all__`.
    #[must_use]
    pub fn may_hand_on(&self, name: &str) -> bool {
        match self {
            Self::Known(names) if !names.is_empty() => names.iter().any(|export| export == name),
            Self::Partial(_) | Self::Unknown => true,
            Self::Known(_) | Self::Absent => !name.starts_with('_'),
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// The state after an update the parser could not read: a read list
    /// keeps its names but may miss others, and no list stays unreadable.
    pub(super) fn lose(&mut self) {
        *self = match std::mem::take(self) {
            Self::Known(names) | Self::Partial(names) => Self::Partial(names),
            Self::Absent | Self::Unknown => Self::Unknown,
        };
    }
}

/// A string literal that names a module by its dotted name (#1026):
/// `"pkg.mod"`, `"pkg.mod.attr"` or `"python:pkg.mod"`, as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleRef {
    /// The literal as written, `python:` prefix included.
    pub text: String,
    /// 1-based line of the literal's first occurrence in the module.
    pub line: u32,
}

/// Result of parsing one `.py` file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ParsedModule {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// Extracted import references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ImportRef>,
    /// Literal dynamic imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_imports: Vec<DynamicImport>,
    /// Packages whose submodules a dynamic import loads by a name built from a
    /// literal prefix (`import_module("pkg.commands." + name)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_import_prefixes: Vec<DynamicImport>,
    /// Module names a module-level `pytest_plugins` lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pytest_plugins: Vec<DynamicImport>,
    /// Attribute accesses for `import module; module.name` symbol tracking,
    /// only those through a receiver one of `imports` binds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribute_accesses: Vec<AttributeAccess>,
    /// Top-level symbol definitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolDef>,
    /// Statically named decorators seen anywhere in the module.
    pub decorator_sites: Vec<DecoratorSite>,
    /// What `__all__` reads as.
    #[serde(default, skip_serializing_if = "Exports::is_absent")]
    pub exports: Exports,
    /// Names bound by an import and read in this module (`from .x import f; f()`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_import_bindings: Vec<String>,
    /// Extracted ignore directives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignores: Vec<IgnoreDirective>,
    /// Non-literal dynamic import was seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_opaque_dynamic_import: bool,
    /// Runs another Python file with `sys.executable`, in this interpreter's
    /// environment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runs_python_file: bool,
    /// In a module that imports `subprocess`, the first words of string
    /// literals that look like command lines: programs it may run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell_commands: Vec<String>,
    /// In a module that edits `sys.path`, string literals that may name the
    /// added directory, as `/`-separated relative paths (`""` is the
    /// module's own directory) (#719).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sys_path_hints: Vec<String>,
    /// A module-level `warnings.warn` with a deprecation category: importing
    /// the module is deprecated, so it is a compatibility shim kept for
    /// outside callers (#778).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated_shim: bool,
    /// A module-level `if __name__ == "__main__":` block: the file is run as
    /// a script, so it is an entry root (#1026).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_main_block: bool,
    /// String literals that are whole dotted module names, which may load
    /// the module they name (#1026): each distinct literal at its first line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub module_refs: Vec<ModuleRef>,
    /// Non-fatal parse issues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ParseDiagnostic>,
    /// The source could not be decoded, has a syntax error, or is a notebook
    /// without readable cells, so nothing was extracted from it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    /// A notebook was read, but some cells that may be code were not (#898),
    /// so what the module imports is partly unknown (#902).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_unread_cells: bool,
}

/// Aggregate result of parsing all project `.py` sources.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseSummary {
    /// One parsed module per `.py` file.
    pub modules: Vec<ParsedModule>,
}

/// Map file context to the default import context.
#[must_use]
pub(crate) const fn import_context_for_file(file_context: FileContext) -> ImportContext {
    match file_context {
        FileContext::Test => ImportContext::Test,
        FileContext::Runtime | FileContext::Docs | FileContext::Dev => ImportContext::Runtime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(
        kind: ImportKind,
        module: &str,
        name: Option<&str>,
        alias: Option<&str>,
    ) -> ImportRef {
        ImportRef {
            module: module.to_owned(),
            name: name.map(str::to_owned),
            alias: alias.map(str::to_owned),
            line: 1,
            kind,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            deferred: false,
            relative_level: 0,
            fallback_for: Vec::new(),
        }
    }

    #[test]
    fn import_target_splits_folded_relative_imports() {
        use ImportKind::{Import, ImportFrom};
        let cases = [
            (
                import(ImportFrom, "pkg.core", Some("helper"), None),
                Some(("pkg.core", "helper")),
            ),
            (
                import(ImportFrom, "pkg.core", Some("tool"), Some("t")),
                Some(("pkg.core", "tool")),
            ),
            (
                import(ImportFrom, "pkg.other", Some("*"), None),
                Some(("pkg.other", "*")),
            ),
            // `from . import sub` in `pkg`.
            (
                import(ImportFrom, "pkg.sub", None, None),
                Some(("pkg", "sub")),
            ),
            // An unresolved `from .m import x` keeps `x`; `from . import x` loses it.
            (import(ImportFrom, "", Some("x"), None), Some(("", "x"))),
            (import(ImportFrom, "", None, None), None),
            (import(Import, "os.path", None, None), None),
        ];
        for (import, expected) in cases {
            assert_eq!(import.import_target(), expected, "{import:?}");
        }
    }

    #[test]
    fn bound_name_prefers_alias_and_skips_star_imports() {
        use ImportKind::{Import, ImportFrom};
        let cases = [
            (
                import(ImportFrom, "pkg.core", Some("helper"), None),
                Some("helper"),
            ),
            (
                import(ImportFrom, "pkg.core", Some("tool"), Some("t")),
                Some("t"),
            ),
            (import(ImportFrom, "pkg.other", Some("*"), None), None),
            (import(ImportFrom, "pkg.sub", None, None), Some("sub")),
            (import(ImportFrom, "pkg.sub", None, Some("s")), Some("s")),
            (import(ImportFrom, "", None, Some("s")), Some("s")),
            (import(ImportFrom, "", None, None), None),
            (import(Import, "os.path", None, None), Some("os")),
            (import(Import, "json", None, Some("js")), Some("js")),
        ];
        for (import, expected) in cases {
            assert_eq!(import.bound_name(), expected, "{import:?}");
        }
    }

    #[test]
    fn exports_survive_the_cache_and_absent_is_left_out() {
        let names = vec!["f".to_owned()];
        for exports in [
            Exports::Absent,
            Exports::Known(names.clone()),
            Exports::Partial(names),
            Exports::Unknown,
        ] {
            let module = ParsedModule {
                exports,
                ..ParsedModule::default()
            };
            let json = serde_json::to_string(&module).expect("serialize");
            assert_eq!(
                json.contains("\"exports\""),
                module.exports != Exports::Absent,
                "{json}"
            );
            let back: ParsedModule = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, module);
        }
    }
}
