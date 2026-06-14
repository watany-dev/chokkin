//! Python source parser (pipeline step 6).

mod decorators;
mod dynamic;
mod error;
mod exports;
mod ignores;
mod parse;
mod relative;
mod syntax;
mod type_checking;
mod types;
mod visit;

pub use error::ParseError;
pub use parse::{parse_file, parse_project_sources};
pub use relative::{file_module_name, resolve_relative_import};
pub use types::{
    DynamicImport, IgnoreDirective, ImportContext, ImportKind, ImportRef, ParseDiagnostic,
    ParseSeverity, ParseSummary, ParsedModule, SymbolDef, SymbolKind,
};
