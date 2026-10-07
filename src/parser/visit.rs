//! AST visitor for imports, symbols, and dynamic references.

use std::collections::{BTreeSet, HashSet};

use ruff_python_ast::visitor::{Visitor, walk_annotation, walk_expr};
use ruff_python_ast::{
    Alias, Decorator, ExceptHandler, Expr, ExprCall, Identifier, Operator, Stmt, StmtImport,
    StmtImportFrom,
};
use ruff_text_size::Ranged;

use crate::sources::{FileContext, LayoutInfo};

use super::attributes::attribute_receiver;
use super::decorators::normalize_decorator;
use super::dynamic::{
    LiteralTarget, LoaderNames, PythonRun, command_word, literal_target, module_prefix,
    pytest_plugin_names, python_run,
};
use super::exports::extract_exports;
use super::lines::LineIndex;
use super::platform_guard::is_platform_guard_test;
use super::relative::{module_package, resolve_relative_import, unresolved_relative_diagnostic};
use super::type_checking::is_type_checking_test;
use super::types::{
    AttributeAccess, DecoratorSite, DynamicImport, ImportContext, ImportKind, ImportRef,
    ParsedModule, SymbolDef, SymbolKind, import_context_for_file,
};

/// Mutable parse state accumulated while visiting one module.
pub(super) struct ModuleVisitor<'a> {
    path: &'a str,
    layout: &'a LayoutInfo,
    lines: &'a LineIndex,
    default_context: ImportContext,
    in_type_checking: bool,
    try_depth: u32,
    platform_guard_depth: u32,
    function_depth: u32,
    module_level: bool,
    /// Module-level names set to `True` inside a `try` body (`has_x = True`).
    try_flags: HashSet<String>,
    typing_aliases: HashSet<String>,
    type_checking_names: HashSet<String>,
    loader_names: LoaderNames,
    command_words: BTreeSet<String>,
    loaded_names: HashSet<String>,
    parsed: ParsedModule,
}

