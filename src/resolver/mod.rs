//! Import name → distribution resolution (pipeline step 7).

mod apply;
mod bundled;
mod first_party;
mod maps;
mod metadata;
mod resolve;
mod stdlib;
mod types;
mod venv;

pub use apply::apply_resolution_to_graph;
pub(crate) use first_party::is_first_party_import;
pub use maps::{ImportMap, build_binary_map};
pub use resolve::{resolve_imports, resolve_imports_with_script_targets};
pub use types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, TransitiveIndex,
    import_root,
};
pub use venv::VenvIndex;
