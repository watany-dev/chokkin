//! AST visitor for imports, symbols, and dynamic references.

use std::collections::HashSet;

use rustpython_parser::ast::Ranged;
use rustpython_parser::ast::{
    Alias, Arguments, Comprehension, ExceptHandler, Expr, Stmt, StmtImport, StmtImportFrom,
    StmtTry, StmtTryStar,
};
use rustpython_parser::source_code::RandomLocator;

use crate::sources::{FileContext, LayoutInfo};

use super::attributes::attribute_receiver;
use super::decorators::normalize_decorator;
use super::dynamic::{LoaderNames, literal_module};
use super::exports::extract_exports;
use super::platform_guard::is_platform_guard_if;
use super::relative::{resolve_relative_import, unresolved_relative_diagnostic};
use super::type_checking::is_type_checking_if;
use super::types::{
    AttributeAccess, DecoratorSite, DynamicImport, ImportContext, ImportKind, ImportRef,
    ParsedModule, SymbolDef, SymbolKind, import_context_for_file,
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
    typing_aliases: HashSet<String>,
    type_checking_names: HashSet<String>,
    loader_names: LoaderNames,
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
            typing_aliases: HashSet::from(["typing".to_owned()]),
            type_checking_names: HashSet::from(["TYPE_CHECKING".to_owned()]),
            loader_names: LoaderNames::default(),
            parsed: ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            },
        }
    }

    /// Consume the visitor and return the accumulated parse result.
    #[must_use]
    pub fn into_parsed(self) -> ParsedModule {
        self.parsed
    }

    /// Visit module-level statements.
    ///
    /// `__all__` is read first because [`Self::record_symbol`] consults the export
    /// list to decide whether an underscore-prefixed symbol is public.
    pub fn visit_module(&mut self, stmts: &[Stmt]) {
        self.parsed.exports = extract_exports(stmts, self.locator, &mut self.parsed.diagnostics);
        for stmt in stmts {
            self.visit_stmt(stmt);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Import(import) => self.visit_import(import),
            Stmt::ImportFrom(import_from) => self.visit_import_from(import_from),
            Stmt::FunctionDef(_) | Stmt::AsyncFunctionDef(_) | Stmt::ClassDef(_) => {
                self.visit_def(stmt);
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
                for target in &assign.targets {
                    self.visit_expr(target);
                }
                self.visit_expr(&assign.value);
            },
            Stmt::AnnAssign(ann_assign) => {
                if self.module_level
                    && let Expr::Name(name) = &*ann_assign.target
                {
                    let line = self.line_number(ann_assign);
                    self.record_symbol(name.id.to_string(), SymbolKind::Variable, line, &[]);
                }
                self.visit_expr(&ann_assign.target);
                self.visit_expr(&ann_assign.annotation);
                if let Some(value) = &ann_assign.value {
                    self.visit_expr(value);
                }
            },
            Stmt::AugAssign(aug_assign) => self.visit_expr(&aug_assign.value),
            Stmt::Return(return_stmt) => {
                if let Some(value) = &return_stmt.value {
                    self.visit_expr(value);
                }
            },
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::If(if_stmt) => {
                self.visit_expr(&if_stmt.test);
                let was_type_checking = self.in_type_checking;
                let was_platform_guard = self.platform_guard_depth;
                if is_type_checking_if(stmt, &self.typing_aliases, &self.type_checking_names) {
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
            Stmt::Try(StmtTry {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            })
            | Stmt::TryStar(StmtTryStar {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            }) => {
                self.try_depth = self.try_depth.saturating_add(1);
                for inner in body {
                    self.visit_stmt(inner);
                }
                self.try_depth = self.try_depth.saturating_sub(1);
                for handler in handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(exc_type) = &handler.type_ {
                        self.visit_expr(exc_type);
                    }
                    for inner in &handler.body {
                        self.visit_stmt(inner);
                    }
                }
                for inner in orelse {
                    self.visit_stmt(inner);
                }
                for inner in finalbody {
                    self.visit_stmt(inner);
                }
            },
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                }
                self.visit_body(&with_stmt.body);
            },
            Stmt::AsyncWith(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                }
                self.visit_body(&with_stmt.body);
            },
            Stmt::Match(match_stmt) => {
                self.visit_expr(&match_stmt.subject);
                for case in &match_stmt.cases {
                    if let Some(guard) = &case.guard {
                        self.visit_expr(guard);
                    }
                    for inner in &case.body {
                        self.visit_stmt(inner);
                    }
                }
            },
            Stmt::For(for_stmt) => {
                self.visit_expr(&for_stmt.iter);
                self.visit_body(&for_stmt.body);
                self.visit_body(&for_stmt.orelse);
            },
            Stmt::AsyncFor(for_stmt) => {
                self.visit_expr(&for_stmt.iter);
                self.visit_body(&for_stmt.body);
                self.visit_body(&for_stmt.orelse);
            },
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test);
                self.visit_body(&while_stmt.body);
                self.visit_body(&while_stmt.orelse);
            },
            Stmt::Raise(raise) => {
                if let Some(exc) = &raise.exc {
                    self.visit_expr(exc);
                }
                if let Some(cause) = &raise.cause {
                    self.visit_expr(cause);
                }
            },
            Stmt::Assert(assert) => {
                self.visit_expr(&assert.test);
                if let Some(msg) = &assert.msg {
                    self.visit_expr(msg);
                }
            },
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.visit_expr(target);
                }
            },
            _ => {},
        }
    }

    fn visit_def(&mut self, stmt: &Stmt) {
        let (name, decorators, body, kind) = match stmt {
            Stmt::FunctionDef(def) => (
                &def.name,
                &def.decorator_list,
                &def.body,
                SymbolKind::Function,
            ),
            Stmt::AsyncFunctionDef(def) => (
                &def.name,
                &def.decorator_list,
                &def.body,
                SymbolKind::Function,
            ),
            Stmt::ClassDef(def) => (&def.name, &def.decorator_list, &def.body, SymbolKind::Class),
            _ => return,
        };
        self.record_decorators(decorators);
        for decorator in decorators {
            self.visit_expr(decorator);
        }
        match stmt {
            Stmt::FunctionDef(def) => {
                self.visit_arguments(&def.args);
                if let Some(returns) = &def.returns {
                    self.visit_expr(returns);
                }
            },
            Stmt::AsyncFunctionDef(def) => {
                self.visit_arguments(&def.args);
                if let Some(returns) = &def.returns {
                    self.visit_expr(returns);
                }
            },
            Stmt::ClassDef(def) => {
                for base in &def.bases {
                    self.visit_expr(base);
                }
                for keyword in &def.keywords {
                    self.visit_expr(&keyword.value);
                }
            },
            _ => {},
        }
        if self.module_level {
            let line = self.line_number(stmt);
            self.record_symbol(name.to_string(), kind, line, decorators);
        }
        let saved = self.module_level;
        self.module_level = false;
        self.visit_body(body);
        self.module_level = saved;
    }

    fn visit_arguments(&mut self, arguments: &Arguments) {
        for arg in arguments
            .posonlyargs
            .iter()
            .chain(&arguments.args)
            .chain(&arguments.kwonlyargs)
        {
            if let Some(annotation) = &arg.def.annotation {
                self.visit_expr(annotation);
            }
            if let Some(default) = &arg.default {
                self.visit_expr(default);
            }
        }
        for arg in arguments.vararg.iter().chain(&arguments.kwarg) {
            if let Some(annotation) = &arg.annotation {
                self.visit_expr(annotation);
            }
        }
    }

    fn visit_body(&mut self, body: &[Stmt]) {
        for inner in body {
            self.visit_stmt(inner);
        }
    }

    /// Walk one expression for dynamic imports and module attribute accesses.
    ///
    /// Both used to be separate full-tree passes; folding them into the statement
    /// walk keeps cold parse to a single traversal per file.
    #[allow(clippy::too_many_lines)]
    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Attribute(attribute) => {
                if let Some(receiver) = attribute_receiver(&attribute.value) {
                    let line = self.line_number(attribute);
                    self.parsed.attribute_accesses.push(AttributeAccess {
                        receiver,
                        name: attribute.attr.to_string(),
                        line,
                    });
                }
                self.visit_expr(&attribute.value);
            },
            Expr::Call(call) => {
                if self.loader_names.is_loader(&call.func) {
                    if let Some(module) = literal_module(call) {
                        let line = self.line_number(call);
                        self.parsed
                            .dynamic_imports
                            .push(DynamicImport { module, line });
                    } else if !call.args.is_empty() || !call.keywords.is_empty() {
                        self.parsed.has_opaque_dynamic_import = true;
                    }
                }
                // `map(importlib.import_module, names)` loads modules this walk never sees.
                if call
                    .args
                    .iter()
                    .chain(call.keywords.iter().map(|keyword| &keyword.value))
                    .any(|arg| self.loader_names.is_loader(arg))
                {
                    self.parsed.has_opaque_dynamic_import = true;
                }
                self.visit_expr(&call.func);
                for arg in &call.args {
                    self.visit_expr(arg);
                }
                for keyword in &call.keywords {
                    self.visit_expr(&keyword.value);
                }
            },
            Expr::BoolOp(bool_op) => {
                for value in &bool_op.values {
                    self.visit_expr(value);
                }
            },
            Expr::NamedExpr(named) => self.visit_expr(&named.value),
            Expr::BinOp(bin_op) => {
                self.visit_expr(&bin_op.left);
                self.visit_expr(&bin_op.right);
            },
            Expr::UnaryOp(unary) => self.visit_expr(&unary.operand),
            Expr::Lambda(lambda) => {
                self.visit_arguments(&lambda.args);
                self.visit_expr(&lambda.body);
            },
            Expr::IfExp(if_exp) => {
                self.visit_expr(&if_exp.test);
                self.visit_expr(&if_exp.body);
                self.visit_expr(&if_exp.orelse);
            },
            Expr::Dict(dict) => {
                for (key, value) in dict.keys.iter().zip(&dict.values) {
                    if let Some(key) = key {
                        self.visit_expr(key);
                    }
                    self.visit_expr(value);
                }
            },
            Expr::Set(set) => {
                for value in &set.elts {
                    self.visit_expr(value);
                }
            },
            Expr::ListComp(list_comp) => {
                self.visit_expr(&list_comp.elt);
                self.visit_comprehensions(&list_comp.generators);
            },
            Expr::SetComp(set_comp) => {
                self.visit_expr(&set_comp.elt);
                self.visit_comprehensions(&set_comp.generators);
            },
            Expr::DictComp(dict_comp) => {
                self.visit_expr(&dict_comp.key);
                self.visit_expr(&dict_comp.value);
                self.visit_comprehensions(&dict_comp.generators);
            },
            Expr::GeneratorExp(generator) => {
                self.visit_expr(&generator.elt);
                self.visit_comprehensions(&generator.generators);
            },
            Expr::Await(await_expr) => self.visit_expr(&await_expr.value),
            Expr::Yield(yield_expr) => {
                if let Some(value) = &yield_expr.value {
                    self.visit_expr(value);
                }
            },
            Expr::YieldFrom(yield_from) => self.visit_expr(&yield_from.value),
            Expr::Compare(compare) => {
                self.visit_expr(&compare.left);
                for comparator in &compare.comparators {
                    self.visit_expr(comparator);
                }
            },
            Expr::Subscript(subscript) => {
                self.visit_expr(&subscript.value);
                self.visit_expr(&subscript.slice);
            },
            Expr::Starred(starred) => self.visit_expr(&starred.value),
            Expr::List(list) => {
                for value in &list.elts {
                    self.visit_expr(value);
                }
            },
            Expr::Tuple(tuple) => {
                for value in &tuple.elts {
                    self.visit_expr(value);
                }
            },
            Expr::FormattedValue(formatted) => self.visit_expr(&formatted.value),
            Expr::JoinedStr(joined) => {
                for value in &joined.values {
                    self.visit_expr(value);
                }
            },
            Expr::Slice(slice) => {
                if let Some(lower) = &slice.lower {
                    self.visit_expr(lower);
                }
                if let Some(upper) = &slice.upper {
                    self.visit_expr(upper);
                }
                if let Some(step) = &slice.step {
                    self.visit_expr(step);
                }
            },
            Expr::Constant(_) | Expr::Name(_) => {},
        }
    }

    fn visit_comprehensions(&mut self, comprehensions: &[Comprehension]) {
        for comprehension in comprehensions {
            self.visit_expr(&comprehension.target);
            self.visit_expr(&comprehension.iter);
            for if_clause in &comprehension.ifs {
                self.visit_expr(if_clause);
            }
        }
    }

    fn visit_import(&mut self, import: &StmtImport) {
        let line = self.line_number(import);
        let context = self.current_import_context();
        let optional = self.try_depth > 0;
        let platform_guarded = self.platform_guard_depth > 0;
        for alias in &import.names {
            self.loader_names.record_import(alias);
            if alias.name.as_str() == "typing" {
                self.typing_aliases.insert(
                    alias
                        .asname
                        .as_ref()
                        .map_or_else(|| "typing".to_owned(), ToString::to_string),
                );
            }
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

            if level == 0 {
                self.loader_names
                    .record_import_from(module_suffix.as_deref(), alias);
            }
            if level == 0
                && module_suffix.as_deref() == Some("typing")
                && alias.name.as_str() == "TYPE_CHECKING"
            {
                self.type_checking_names.insert(
                    alias
                        .asname
                        .as_ref()
                        .map_or_else(|| "TYPE_CHECKING".to_owned(), ToString::to_string),
                );
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

    /// Record every recognized decorator, nested definitions included.
    ///
    /// Plugins (Flask routes, Celery tasks) read these instead of re-opening
    /// each source file, so app-factory patterns that decorate inside a
    /// function must land here too.
    fn record_decorators(&mut self, decorators: &[Expr]) {
        for decorator in decorators {
            let Some(name) = normalize_decorator(decorator) else {
                continue;
            };
            let line = self.line_number(decorator);
            self.parsed
                .decorator_sites
                .push(DecoratorSite { name, line });
        }
    }

    fn line_number<R: Ranged>(&mut self, node: &R) -> u32 {
        self.locator.locate(node.start()).row.get()
    }
}

fn alias_as_name(alias: &Alias) -> Option<String> {
    alias.asname.as_ref().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast::Suite;

    use super::*;
    use crate::sources::ProjectLayout;

    fn visit_source(source: &str) -> ParsedModule {
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
        };
        let mut locator = RandomLocator::new(source);
        let mut visitor = ModuleVisitor::new("mod.py", &layout, FileContext::Runtime, &mut locator);
        visitor.visit_module(&stmts);
        visitor.into_parsed()
    }

    #[test]
    fn collects_module_attribute_accesses() {
        let parsed =
            visit_source("import acme.utils\nacme.utils.helper()\nvalue = acme.utils.CONFIG\n");
        assert!(parsed.attribute_accesses.iter().any(|access| {
            access.receiver == "acme.utils" && access.name == "helper" && access.line == 2
        }));
        assert!(parsed.attribute_accesses.iter().any(|access| {
            access.receiver == "acme.utils" && access.name == "CONFIG" && access.line == 3
        }));
    }

    #[test]
    fn extracts_importlib_literal() {
        let parsed = visit_source("import importlib\nimportlib.import_module(\"acme.plugins\")\n");
        assert_eq!(parsed.dynamic_imports.len(), 1);
        assert_eq!(parsed.dynamic_imports[0].module, "acme.plugins");
        assert_eq!(parsed.dynamic_imports[0].line, 2);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn extracts_importlib_from_assignment() {
        let parsed =
            visit_source("import importlib\nmod = importlib.import_module(\"acme.plugins\")\n");
        assert_eq!(parsed.dynamic_imports.len(), 1);
        assert_eq!(parsed.dynamic_imports[0].line, 2);
    }

    #[test]
    fn extracts_importlib_from_return() {
        let parsed = visit_source(
            "import importlib\ndef load():\n    return importlib.import_module(\"acme.plugins\")\n",
        );
        assert_eq!(parsed.dynamic_imports.len(), 1);
        assert_eq!(parsed.dynamic_imports[0].line, 3);
    }

    #[test]
    fn extracts_importlib_from_call_argument() {
        let parsed = visit_source(
            "import importlib\ndef run(fn):\n    pass\nrun(importlib.import_module(\"acme.plugins\"))\n",
        );
        assert_eq!(parsed.dynamic_imports.len(), 1);
        assert_eq!(parsed.dynamic_imports[0].line, 4);
    }

    #[test]
    fn marks_opaque_assignment_with_non_literal() {
        let parsed = visit_source("import importlib\nmod = importlib.import_module(name)\n");
        assert!(parsed.dynamic_imports.is_empty());
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn extracts_aliased_import_module_literal() {
        let parsed = visit_source(
            "from importlib import import_module as im\nim(\"acme.a\")\nimport importlib as il\nil.import_module(\"acme.b\")\n",
        );
        let modules: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|dynamic| (dynamic.module.as_str(), dynamic.line))
            .collect();
        assert_eq!(modules, vec![("acme.a", 2), ("acme.b", 4)]);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_aliased_import_module() {
        let parsed = visit_source("from importlib import import_module\nimport_module(name)\n");
        assert!(parsed.dynamic_imports.is_empty());
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn extracts_import_module_name_keyword() {
        let parsed = visit_source(
            "import importlib\nimportlib.import_module(name=\"acme.a\")\n__import__(name=\"acme.b\")\n",
        );
        let modules: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|dynamic| dynamic.module.as_str())
            .collect();
        assert_eq!(modules, vec!["acme.a", "acme.b"]);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_non_literal_keyword() {
        let parsed = visit_source("import importlib\nimportlib.import_module(name=target)\n");
        assert!(parsed.dynamic_imports.is_empty());
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_loader_passed_as_argument() {
        let parsed =
            visit_source("import importlib\nmods = list(map(importlib.import_module, names))\n");
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn ignores_unrelated_import_module_function() {
        let parsed = visit_source(
            "from acme.loader import import_module\nimport_module(name)\nimport_module(\"acme.a\")\n",
        );
        assert!(parsed.dynamic_imports.is_empty());
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn single_walk_reaches_nested_expression_positions() {
        // These positions were unreachable while attribute collection had its own
        // statement/expression arm set; the merged walk uses the wider one.
        let parsed = visit_source(
            "import acme.utils\n\n\nasync def run(total):\n    total += acme.utils.STEP\n    await acme.utils.flush()\n    return [acme.utils.name(x) for x in acme.utils.items]\n",
        );
        for (name, line) in [("STEP", 5), ("flush", 6), ("name", 7), ("items", 7)] {
            assert!(
                parsed.attribute_accesses.iter().any(|access| {
                    access.receiver == "acme.utils" && access.name == name && access.line == line
                }),
                "missing acme.utils.{name} at line {line}"
            );
        }
    }

    fn attribute_lines(parsed: &ParsedModule, name: &str) -> Vec<u32> {
        parsed
            .attribute_accesses
            .iter()
            .filter(|access| access.receiver == "utils" && access.name == name)
            .map(|access| access.line)
            .collect()
    }

    #[test]
    fn marks_opaque_import_inside_for_iter() {
        let parsed = visit_source(
            "import importlib\nfor p in importlib.import_module(name).plugins():\n    pass\n",
        );
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn collects_attributes_from_statement_expression_slots() {
        let source = "\
from acme import utils
if utils.A:
    pass
while utils.B:
    pass
for x in utils.C:
    pass
with utils.D() as d:
    pass
match utils.E:
    case 1 if utils.F:
        pass
try:
    pass
except utils.G:
    pass
raise utils.H from utils.I
assert utils.J, utils.K
del utils.L
value: utils.M = 1
";
        let parsed = visit_source(source);
        for (name, line) in [
            ("A", 2),
            ("B", 4),
            ("C", 6),
            ("D", 8),
            ("E", 10),
            ("F", 11),
            ("G", 15),
            ("H", 17),
            ("I", 17),
            ("J", 18),
            ("K", 18),
            ("L", 19),
            ("M", 20),
        ] {
            assert_eq!(
                attribute_lines(&parsed, name),
                vec![line],
                "utils.{name} should be recorded exactly once"
            );
        }
    }

    #[test]
    fn collects_attributes_from_definition_slots() {
        let source = "\
from acme import utils
@utils.deco
def f(a: utils.A = utils.B, *args: utils.C, k=utils.D, **kw: utils.E) -> utils.F:
    pass
class C(utils.Base, metaclass=utils.Meta):
    pass
g = lambda x=utils.G: x
";
        let parsed = visit_source(source);
        for (name, line) in [
            ("deco", 2),
            ("A", 3),
            ("B", 3),
            ("C", 3),
            ("D", 3),
            ("E", 3),
            ("F", 3),
            ("Base", 5),
            ("Meta", 5),
            ("G", 7),
        ] {
            assert_eq!(
                attribute_lines(&parsed, name),
                vec![line],
                "utils.{name} should be recorded exactly once"
            );
        }
    }

    #[test]
    fn walks_try_star_body_handlers_else_and_finally() {
        let parsed = visit_source(
            "try:\n    import requests\n\n    @app.route(\"/\")\n    def f():\n        pass\nexcept* Exception:\n    import fallback_lib\nelse:\n    import else_lib\nfinally:\n    import finally_lib\n",
        );
        let requests = parsed
            .imports
            .iter()
            .find(|import| import.module == "requests")
            .expect("requests import inside try body");
        assert!(requests.optional);
        for module in ["fallback_lib", "else_lib", "finally_lib"] {
            assert!(
                parsed.imports.iter().any(|import| import.module == module),
                "missing import {module}"
            );
        }
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == "f"));
        assert!(
            parsed
                .decorator_sites
                .iter()
                .any(|site| site.name == "app.route" && site.line == 4)
        );
    }

    #[test]
    fn collects_attributes_from_try_star_handler_type() {
        let parsed =
            visit_source("from acme import utils\ntry:\n    pass\nexcept* utils.G:\n    pass\n");
        assert_eq!(attribute_lines(&parsed, "G"), vec![4]);
    }
}
