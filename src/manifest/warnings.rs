//! Non-fatal warnings emitted during manifest extraction.

use serde::{Deserialize, Serialize};

use crate::path_util::join_rel;

/// Warning that does not prevent manifest extraction from completing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestWarning {
    /// `setup.py` could not be parsed statically; dependencies were skipped.
    SetupPyNotStatic {
        /// Root-relative path to `setup.py`.
        file: String,
    },
    /// Runtime dependencies could not be read statically; CHK003/CHK004 are
    /// downgraded to low-confidence info.
    RuntimeDependenciesUnknown {
        /// Root-relative manifest file declaring the unreadable dependencies.
        file: String,
    },
    /// Poetry manifest sections were detected.
    PoetryDetected,
    /// PDM manifest sections were detected.
    PdmDetected,
    /// Hatch manifest sections were detected.
    HatchDetected,
    /// A PEP 508 requirement line could not be parsed.
    InvalidRequirementLine {
        /// Root-relative file path.
        file: String,
        /// 1-based line number, when the source tracks one.
        line: Option<u32>,
        /// TOML key path or requirements label, e.g. `project.dependencies[2]`.
        label: String,
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
    /// An extra requirements file stopped parsing partway; what came before
    /// the error is kept.
    RequirementsUnreadable {
        /// Root-relative file path.
        file: String,
        /// Why parsing stopped.
        reason: String,
    },
    /// A PEP 735 `{include-group = "..."}` names a group that is not defined.
    DependencyGroupIncludeUndefined {
        /// Root-relative path to `pyproject.toml`.
        file: String,
        /// Group containing the include, as written.
        group: String,
        /// Referenced group name, as written.
        include: String,
    },
    /// PEP 735 `include-group` references form a cycle; expansion stops there.
    DependencyGroupIncludeCycle {
        /// Root-relative path to `pyproject.toml`.
        file: String,
        /// Groups along the cycle, starting and ending with the same group.
        groups: Vec<String>,
    },
    /// A manifest file is neither UTF-8 nor declares a supported PEP 263
    /// coding, so it was left out of extraction.
    FileUndecodable {
        /// Root-relative path to the skipped file.
        file: String,
    },
    /// A PEP 723 script block was ignored; the file stays a plain source.
    InlineScriptInvalid {
        /// Root-relative script path.
        file: String,
        /// Why the block was rejected.
        reason: String,
    },
}

impl ManifestWarning {
    /// The same warning with every file joined onto `base`, the
    /// root-relative directory of the manifest that emitted it.
    #[must_use]
    pub(crate) fn rebased(self, base: &str) -> Self {
        let join = |file: String| join_rel(base, &file);
        match self {
            Self::SetupPyNotStatic { file } => Self::SetupPyNotStatic { file: join(file) },
            Self::RuntimeDependenciesUnknown { file } => {
                Self::RuntimeDependenciesUnknown { file: join(file) }
            },
            Self::InvalidRequirementLine {
                file,
                line,
                label,
                raw,
            } => Self::InvalidRequirementLine {
                file: join(file),
                line,
                label,
                raw,
            },
            Self::SetupPyPartiallyStatic { file, argument } => Self::SetupPyPartiallyStatic {
                file: join(file),
                argument,
            },
            Self::MetadataConflict {
                field,
                kept,
                ignored,
                kept_source,
                ignored_source,
            } => Self::MetadataConflict {
                field,
                kept,
                ignored,
                kept_source: join(kept_source),
                ignored_source: join(ignored_source),
            },
            Self::RequirementsOptionIgnored { file, line, raw } => {
                Self::RequirementsOptionIgnored {
                    file: join(file),
                    line,
                    raw,
                }
            },
            Self::RequirementsUnreadable { file, reason } => Self::RequirementsUnreadable {
                file: join(file),
                reason,
            },
            Self::DependencyGroupIncludeUndefined {
                file,
                group,
                include,
            } => Self::DependencyGroupIncludeUndefined {
                file: join(file),
                group,
                include,
            },
            Self::DependencyGroupIncludeCycle { file, groups } => {
                Self::DependencyGroupIncludeCycle {
                    file: join(file),
                    groups,
                }
            },
            Self::FileUndecodable { file } => Self::FileUndecodable { file: join(file) },
            Self::InlineScriptInvalid { file, reason } => Self::InlineScriptInvalid {
                file: join(file),
                reason,
            },
            // No file to rebase: the member path is printed beside them.
            warning @ (Self::PoetryDetected
            | Self::PdmDetected
            | Self::HatchDetected
            | Self::RequirementsConstraintMissing { .. }) => warning,
        }
    }
}
