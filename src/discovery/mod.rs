//! Project root discovery (pipeline step 1).

mod error;
mod root;

pub use error::DiscoveryError;
pub use root::{ProjectRoot, RootMarker, discover_project_root};
