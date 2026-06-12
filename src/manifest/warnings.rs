//! Non-fatal warnings emitted during manifest extraction.

/// Warning that does not prevent manifest extraction from completing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestWarning {
    /// `setup.py` could not be parsed statically; dependencies were skipped.
    SetupPyNotStatic {
        /// Root-relative path to `setup.py`.
        file: String,
    },
    /// Poetry manifest sections were detected but are not supported in v0.1.
    PoetryDetected,
    /// PDM manifest sections were detected but are not supported in v0.1.
    PdmDetected,
    /// Hatch manifest sections were detected but are not supported in v0.1.
    HatchDetected,
    /// A PEP 508 requirement line could not be parsed.
    InvalidRequirementLine {
        /// Root-relative file path.
        file: String,
        /// 1-based line number.
        line: u32,
        /// Raw line content.
        raw: String,
    },
}
