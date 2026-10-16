//! Manifest extraction (pipeline step 3).

mod dependency_groups;
mod error;
mod extract;
pub(crate) mod literals;
mod lockfile;
mod pdm_lock;
mod pep508;
mod pep508_util;
mod poetry_lock;
mod pylock;
mod pyproject;
mod requirements;
mod script;
mod setup_cfg;
mod setup_py;
mod setup_py_eval;
mod setuptools_reqs;
mod toml_lines;
mod types;
pub(crate) mod util;
mod uv_lock;
mod uv_tool;
mod warnings;
mod wheel;

pub use error::ManifestError;
pub use extract::{extract_manifest, extract_manifest_with_cache, resolve_target_version};
pub(crate) use extract::{infer_target_version_from_requires_python, requires_python_max_minor};
pub(crate) use lockfile::lockfile_candidates;
pub(crate) use pep508::{canonical_version_specifiers, is_valid_marker};
pub(crate) use pep508_util::pep508_distribution_name;
pub(crate) use pep508_util::{
    invalid_requirement_name, is_project_name, normalize_distribution_name,
};
pub(crate) use requirements::{
    extra_requirements_candidates, join_continued_lines, requirements_line_distribution,
    requirements_line_hashed,
};
pub use script::discover_inline_scripts;
pub(crate) use script::{InlineScript, inline_script_target};
pub(crate) use setup_cfg::{IniLine, ini_lines};
pub(crate) use setuptools_reqs::setuptools_requirement_items;
pub(crate) use types::{
    DeclaredDependency, LockfileGraph, ManifestSources, PackageFind, ProjectMetadata, UvSourceKind,
    UvToolSettings, WheelTargets,
};
pub use types::{DependencyContext, DependencyOrigin, ExtrasState, LoadedManifest, LockfileKind};
#[cfg(test)]
pub(crate) use types::{EntryPointDecl, LockfileSource, UvSource};
pub(crate) use uv_tool::source_entries as uv_source_entries;
pub use warnings::ManifestWarning;
