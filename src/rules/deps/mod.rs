//! Dependency reconciliation (pipeline step 10).

mod binary;
mod context;
mod duplicate;
mod misplaced;
mod missing;
mod reconcile;
mod script;
mod unused;
mod used;

pub(crate) use context::{DeclarationBucket, declaration_buckets};
pub(crate) use duplicate::duplicate_declarations;

pub use reconcile::reconcile_with_context;
