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
}

/// One import statement extracted from a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRef {
    /// Imported module name (normalized dotted name; empty = unresolved relative).
    pub module: String,
    /// `from … import` symbol name when applicable.
    pub name: Option<String>,
    /// Local alias (`as` name).
    pub alias: Option<String>,
    /// 1-based source line.
    pub line: u32,
    /// Import statement kind.
    pub kind: ImportKind,
    /// Import context for dependency rules.
    pub context: ImportContext,
    /// `true` when the import appears inside a `try` block body.
    pub optional: bool,
    /// `true` when the import appears under an `if sys.platform …` guard.
    pub platform_guarded: bool,
    /// Relative import dot count (`0` = absolute).
    pub relative_level: u8,
}

/// A literal dynamic import (`importlib.import_module("…")` or `__import__("…")`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicImport {
    /// Resolved module name from a string literal.
    pub module: String,
    /// 1-based source line.
    pub line: u32,
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
    /// Normalized decorator names (`app.get`, `pytest.fixture`, …).
    pub decorators: Vec<String>,
    /// Defined inside a `TYPE_CHECKING` block.
    pub in_type_checking: bool,
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

/// Result of parsing one `.py` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// Extracted import references.
    pub imports: Vec<ImportRef>,
    /// Literal dynamic imports.
    pub dynamic_imports: Vec<DynamicImport>,
    /// Attribute accesses for `import module; module.name` symbol tracking.
    pub attribute_accesses: Vec<AttributeAccess>,
    /// Top-level symbol definitions.
    pub symbols: Vec<SymbolDef>,
    /// Names listed in `__all__`.
    pub exports: Vec<String>,
    /// Extracted ignore directives.
    pub ignores: Vec<IgnoreDirective>,
    /// Non-literal dynamic import was seen.
    pub has_opaque_dynamic_import: bool,
    /// Non-fatal parse issues.
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Aggregate result of parsing all project `.py` sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseSummary {
    /// One parsed module per `.py` file.
    pub modules: Vec<ParsedModule>,
    /// Successfully parsed files (including those with syntax diagnostics).
    pub parsed_count: u32,
    /// Files with at least one syntax error diagnostic.
    pub error_count: u32,
    /// Skipped files (`.pyi` stubs, etc.).
    pub skipped_count: u32,
}

impl ParsedModule {
    /// Empty parsed module for a path (used when syntax parse fails early).
    #[must_use]
    pub fn empty(path: String) -> Self {
        Self {
            path,
            imports: Vec::new(),
            dynamic_imports: Vec::new(),
            attribute_accesses: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            ignores: Vec::new(),
            has_opaque_dynamic_import: false,
            diagnostics: Vec::new(),
        }
    }
}

impl ParseSummary {
    /// Creates an empty summary.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            modules: Vec::new(),
            parsed_count: 0,
            error_count: 0,
            skipped_count: 0,
        }
    }
}

/// Map file context to the default import context.
#[must_use]
pub const fn import_context_for_file(file_context: FileContext) -> ImportContext {
    match file_context {
        FileContext::Test => ImportContext::Test,
        FileContext::Runtime | FileContext::Docs | FileContext::Dev => ImportContext::Runtime,
    }
}
