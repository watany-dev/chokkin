//! AST visitor for imports, symbols, and dynamic references.

use rustpython_parser::ast::Ranged;
use rustpython_parser::ast::{Alias, ExceptHandler, Expr, Stmt, StmtImport, StmtImportFrom};
use rustpython_parser::source_code::RandomLocator;

use crate::sources::{FileContext, LayoutInfo};

use super::decorators::normalize_decorator;
use super::dynamic::collect_dynamic_imports;
use super::exports::extract_exports;
use super::platform_guard::is_platform_guard_if;
use super::relative::{resolve_relative_import, unresolved_relative_diagnostic};
use super::type_checking::is_type_checking_if;
use super::types::{
    ImportContext, ImportKind, ImportRef, ParsedModule, SymbolDef, SymbolKind,
    import_context_for_file,
};

/// Mutable parse state accumulated while visiting one module.
pub struct ModuleVisitor<'a> {
    path: &'a str,
    layout: &'a LayoutInfo,
    locator: &'a mut RandomLocator<'a>,
    default_context: ImportContext,
    in_type_checking: bool,
    try_depth: u32,
    platform_guard_depth: u32,
    module_level: bool,
    parsed: ParsedModule,
}

impl<'a> ModuleVisitor<'a> {
    /// Create a visitor for `path` with empty output.
    pub fn new(
        path: &'a str,
        layout: &'a LayoutInfo,
        file_context: FileContext,
        locator: &'a mut RandomLocator<'a>,
    ) -> Self {
        let default_context = import_context_for_file(file_context);
        Self {
            path,
            layout,
            locator,
            default_context,
            in_type_checking: false,
            try_depth: 0,
            platform_guard_depth: 0,
            module_level: true,
            parsed: ParsedModule::empty(path.to_owned()),
        }
    }

    /// Consume the visitor and return the accumulated parse result.
    #[must_use]
    pub fn into_parsed(self) -> ParsedModule {
        self.parsed
    }

