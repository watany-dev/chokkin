//! Python source parser (pipeline step 6).

mod attributes;
mod condition;
mod decorators;
mod deprecation;
mod depth;
mod dynamic;
mod encoding;
mod error;
mod exports;
mod ignores;
mod import_check;
mod ipython;
mod lines;
mod module_guard;
mod parse;
mod platform_guard;
mod registry;
mod relative;
mod server_app;
mod type_checking;
mod types;
mod version_guard;
mod visit;

pub(crate) use depth::parse_walkable_module;
pub use dynamic::apply_module_loaders;
pub(crate) use dynamic::module_ref_name;
pub(crate) use encoding::decode_python_source;
pub(crate) use error::ParseError;
pub(crate) use ignores::extract_ignores_with;
pub(crate) use import_check::apply_import_checks;
pub(crate) use lines::LineIndex;
pub(crate) use module_guard::{guarded_roots, predicate_name_module};
pub use parse::{parse_file, parse_project_sources_with_cache};
pub(crate) use relative::{NOT_IN_PACKAGE, resolve_relative_import};
#[cfg(test)]
pub(crate) use types::DynamicImport;
pub(crate) use types::ImportRef;
#[cfg(test)]
pub(crate) use types::ParseDiagnostic;
pub(crate) use types::import_context_for_file;
pub use types::{Exports, ImportContext, ImportKind, ParseSeverity, ParseSummary, ParsedModule};
pub(crate) use types::{IgnoreDirective, SymbolDef, SymbolKind};
