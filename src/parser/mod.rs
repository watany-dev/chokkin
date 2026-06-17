//! Python source parser (pipeline step 6).

mod attributes;
mod decorators;
mod dynamic;
mod error;
mod exports;
mod ignores;
mod parse;
mod platform_guard;
mod relative;
mod syntax;
mod type_checking;
mod types;
mod visit;

pub use error::ParseError;
pub use parse::{parse_file, parse_project_sources, parse_project_sources_with_cache};
pub use relative::{file_module_name, resolve_relative_import};
pub use types::{
    AttributeAccess, DynamicImport, IgnoreDirective, ImportContext, ImportKind, ImportRef,
    ParseDiagnostic, ParseSeverity, ParseSummary, ParsedModule, SymbolDef, SymbolKind,
};
