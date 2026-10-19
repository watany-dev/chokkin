//! Python source parser (pipeline step 6).

mod attributes;
mod condition;
mod decorators;
mod deprecation;
mod dynamic;
mod encoding;
mod error;
mod exports;
mod ignores;
mod ipython;
mod lines;
mod module_guard;
mod parse;
mod platform_guard;
mod relative;
mod server_app;
mod type_checking;
mod types;
mod visit;

pub(crate) use dynamic::module_ref_name;
pub(crate) use encoding::decode_python_source;
pub(crate) use error::ParseError;
pub use ignores::extract_ignores;
pub(crate) use lines::LineIndex;
pub(crate) use module_guard::{guarded_roots, predicate_name_module};
pub use parse::{parse_file, parse_project_sources_with_cache};
pub(crate) use relative::resolve_relative_import;
#[cfg(test)]
pub(crate) use types::ParseDiagnostic;
pub(crate) use types::import_context_for_file;
#[cfg(test)]
pub(crate) use types::{DynamicImport, ImportRef};
pub use types::{Exports, ImportContext, ImportKind, ParseSeverity, ParseSummary, ParsedModule};
pub(crate) use types::{IgnoreDirective, SymbolDef, SymbolKind};
