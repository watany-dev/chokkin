//! Manifest extraction (pipeline step 3).

mod dependency_groups;
mod error;
mod extract;
pub(crate) mod literals;
mod lockfile;
mod pdm_lock;
mod pep508_util;
mod poetry_lock;
mod pylock;
mod pyproject;
mod requirements;
mod script;
mod setup_cfg;
mod setup_py;
mod types;
pub(crate) mod util;
mod uv_lock;
mod warnings;

pub use error::ManifestError;
pub use extract::{extract_manifest, extract_manifest_with_cache, resolve_target_version};
pub(crate) use lockfile::lockfile_candidates;
pub use pep508_util::normalize_distribution_name;
pub use script::{
    InlineScript, discover_inline_scripts, inline_script_target, parse_inline_script,
};
pub use types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, LoadedManifest,
    LockfileGraph, LockfileKind, LockfileSource, ManifestSources, ProjectMetadata,
};
pub use warnings::ManifestWarning;
