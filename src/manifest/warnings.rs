//! Non-fatal warnings emitted during manifest extraction.

use serde::{Deserialize, Serialize};

/// Warning that does not prevent manifest extraction from completing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `setup.py` was only partially parsed statically.
    SetupPyPartiallyStatic {
        /// Root-relative path to `setup.py`.
        file: String,
        /// Keyword argument that could not be fully read, e.g. `install_requires`.
        argument: String,
    },
    /// Conflicting metadata values were found across manifest sources.
    MetadataConflict {
        /// Metadata field name, e.g. `requires-python`.
        field: String,
        /// Value kept from the higher-priority source.
        kept: String,
        /// Value ignored from the lower-priority source.
        ignored: String,
        /// Root-relative file for the kept value.
        kept_source: String,
        /// Root-relative file for the ignored value.
        ignored_source: String,
    },
    /// A requirements option line was not recognized and was skipped.
    RequirementsOptionIgnored {
        /// Root-relative file path.
        file: String,
        /// 1-based line number.
        line: u32,
        /// Raw line content.
        raw: String,
    },
    /// A `-c` constraints file reference could not be resolved.
    RequirementsConstraintMissing {
        /// Missing constraints file path as written.
        path: String,
    },
}
