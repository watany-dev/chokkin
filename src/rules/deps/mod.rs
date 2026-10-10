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

pub(crate) use context::{DeclarationBucket, declaration_buckets, file_context};
pub(crate) use duplicate::removable_duplicates;
pub(crate) use used::local_source_member;

pub use reconcile::reconcile_with_context;
