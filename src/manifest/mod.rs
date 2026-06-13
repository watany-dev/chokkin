//! Manifest extraction (pipeline step 3).

mod error;
mod extract;
pub(crate) mod literals;
mod pep508_util;
mod pyproject;
mod requirements;
mod setup_cfg;
mod setup_py;
mod types;
pub(crate) mod util;
mod uv_lock;
mod warnings;

pub use error::ManifestError;
pub use extract::{extract_manifest, resolve_target_version};
pub use types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, LoadedManifest,
    LockfileGraph, ManifestSources, ProjectMetadata,
};
pub use warnings::ManifestWarning;