impl<'a> ModuleVisitor<'a> {
    /// Create a visitor for `path` with empty output.
    pub(super) fn new(
        path: &'a str,
        layout: &'a LayoutInfo,
        file_context: FileContext,
        lines: &'a LineIndex,
    ) -> Self {
        let default_context = import_context_for_file(file_context);
        Self {
            path,
            layout,
            lines,
            default_context,
            in_type_checking: false,
            try_depth: 0,
            platform_guard_depth: 0,
            function_depth: 0,
            module_level: true,
            try_flags: HashSet::new(),
            typing_aliases: HashSet::from(["typing".to_owned()]),
            type_checking_names: HashSet::from(["TYPE_CHECKING".to_owned()]),
            loader_names: LoaderNames::default(),
            command_words: BTreeSet::new(),
            loaded_names: HashSet::new(),
            parsed: ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            },
        }
    }

    /// Consume the visitor and return the accumulated parse result.
    #[must_use]
    pub(super) fn into_parsed(mut self) -> ParsedModule {
        let runs_commands = self
            .parsed
            .imports
            .iter()
            .any(|import| import.module == "subprocess");
        if runs_commands {
            self.parsed.shell_commands = self.command_words.into_iter().collect();
        }
        let used: BTreeSet<String> = self
            .parsed
            .imports
            .iter()
            .filter_map(import_binding)
            .filter(|binding| self.loaded_names.contains(*binding))
            .map(str::to_owned)
            .collect();
        self.parsed.used_import_bindings = used.into_iter().collect();
        for symbol in &mut self.parsed.symbols {
            symbol.used_in_module = self.loaded_names.contains(&symbol.name);
        }
        self.parsed
    }

    /// Visit module-level statements.
    ///
    /// `__all__` is read first because [`Self::record_symbol`] consults the export
    /// list to decide whether an underscore-prefixed symbol is public.
    pub(super) fn visit_module(&mut self, stmts: &[Stmt]) {
        self.parsed.exports = extract_exports(stmts, self.lines, &mut self.parsed.diagnostics);
        self.visit_body(stmts);
    }

    /// Visit one `if`/`elif`/`else` branch; `test` is `None` for `else`.
    fn visit_branch<'ast>(&mut self, test: Option<&'ast Expr>, body: &'ast [Stmt]) {
        let was_type_checking = self.in_type_checking;
        let was_platform_guard = self.platform_guard_depth;
        let was_try_depth = self.try_depth;
        if let Some(test) = test {
            self.visit_expr(test);
            if is_type_checking_test(test, &self.typing_aliases, &self.type_checking_names) {
                self.in_type_checking = true;
            }
            if is_platform_guard_test(test) {
                self.platform_guard_depth = self.platform_guard_depth.saturating_add(1);
            }
            // `if has_x:` only runs when the guarded `try` import succeeded.
            if self.module_level
                && let Expr::Name(name) = test
                && self.try_flags.contains(name.id.as_str())
            {
                self.try_depth = self.try_depth.saturating_add(1);
            }
        }
        self.visit_body(body);
        self.in_type_checking = was_type_checking;
        self.platform_guard_depth = was_platform_guard;
        self.try_depth = was_try_depth;
    }

    fn visit_decorators(&mut self, decorators: &[Decorator]) {
        self.record_decorators(decorators);
        for decorator in decorators {
            self.visit_expr(&decorator.expression);
        }
    }

    /// Record a def/class symbol and walk its body; the caller has already
    /// walked the decorators and signature.
    fn visit_def_body<'ast>(
        &mut self,
        name: &Identifier,
        decorators: &'ast [Decorator],
        body: &'ast [Stmt],
        kind: SymbolKind,
    ) {
        if self.module_level {
            // The def's own range starts at its first decorator; CPython reports
            // the `def`/`class` line, which is where the name sits.
            let line = self.line_number(name);
            self.record_symbol(name.to_string(), kind, line, decorators);
        }
        let saved = self.module_level;
        let saved_function_depth = self.function_depth;
        self.module_level = false;
        if matches!(kind, SymbolKind::Function) {
            self.function_depth += 1;
        }
        self.visit_body(body);
        self.module_level = saved;
        self.function_depth = saved_function_depth;
    }

    /// `[sys.executable, "-m", "pkg"]` uses `pkg` without importing it. It is
    /// recorded as an optional import so it counts as a use but is never
    /// reported missing (`pip` rarely is declared).
    fn record_python_run(&mut self, expr: &Expr, elts: &[Expr]) {
        match python_run(elts) {
            Some(PythonRun::Module(module)) => {
                let line = self.line_number(expr);
                let context = self.current_import_context();
                self.parsed.imports.push(ImportRef {
                    module,
                    name: None,
                    alias: None,
                    line,
                    kind: ImportKind::Import,
                    context,
                    optional: true,
                    platform_guarded: false,
                    deferred: self.function_depth > 0,
                    relative_level: 0,
                });
            },
            Some(PythonRun::File) => self.parsed.runs_python_file = true,
            None => {},
        }
    }

    fn dynamic_import(&self, module: String, call: &ExprCall) -> DynamicImport {
        DynamicImport {
            module,
            line: self.line_number(call),
            optional: self.try_depth > 0,
            platform_guarded: self.platform_guard_depth > 0,
            deferred: self.function_depth > 0,
        }
    }

    /// pytest imports the modules a module-level `pytest_plugins` names, in
    /// a conftest or in a module it loaded as a plugin.
    fn record_pytest_plugins(&mut self, target: &str, value: &Expr) {
        if target != "pytest_plugins" {
            return;
        }
        for (module, literal) in pytest_plugin_names(value) {
            let line = self.line_number(literal);
            self.parsed.pytest_plugins.push(DynamicImport {
                module: module.to_owned(),
                line,
                ..DynamicImport::default()
            });
        }
    }

    fn visit_import(&mut self, import: &StmtImport) {
        let line = self.line_number(import);
        let context = self.current_import_context();
        let optional = self.try_depth > 0;
        let platform_guarded = self.platform_guard_depth > 0;
        let deferred = self.function_depth > 0;
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
                deferred,
                relative_level: 0,
            });
        }
    }

    fn visit_import_from(&mut self, import_from: &StmtImportFrom) {
        let line = self.line_number(import_from);
        let context = self.current_import_context();
        let optional = self.try_depth > 0;
        let platform_guarded = self.platform_guard_depth > 0;
        let deferred = self.function_depth > 0;
        let level = u8::try_from(import_from.level).unwrap_or(u8::MAX);
        let module_suffix = import_from.module.as_ref().map(ToString::to_string);

        for alias in &import_from.names {
            // `from m import *` still loads `m`; it is kept with name `*` so
            // `from . import *` resolves to the package itself and consumers
            // can tell it apart from `from . import m`.
            let is_star = alias.name.as_str() == "*";

            if level == 0 && !is_star {
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
                let name = if module_suffix.is_some() || is_star {
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
                deferred,
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

    fn record_symbol(
        &mut self,
        name: String,
        kind: SymbolKind,
        line: u32,
        decorators: &[Decorator],
    ) {
        let is_public =
            !name.starts_with('_') || self.parsed.exports.iter().any(|export| export == &name);
        let normalized = decorators
            .iter()
            .filter_map(|decorator| normalize_decorator(&decorator.expression))
            .collect();
        self.parsed.symbols.push(SymbolDef {
            name,
            kind,
            line,
            is_public,
            decorators: normalized,
            in_type_checking: self.in_type_checking,
            used_in_module: false,
        });
    }

    /// Record every statically named decorator, nested definitions included.
    ///
    /// Plugins (Flask routes, Celery tasks) read these instead of re-opening
    /// each source file, so app-factory patterns that decorate inside a
    /// function must land here too.
    fn record_decorators(&mut self, decorators: &[Decorator]) {
        for decorator in decorators {
            let Some(name) = normalize_decorator(&decorator.expression) else {
                continue;
            };
            let line = self.line_number(&decorator.expression);
            let is_call = matches!(decorator.expression, Expr::Call(_));
            self.parsed.decorator_sites.push(DecoratorSite {
                name,
                line,
                is_call,
            });
        }
    }

    fn line_number<R: Ranged>(&self, node: &R) -> u32 {
        self.lines.line(node.start())
    }
}

impl<'ast> Visitor<'ast> for ModuleVisitor<'_> {
    #[allow(clippy::too_many_lines)]
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::Import(import) => self.visit_import(import),
            Stmt::ImportFrom(import_from) => self.visit_import_from(import_from),
            Stmt::FunctionDef(def) => {
                self.visit_decorators(&def.decorator_list);
                if let Some(type_params) = &def.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_parameters(&def.parameters);
                if let Some(returns) = &def.returns {
                    self.visit_annotation(returns);
                }
                self.visit_def_body(
                    &def.name,
                    &def.decorator_list,
                    &def.body,
                    SymbolKind::Function,
                );
            },
            Stmt::ClassDef(def) => {
                self.visit_decorators(&def.decorator_list);
                if let Some(type_params) = &def.type_params {
                    self.visit_type_params(type_params);
                }
                if let Some(arguments) = &def.arguments {
                    self.visit_arguments(arguments);
                }
                self.visit_def_body(&def.name, &def.decorator_list, &def.body, SymbolKind::Class);
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
                            self.record_pytest_plugins(name.id.as_str(), &assign.value);
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
                    if let Some(value) = &ann_assign.value {
                        self.record_pytest_plugins(name.id.as_str(), value);
                    }
                }
                self.visit_expr(&ann_assign.target);
                self.visit_annotation(&ann_assign.annotation);
                if let Some(value) = &ann_assign.value {
                    self.visit_expr(value);
                }
            },
            Stmt::TypeAlias(alias) => {
                if self.module_level
                    && let Expr::Name(name) = &*alias.name
                {
                    let line = self.line_number(alias);
                    self.record_symbol(name.id.to_string(), SymbolKind::Variable, line, &[]);
                }
                if let Some(type_params) = &alias.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_expr(&alias.value);
            },
            Stmt::AugAssign(aug_assign) => {
                if self.module_level
                    && matches!(aug_assign.op, Operator::Add)
                    && let Expr::Name(name) = &*aug_assign.target
                {
                    self.record_pytest_plugins(name.id.as_str(), &aug_assign.value);
                }
                self.visit_expr(&aug_assign.value);
            },
            Stmt::Return(return_stmt) => {
                if let Some(value) = &return_stmt.value {
                    self.visit_expr(value);
                }
            },
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::If(if_stmt) => {
                self.visit_branch(Some(&if_stmt.test), &if_stmt.body);
                for clause in &if_stmt.elif_else_clauses {
                    self.visit_branch(clause.test.as_ref(), &clause.body);
                }
            },
            Stmt::Try(try_stmt) => {
                if self.module_level {
                    self.try_flags.extend(true_flags(&try_stmt.body));
                }
                self.try_depth = self.try_depth.saturating_add(1);
                self.visit_body(&try_stmt.body);
                self.try_depth = self.try_depth.saturating_sub(1);
                for handler in &try_stmt.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(exc_type) = &handler.type_ {
                        self.visit_expr(exc_type);
                    }
                    self.visit_body(&handler.body);
                }
                // `else:` runs only when the `try` body raised nothing, so its
                // imports are as optional as the body's.
                self.try_depth = self.try_depth.saturating_add(1);
                self.visit_body(&try_stmt.orelse);
                self.try_depth = self.try_depth.saturating_sub(1);
                self.visit_body(&try_stmt.finalbody);
            },
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                }
                let suppresses = with_stmt
                    .items
                    .iter()
                    .any(|item| suppresses_import_error(&item.context_expr));
                if suppresses {
                    self.try_depth = self.try_depth.saturating_add(1);
                }
                self.visit_body(&with_stmt.body);
                if suppresses {
                    self.try_depth = self.try_depth.saturating_sub(1);
                }
            },
            Stmt::Match(match_stmt) => {
                self.visit_expr(&match_stmt.subject);
                for case in &match_stmt.cases {
                    if let Some(guard) = &case.guard {
                        self.visit_expr(guard);
                    }
                    self.visit_body(&case.body);
                }
            },
            Stmt::For(for_stmt) => {
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

    /// Quoted forward references (`x: "list[T]"`) read the names inside them.
    fn visit_annotation(&mut self, expr: &'ast Expr) {
        AnnotationNames(&mut self.loaded_names).visit_expr(expr);
        walk_annotation(self, expr);
    }

    /// Walk one expression for dynamic imports and module attribute accesses.
    ///
    /// Both used to be separate full-tree passes; folding them into the statement
    /// walk keeps cold parse to a single traversal per file.
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Name(name) if name.ctx.is_load() => {
                self.loaded_names.insert(name.id.to_string());
            },
            Expr::Attribute(attribute) => {
                if let Some(receiver) = attribute_receiver(&attribute.value) {
                    let line = self.line_number(attribute);
                    self.parsed.attribute_accesses.push(AttributeAccess {
                        receiver,
                        name: attribute.attr.to_string(),
                        line,
                    });
                }
            },
            Expr::Call(call) => {
                let arguments = &call.arguments;
                if self.loader_names.is_loader(&call.func) {
                    if let Some(target) =
                        literal_target(call, || module_package(self.path, self.layout))
                    {
                        match target {
                            LiteralTarget::Module(module) => {
                                let dynamic = self.dynamic_import(module, call);
                                self.parsed.dynamic_imports.push(dynamic);
                            },
                            LiteralTarget::Opaque => self.parsed.has_opaque_dynamic_import = true,
                            LiteralTarget::Nothing => {},
                        }
                    } else if !arguments.args.is_empty() || !arguments.keywords.is_empty() {
                        if let Some(module) = module_prefix(call) {
                            let prefix = self.dynamic_import(module, call);
                            self.parsed.dynamic_import_prefixes.push(prefix);
                        }
                        self.parsed.has_opaque_dynamic_import = true;
                    }
                }
                // `map(importlib.import_module, names)` loads modules this walk never sees.
                if arguments
                    .args
                    .iter()
                    .chain(arguments.keywords.iter().map(|keyword| &keyword.value))
                    .any(|arg| self.loader_names.is_loader(arg))
                {
                    self.parsed.has_opaque_dynamic_import = true;
                }
            },
            Expr::List(list) => self.record_python_run(expr, &list.elts),
            Expr::Tuple(tuple) => self.record_python_run(expr, &tuple.elts),
            Expr::StringLiteral(_) => {
                if let Some(word) = command_word(expr) {
                    self.command_words.insert(word.to_owned());
                }
            },
            _ => {},
        }
        let lambda = matches!(expr, Expr::Lambda(_));
        if lambda {
            self.function_depth += 1;
        }
        walk_expr(self, expr);
        if lambda {
            self.function_depth -= 1;
        }
    }
}

