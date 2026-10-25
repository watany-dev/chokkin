//! Non-fatal warnings emitted during manifest extraction.

use serde::{Deserialize, Serialize};

use crate::path_util::join_rel;

use super::types::DependencyContext;

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
        /// Where the line would have declared its distribution: a runtime
        /// line may be the declaration CHK005 misses (#935).
        context: DependencyContext,
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
    /// A path a manifest names resolves outside the project root, so what it
    /// points at was not read (#981).
    PathOutsideRoot {
        /// Root-relative file that names the path.
        file: String,
        /// Where in `file` the path is written (`-c`, `tool.uv.sources.<name>`).
        field: String,
        /// The path as written.
        path: String,
    },
    /// A root `[tool.uv.sources]` path inside the root names no directory,
    /// so it is no member (#1000).
    PathSourceMissing {
        /// Root-relative manifest that names the path.
        file: String,
        /// `tool.uv.sources.<name>`.
        field: String,
        /// The path as written.
        path: String,
    },
    /// A root `[tool.uv.sources]` directory inside the root has no
    /// `pyproject.toml` with a `[project]` table, so it is no member: its code
    /// is analyzed as the root's, without its own entries or declarations
    /// (#1000).
    PathSourceNotProject {
        /// Root-relative manifest that names the path.
        file: String,
        /// `tool.uv.sources.<name>`.
        field: String,
        /// The path as written.
        path: String,
    },
    /// A `-r` / `-c` names a URL or a `${VAR}` path, which pip fetches or
    /// expands from the environment and chokkin does not read (#1130). A
    /// `-r` marks its file as partly read.
    RequirementsIncludeNotRead {
        /// Root-relative file path.
        file: String,
        /// 1-based line number.
        line: u32,
        /// `-r` or `-c`.
        option: String,
        /// The path as written.
        path: String,
        /// What pip would do with it, e.g. `a URL`.
        reason: String,
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
    /// `dynamic` lists `optional-dependencies`, and these extras could not
    /// be read statically (#905). Empty when no source names any extra.
    DynamicExtrasUnread {
        /// Root-relative path to `pyproject.toml`.
        file: String,
        /// Extras named but filled from a source chokkin does not read.
        extras: Vec<String>,
    },
    /// `requires-python` is not a specifier set, so the target version fell
    /// back to py311 (#917). Not emitted when `target_version` is explicit.
    RequiresPythonUnreadable {
        /// Root-relative manifest the value came from.
        file: String,
        /// `requires-python` as written.
        value: String,
    },
    /// A PEP 723 script's `requires-python` is not a specifier set, so the
    /// script is parsed and resolved with the project's target version (#917).
    InlineScriptRequiresPythonUnreadable {
        /// Root-relative script path.
        file: String,
        /// `requires-python` as written.
        value: String,
    },
    /// A lockfile was not used for the transitive check (#972).
    LockfileSkipped {
        /// Root-relative lockfile path.
        file: String,
        /// Why it was skipped and what that leaves unchecked.
        reason: String,
    },
    /// An entry-point declaration could not be read, so it roots nothing
    /// (#916). Findings are not lowered for it (#888).
    EntryPointUnreadable {
        /// Root-relative manifest file.
        file: String,
        /// Key path of the declaration, e.g. `tool.poetry.scripts.acme`.
        label: String,
        /// Why it was skipped.
        reason: String,
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
    /// Join every file onto `base`, the root-relative directory of the
    /// manifest that emitted the warning.
    pub(crate) fn rebase(&mut self, base: &str) {
        match self {
            Self::SetupPyNotStatic { file }
            | Self::RuntimeDependenciesUnknown { file }
            | Self::InvalidRequirementLine { file, .. }
            | Self::SetupPyPartiallyStatic { file, .. }
            | Self::RequirementsOptionIgnored { file, .. }
            | Self::RequirementsIncludeNotRead { file, .. }
            | Self::PathOutsideRoot { file, .. }
            | Self::PathSourceMissing { file, .. }
            | Self::PathSourceNotProject { file, .. }
            | Self::RequirementsUnreadable { file, .. }
            | Self::DependencyGroupIncludeUndefined { file, .. }
            | Self::DependencyGroupIncludeCycle { file, .. }
            | Self::FileUndecodable { file }
            | Self::DynamicExtrasUnread { file, .. }
            | Self::RequiresPythonUnreadable { file, .. }
            | Self::InlineScriptRequiresPythonUnreadable { file, .. }
            | Self::InlineScriptInvalid { file, .. }
            | Self::EntryPointUnreadable { file, .. }
            | Self::LockfileSkipped { file, .. } => *file = join_rel(base, file),
            Self::MetadataConflict {
                kept_source,
                ignored_source,
                ..
            } => {
                *kept_source = join_rel(base, kept_source);
                *ignored_source = join_rel(base, ignored_source);
            },
            // No file to rebase: the member path is printed beside them.
            Self::PoetryDetected
            | Self::PdmDetected
            | Self::HatchDetected
            | Self::RequirementsConstraintMissing { .. } => {},
        }
    }
}
