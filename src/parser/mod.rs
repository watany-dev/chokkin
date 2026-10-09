//! Python source parser (pipeline step 6).

mod attributes;
mod decorators;
mod dynamic;
mod encoding;
mod error;
mod exports;
mod ignores;
mod lines;
mod module_guard;
mod parse;
mod platform_guard;
mod relative;
mod type_checking;
mod types;
mod visit;

pub(crate) use encoding::decode_python_source;
pub(crate) use error::ParseError;
pub use ignores::extract_ignores;
pub use parse::{parse_file, parse_project_sources_with_cache};
#[cfg(test)]
pub(crate) use relative::resolve_relative_import;
pub(crate) use types::import_context_for_file;
#[cfg(test)]
pub(crate) use types::{DynamicImport, ImportRef};
pub(crate) use types::{IgnoreDirective, SymbolDef, SymbolKind};
pub use types::{ImportContext, ImportKind, ParseSeverity, ParseSummary, ParsedModule};