/// Every name is collected, the ones outside strings included, because
/// a parsed string annotation is a fresh tree the module walk never reaches.
struct AnnotationNames<'n>(&'n mut HashSet<String>);

impl<'a> Visitor<'a> for AnnotationNames<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => {
                self.0.insert(name.id.to_string());
            },
            Expr::StringLiteral(string) => {
                if let Ok(parsed) = ruff_python_parser::parse_expression(string.value.to_str()) {
                    self.visit_expr(parsed.expr());
                }
            },
            // `Literal["fast"]` and the metadata of `Annotated[T, Field(alias="x")]`
            // hold values, not names.
            Expr::Subscript(subscript) => match subscript_name(&subscript.value) {
                Some("Literal") => self.visit_expr(&subscript.value),
                Some("Annotated") => {
                    self.visit_expr(&subscript.value);
                    match &*subscript.slice {
                        Expr::Tuple(tuple) => {
                            if let Some(first) = tuple.elts.first() {
                                self.visit_expr(first);
                            }
                        },
                        slice => self.visit_expr(slice),
                    }
                },
                _ => walk_expr(self, expr),
            },
            _ => walk_expr(self, expr),
        }
    }
}

fn subscript_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        _ => None,
    }
}

/// Names a `try` body assigns the literal `True` (`has_x = True`).
fn true_flags(body: &[Stmt]) -> impl Iterator<Item = String> + '_ {
    body.iter().filter_map(|stmt| {
        let Stmt::Assign(assign) = stmt else {
            return None;
        };
        let ([Expr::Name(name)], Expr::BooleanLiteral(value)) =
            (assign.targets.as_slice(), &*assign.value)
        else {
            return None;
        };
        value.value.then(|| name.id.to_string())
    })
}

