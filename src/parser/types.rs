//! Parsed Python module types.

/// Whether an import came from `import` or `from … import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// `import module`.
    Import,
    /// `from module import name`.
    ImportFrom,
}

/// One import statement extracted from a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRef {
    /// Imported module name (may be empty for unresolvable relative imports).
    pub module: String,
    /// 1-based source line.
    pub line: u32,
    /// Import statement kind.
    pub kind: ImportKind,
}

/// Severity of a non-fatal parse diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseSeverity {
    /// Syntax or unsupported construct.
    Error,
    /// Recoverable warning.
    Warning,
}

/// Non-fatal parse diagnostic; analysis continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// 1-based line number when known.
    pub line: u32,
    /// Human-readable message.
    pub message: String,
    /// Diagnostic severity.
    pub severity: ParseSeverity,
}

/// Result of parsing one `.py` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModule {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// Extracted import references.
    pub imports: Vec<ImportRef>,
    /// Non-fatal parse issues.
    pub diagnostics: Vec<ParseDiagnostic>,
}
