//! Import name → distribution resolution (pipeline step 7).

mod apply;
mod bundled;
mod first_party;
mod maps;
mod metadata;
mod pytest_path;
mod resolve;
mod stdlib;
mod types;
mod venv;

pub use apply::apply_resolution_to_graph;
pub(crate) use first_party::is_first_party_import;
pub use maps::{ImportMap, build_binary_map};
pub(crate) use pytest_path::PytestImportPaths;
pub use resolve::{ScopedDeclarations, resolve_imports, resolve_imports_for_analysis};
pub use stdlib::StdlibRange;
pub use types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, TransitiveIndex,
    import_root,
};
pub use venv::VenvIndex;