/// `suppress(...)` / `contextlib.suppress(...)` swallowing a failed import
/// (#614).
fn suppresses_import_error(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let is_suppress = match &*call.func {
        Expr::Name(name) => name.id.as_str() == "suppress",
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "suppress"
                && matches!(&*attribute.value, Expr::Name(module) if module.id.as_str() == "contextlib")
        },
        _ => false,
    };
    if !is_suppress {
        return false;
    }
    call.arguments.args.iter().any(|arg| {
        matches!(
            subscript_name(arg),
            Some("ImportError" | "ModuleNotFoundError" | "Exception" | "BaseException")
        )
    })
}

fn alias_as_name(alias: &Alias) -> Option<String> {
    alias.asname.as_ref().map(ToString::to_string)
}

/// The name an import binds in the importing module, if any.
fn import_binding(import: &ImportRef) -> Option<&str> {
    if let Some(alias) = &import.alias {
        return Some(alias);
    }
    match (import.kind, import.name.as_deref()) {
        (ImportKind::ImportFrom, Some("*")) => None,
        (ImportKind::ImportFrom, Some(name)) => Some(name),
        // `from . import x` carries no `name`: the parser folds `x` into `module`.
        (ImportKind::ImportFrom, None) => import.module.rsplit('.').next(),
        (ImportKind::Import, _) => import.module.split('.').next(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::ProjectLayout;

    fn visit_source(source: &str) -> ParsedModule {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            package_root: String::new(),
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new("mod.py", &layout, FileContext::Runtime, &lines);
        visitor.visit_module(module.suite());
        visitor.into_parsed()
    }

    #[test]
    fn python_runs_record_optional_imports_and_file_runs() {
        let parsed = visit_source(
            "import subprocess, sys\ncmd = [\n    sys.executable,\n    \"-m\",\n    \"virtualenv\",\n]\n",
        );
        let run = parsed
            .imports
            .iter()
            .find(|import| import.module == "virtualenv")
            .expect("virtualenv import");
        assert!(run.optional);
        assert_eq!(run.line, 2);
        assert!(!parsed.runs_python_file);

        let parsed =
            visit_source("import subprocess, sys\nsubprocess.run([sys.executable, path])\n");
        assert!(parsed.runs_python_file);
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
    fn records_import_bindings_read_as_names() {
        let parsed = visit_source(
            "import os.path\nimport json as js\nfrom acme import used, unused, orig as alias\nfrom acme.models import *\n\ndef f():\n    return used, alias, os.sep, js\n\nunused = 1\n",
        );
        assert_eq!(parsed.used_import_bindings, ["alias", "js", "os", "used"]);
    }

    #[test]
    fn marks_symbols_read_in_their_own_module() {
        let parsed = visit_source(
            "T = TypeVar(\"T\")\n\nclass Res:\n    pass\n\nclass C:\n    def get(self) -> Res:\n        x: T\n\ndef unused():\n    pass\n",
        );
        let used: Vec<_> = parsed
            .symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.used_in_module))
            .collect();
        assert_eq!(
            used,
            [("T", true), ("Res", true), ("C", false), ("unused", false)]
        );
    }

    #[test]
    fn marks_symbols_read_in_string_annotations() {
        let parsed = visit_source(
            "T = TypeVar(\"T\")\nP = 1\nSelf = 1\nfast = 1\n\ndef f(x: \"list[T]\") -> list[\"P\"]:\n    y: \"'Self'\" = 1\n\nz: Literal[\"fast\"]\nv: typing.Literal[\"fast\"]\nw: Annotated[\"T\", Field(alias=\"model\")]\nmodel = 1\n",
        );
        let used: Vec<_> = parsed
            .symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.used_in_module))
            .collect();
        assert_eq!(
            used,
            [
                ("T", true),
                ("P", true),
                ("Self", true),
                ("fast", false),
                ("f", false),
                ("z", false),
                ("v", false),
                ("w", false),
                ("model", false)
            ]
        );
    }

    #[test]
    fn keeps_star_import_as_module_import() {
        let parsed = visit_source("from acme.models import *\n");
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.imports[0].module, "acme.models");
        assert_eq!(parsed.imports[0].name.as_deref(), Some("*"));
        assert_eq!(parsed.imports[0].kind, ImportKind::ImportFrom);
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
    fn extracts_pytest_plugins_literals() {
        let modules = |source: &str| {
            visit_source(source)
                .pytest_plugins
                .iter()
                .map(|dynamic| (dynamic.module.clone(), dynamic.line))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            modules("pytest_plugins = [\n    \"tests.optional\",\n    name,\n    \"\",\n]\n"),
            [("tests.optional".to_owned(), 2)]
        );
        assert_eq!(
            modules("pytest_plugins = (\"a.b\", \"c\")\n"),
            [("a.b".to_owned(), 1), ("c".to_owned(), 1)]
        );
        assert_eq!(
            modules("pytest_plugins: str = \"a.b\"\n"),
            [("a.b".to_owned(), 1)]
        );
        assert_eq!(
            modules("pytest_plugins = [\"a\"]\npytest_plugins += [\"b\"]\n"),
            [("a".to_owned(), 1), ("b".to_owned(), 2)]
        );
        assert_eq!(modules("def f():\n    pytest_plugins = [\"a.b\"]\n"), []);
        assert_eq!(modules("plugins = [\"a.b\"]\n"), []);
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
        assert_eq!(parsed.dynamic_imports, []);
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn records_command_words_only_with_subprocess() {
        let source = "cmd = \"ruff format --check\"\nrun(cmd, shell=True)\n";
        assert_eq!(visit_source(source).shell_commands, Vec::<String>::new());
        let parsed = visit_source(&format!("import subprocess\n{source}"));
        assert_eq!(parsed.shell_commands, vec!["ruff".to_owned()]);
    }

    #[test]
    fn records_prefix_of_built_module_name() {
        let parsed =
            visit_source("import importlib\nimportlib.import_module(\"acme.commands.\" + name)\n");
        assert_eq!(parsed.dynamic_imports, []);
        let prefixes: Vec<_> = parsed
            .dynamic_import_prefixes
            .iter()
            .map(|dynamic| (dynamic.module.as_str(), dynamic.line))
            .collect();
        assert_eq!(prefixes, vec![("acme.commands", 2)]);
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
        assert_eq!(parsed.dynamic_imports, []);
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
    fn drops_literal_names_python_cannot_import() {
        let parsed = visit_source(
            "import importlib\nimportlib.import_module(\"\", \"acme\")\nimportlib.import_module(\"a..b\")\nimportlib.import_module(\"pkg.\")\nimportlib.import_module(\".sub\")\nimportlib.import_module(\".sub\", None)\n__import__(\"\")\n",
        );
        assert_eq!(parsed.dynamic_imports, []);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn keeps_literal_names_with_non_identifier_segments() {
        let parsed =
            visit_source("import importlib\nimportlib.import_module(\"tests.my-harness.case\")\n");
        let modules: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|dynamic| dynamic.module.as_str())
            .collect();
        assert_eq!(modules, vec!["tests.my-harness.case"]);
    }

    #[test]
    fn resolves_relative_literal_against_package_argument() {
        let source = "import importlib\nimportlib.import_module(\".sub\", __package__)\nimportlib.import_module(\"..api\", package=\"acme.core\")\n";
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(
            "src/acme/__init__.py",
            &layout,
            FileContext::Runtime,
            &lines,
        );
        visitor.visit_module(module.suite());
        let parsed = visitor.into_parsed();
        let modules: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|dynamic| dynamic.module.as_str())
            .collect();
        assert_eq!(modules, vec!["acme.sub", "acme.api"]);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_relative_literal_with_unknown_package() {
        let parsed = visit_source("import importlib\nimportlib.import_module(\".sub\", base)\n");
        assert_eq!(parsed.dynamic_imports, []);
        assert!(parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_non_literal_keyword() {
        let parsed = visit_source("import importlib\nimportlib.import_module(name=target)\n");
        assert_eq!(parsed.dynamic_imports, []);
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
        assert_eq!(parsed.dynamic_imports, []);
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

    fn optional_by_module(parsed: &ParsedModule) -> Vec<(&str, bool)> {
        parsed
            .imports
            .iter()
            .map(|import| (import.module.as_str(), import.optional))
            .collect()
    }

    /// #580: requests' `help.py` imports `cryptography` in `else:` and its
    /// `compat.py` guards `simplejson` behind a flag set in the `try` body.
    #[test]
    fn try_else_and_flag_guarded_imports_are_optional() {
        let parsed = visit_source(
            "try:\n    from urllib3.contrib import pyopenssl\nexcept ImportError:\n    import fallback_lib\nelse:\n    import cryptography\nfinally:\n    import finally_lib\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("urllib3.contrib", true),
                ("fallback_lib", false),
                ("cryptography", true),
                ("finally_lib", false),
            ]
        );

        let parsed = visit_source(
            "try:\n    import simplejson as json\n    has_simplejson = True\nexcept ImportError:\n    import json\n    has_simplejson = False\n\nif has_simplejson:\n    from simplejson import JSONDecodeError\nelse:\n    from json import JSONDecodeError\nif other_flag:\n    import other_lib\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("simplejson", true),
                ("json", false),
                ("simplejson", true),
                ("json", false),
                ("other_lib", false),
            ]
        );
    }

    /// #614: mlflow registers optional dataset constructors under
    /// `with suppress(ImportError):`.
    #[test]
    fn suppress_import_error_bodies_are_optional() {
        let parsed = visit_source(
            "import contextlib\nfrom contextlib import suppress\n\nwith suppress(ImportError):\n    import polars\nwith contextlib.suppress(KeyError, ModuleNotFoundError):\n    import pandas\nwith suppress(KeyError):\n    import yaml\nwith self.suppress(Exception):\n    import tomli\nwith open(path):\n    import toml\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("contextlib", false),
                ("contextlib", false),
                ("polars", true),
                ("pandas", true),
                ("yaml", false),
                ("tomli", false),
                ("toml", false),
            ]
        );
    }

    /// Only module-level flags guard imports: a function-local `if` may read
    /// a shadowing local.
    #[test]
    fn flag_guard_is_limited_to_module_level() {
        let parsed = visit_source(
            "try:\n    import ujson\n    has_ujson = True\nexcept ImportError:\n    has_ujson = False\n\ndef f(has_ujson):\n    if has_ujson:\n        import inner_lib\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [("ujson", true), ("inner_lib", false)]
        );
    }

    /// #583: only function bodies defer an import; a class body runs at
    /// import time.
    #[test]
    fn imports_in_function_bodies_are_deferred() {
        let parsed = visit_source(
            "import top_lib

class C:
    import class_lib

    def m(self):
        import method_lib

def f():
    def inner():
        import nested_lib
    import func_lib
    from from_lib import x
    cmd = [sys.executable, \"-m\", \"run_lib\"]

from top_from_lib import y
cmd = [sys.executable, \"-m\", \"top_run_lib\"]
",
        );
        let deferred: Vec<(&str, bool)> = parsed
            .imports
            .iter()
            .map(|import| (import.module.as_str(), import.deferred))
            .collect();
        assert_eq!(
            deferred,
            [
                ("top_lib", false),
                ("class_lib", false),
                ("method_lib", true),
                ("nested_lib", true),
                ("func_lib", true),
                ("from_lib", true),
                ("run_lib", true),
                ("top_from_lib", false),
                ("top_run_lib", false),
            ]
        );
    }

    /// #599: dynamic imports carry the same strength flags as static ones.
    #[test]
    fn dynamic_imports_record_try_guard_and_function_flags() {
        let parsed = visit_source(
            "import importlib
import sys

importlib.import_module(\"top_lib\")
try:
    importlib.import_module(\"try_lib\")
except ImportError:
    pass
if sys.platform == \"win32\":
    __import__(\"win_lib\")

def f():
    importlib.import_module(\"func_lib\")

LOADERS = {\"pg\": lambda: importlib.import_module(\"lambda_lib\")}
importlib.import_module(\"after_lambda_lib\")
",
        );
        let flags: Vec<(&str, bool, bool, bool)> = parsed
            .dynamic_imports
            .iter()
            .map(|dynamic| {
                (
                    dynamic.module.as_str(),
                    dynamic.optional,
                    dynamic.platform_guarded,
                    dynamic.deferred,
                )
            })
            .collect();
        assert_eq!(
            flags,
            [
                ("top_lib", false, false, false),
                ("try_lib", true, false, false),
                ("win_lib", false, true, false),
                ("func_lib", false, false, true),
                ("lambda_lib", false, false, true),
                ("after_lambda_lib", false, false, false),
            ]
        );
    }

    /// #610: a prefixed loader call inside a function is deferred as well.
    #[test]
    fn dynamic_import_prefixes_record_function_flag() {
        let parsed = visit_source(
            "import importlib

importlib.import_module(\"top.\" + name)

def f(name):
    importlib.import_module(f\"func.{name}\")
",
        );
        let flags: Vec<(&str, bool)> = parsed
            .dynamic_import_prefixes
            .iter()
            .map(|prefix| (prefix.module.as_str(), prefix.deferred))
            .collect();
        assert_eq!(flags, [("top", false), ("func", true)]);
    }

    #[test]
    fn walks_elif_branches_with_guards() {
        let parsed = visit_source(
            "import sys\nfrom typing import TYPE_CHECKING\nif sys.platform == \"win32\":\n    import winreg\nelif TYPE_CHECKING:\n    import typed_lib\nelse:\n    import plain_lib\n",
        );
        let find = |module: &str| {
            parsed
                .imports
                .iter()
                .find(|import| import.module == module)
                .expect("import recorded")
        };
        assert!(find("winreg").platform_guarded);
        assert_eq!(find("typed_lib").context, ImportContext::Type);
        assert!(!find("typed_lib").platform_guarded);
        assert_eq!(find("plain_lib").context, ImportContext::Runtime);
    }

    #[test]
    fn records_decorated_symbol_on_def_line() {
        let parsed = visit_source("@decorate\n@other\ndef f():\n    pass\n");
        let symbol = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "f")
            .expect("symbol f");
        assert_eq!(symbol.line, 3);
    }

    #[test]
    fn parses_python_313_and_314_syntax() {
        // PEP 695 type params and aliases, PEP 758 bare except tuples, PEP 750 t-strings.
        let source = "\
from acme import utils
type Alias[K: utils.K] = utils.A
def f[T: utils.D = utils.E](x: T) -> utils.B:
    pass
class C[T: utils.F](utils.Base):
    pass
try:
    pass
except utils.G, TypeError:
    pass
value = t\"{utils.C}\"
";
        let parsed = visit_source(source);
        for (name, line) in [
            ("K", 2),
            ("A", 2),
            ("D", 3),
            ("E", 3),
            ("B", 3),
            ("F", 5),
            ("Base", 5),
            ("G", 9),
            ("C", 11),
        ] {
            assert_eq!(attribute_lines(&parsed, name), vec![line], "utils.{name}");
        }
        let alias = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Alias")
            .expect("symbol Alias");
        assert_eq!((alias.kind, alias.line), (SymbolKind::Variable, 2));
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == "f"));
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == "C"));
    }

    #[test]
    fn treats_lazy_import_as_import() {
        let parsed = visit_source("lazy import json\nlazy from acme import utils\n");
        let modules: Vec<_> = parsed
            .imports
            .iter()
            .map(|import| import.module.as_str())
            .collect();
        assert_eq!(modules, vec!["json", "acme"]);
    }
}
