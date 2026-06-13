//! Import name → distribution resolution (pipeline step 7).

mod apply;
mod bundled;
mod error;
mod first_party;
mod maps;
mod metadata;
mod resolve;
mod stdlib;
mod transitive;
mod types;
mod venv;

pub use apply::apply_resolution_to_graph;
pub use error::ResolveError;
pub use maps::{DistributionCandidate, ImportMap, MapSource, build_binary_map};
pub use resolve::resolve_imports;
pub use types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, TransitiveIndex,
    import_root,
};