    /// Visit module-level statements.
    pub fn visit_module(&mut self, stmts: &[Stmt]) {
        self.parsed.exports = extract_exports(stmts, self.locator, &mut self.parsed.diagnostics);
        collect_dynamic_imports(
            stmts,
            self.locator,
            &mut self.parsed.dynamic_imports,
            &mut self.parsed.has_opaque_dynamic_import,
        );
        for stmt in stmts {
            self.visit_stmt(stmt);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Import(import) => self.visit_import(import),
            Stmt::ImportFrom(import_from) => self.visit_import_from(import_from),
            Stmt::FunctionDef(function) => {
                if self.module_level {
                    let line = self.line_number(function);
                    self.record_symbol(
                        function.name.to_string(),
                        SymbolKind::Function,
                        line,
                        &function.decorator_list,
                    );
                }
                let saved = self.module_level;
                self.module_level = false;
                for inner in &function.body {
                    self.visit_stmt(inner);
                }
                self.module_level = saved;
            },
            Stmt::AsyncFunctionDef(function) => {
                if self.module_level {
                    let line = self.line_number(function);
                    self.record_symbol(
                        function.name.to_string(),
                        SymbolKind::Function,
                        line,
                        &function.decorator_list,
                    );
                }
                let saved = self.module_level;
                self.module_level = false;
                for inner in &function.body {
                    self.visit_stmt(inner);
                }
                self.module_level = saved;
            },
            Stmt::ClassDef(class) => {
                if self.module_level {
                    let line = self.line_number(class);
                    self.record_symbol(
                        class.name.to_string(),
                        SymbolKind::Class,
                        line,
                        &class.decorator_list,
                    );
                }
                let saved = self.module_level;
                self.module_level = false;
                for inner in &class.body {
                    self.visit_stmt(inner);
                }
                self.module_level = saved;
            },
            Stmt::Assign(assign) => {
                if self.module_level {
                    let line = self.line_number(assign);
                    for target in &assign.targets {
                        if let Expr::Name(name) = target {
                            self.record_symbol(
                                name.id.to_string(),
                                SymbolKind::Variable,
                                line,
                                &[],
                            );
                        }
                    }
                }
            },
            Stmt::AnnAssign(ann_assign) if self.module_level => {
                if let Expr::Name(name) = &*ann_assign.target {
                    let line = self.line_number(ann_assign);
                    self.record_symbol(name.id.to_string(), SymbolKind::Variable, line, &[]);
                }
            },
            Stmt::If(if_stmt) => {
                let was_type_checking = self.in_type_checking;
                let was_platform_guard = self.platform_guard_depth;
                if is_type_checking_if(stmt) {
                    self.in_type_checking = true;
                }
                if is_platform_guard_if(stmt) {
                    self.platform_guard_depth = self.platform_guard_depth.saturating_add(1);
                }
                for inner in &if_stmt.body {
                    self.visit_stmt(inner);
                }
                self.in_type_checking = was_type_checking;
                self.platform_guard_depth = was_platform_guard;
                for inner in &if_stmt.orelse {
                    self.visit_stmt(inner);
                }
            },
            Stmt::Try(try_stmt) => {
                self.try_depth = self.try_depth.saturating_add(1);
                for inner in &try_stmt.body {
                    self.visit_stmt(inner);
                }
                self.try_depth = self.try_depth.saturating_sub(1);
                for handler in &try_stmt.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    for inner in &handler.body {
                        self.visit_stmt(inner);
                    }
                }
                for inner in &try_stmt.orelse {
                    self.visit_stmt(inner);
                }
                for inner in &try_stmt.finalbody {
                    self.visit_stmt(inner);
                }
            },
            Stmt::With(with_stmt) => {
                for inner in &with_stmt.body {
                    self.visit_stmt(inner);
                }
            },
            Stmt::AsyncWith(with_stmt) => {
                for inner in &with_stmt.body {
                    self.visit_stmt(inner);
                }
            },
            Stmt::Match(match_stmt) => {
                for case in &match_stmt.cases {
                    for inner in &case.body {
                        self.visit_stmt(inner);
                    }
                }
            },
            Stmt::For(for_stmt) => {
                for inner in &for_stmt.body {
                    self.visit_stmt(inner);
                }
                for inner in &for_stmt.orelse {
                    self.visit_stmt(inner);
                }
            },
            Stmt::AsyncFor(for_stmt) => {
                for inner in &for_stmt.body {
                    self.visit_stmt(inner);
                }
                for inner in &for_stmt.orelse {
                    self.visit_stmt(inner);
                }
            },
            Stmt::While(while_stmt) => {
                for inner in &while_stmt.body {
                    self.visit_stmt(inner);
                }
                for inner in &while_stmt.orelse {
                    self.visit_stmt(inner);
                }
            },
            _ => {},
        }
    }

    fn visit_import(&mut self, import: &StmtImport) {
        let line = self.line_number(import);
        let context = self.current_import_context();
        let optional = self.try_depth > 0;
        let platform_guarded = self.platform_guard_depth > 0;
        for alias in &import.names {
            self.push_import(ImportRef {
                module: alias.name.to_string(),
                name: None,
                alias: alias_as_name(alias),
                line,
                kind: ImportKind::Import,
                context,
                optional,
                platform_guarded,
                relative_level: 0,
            });
        }
    }

    fn visit_import_from(&mut self, import_from: &StmtImportFrom) {
        let line = self.line_number(import_from);
        let context = self.current_import_context();
        let optional = self.try_depth > 0;
        let platform_guarded = self.platform_guard_depth > 0;
        let level = import_from
            .level
            .as_ref()
            .map_or(0, rustpython_parser::ast::Int::to_u32);
        let level = u8::try_from(level).unwrap_or(u8::MAX);
        let module_suffix = import_from.module.as_ref().map(ToString::to_string);

        for alias in &import_from.names {
            if alias.name.as_str() == "*" {
                continue;
            }

            let (module, name) = if level == 0 {
                (
                    module_suffix.clone().unwrap_or_default(),
                    Some(alias.name.to_string()),
                )
            } else {
                let imported_name = if module_suffix.is_none() {
                    Some(alias.name.as_str())
                } else {
                    None
                };
                let resolved = resolve_relative_import(
                    self.path,
                    self.layout,
                    level,
                    module_suffix.as_deref(),
                    imported_name,
                );
                let module = resolved.unwrap_or_else(|| {
                    self.parsed
                        .diagnostics
                        .push(unresolved_relative_diagnostic(self.path, line));
                    String::new()
                });
                let name = if module_suffix.is_some() {
                    Some(alias.name.to_string())
                } else {
                    None
                };
                (module, name)
            };

            self.push_import(ImportRef {
                module,
                name,
                alias: alias_as_name(alias),
                line,
                kind: ImportKind::ImportFrom,
                context,
                optional,
                platform_guarded,
                relative_level: level,
            });
        }
    }

    fn push_import(&mut self, import: ImportRef) {
        self.parsed.imports.push(import);
    }

    fn current_import_context(&self) -> ImportContext {
        if self.in_type_checking {
            ImportContext::Type
        } else {
            self.default_context
        }
    }

    fn record_symbol(&mut self, name: String, kind: SymbolKind, line: u32, decorators: &[Expr]) {
        let is_public =
            !name.starts_with('_') || self.parsed.exports.iter().any(|export| export == &name);
        let normalized = decorators.iter().filter_map(normalize_decorator).collect();
        self.parsed.symbols.push(SymbolDef {
            name,
            kind,
            line,
            is_public,
            decorators: normalized,
            in_type_checking: self.in_type_checking,
        });
    }

    fn line_number<R: Ranged>(&mut self, node: &R) -> u32 {
        self.locator.locate(node.start()).row.get()
    }
}

fn alias_as_name(alias: &Alias) -> Option<String> {
    alias.asname.as_ref().map(ToString::to_string)
}
