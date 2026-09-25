//! Dependency reconciliation (pipeline step 10).

mod binary;
mod context;
mod duplicate;
mod misplaced;
mod missing;
mod reconcile;
mod unused;
mod used;

pub use reconcile::reconcile_dependencies;

pub(crate) use reconcile::reconcile_with_context;
