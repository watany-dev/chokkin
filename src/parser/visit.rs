//! AST visitor for imports, symbols, and dynamic references.

use std::collections::{BTreeSet, HashMap, HashSet};

use ruff_python_ast::visitor::{Visitor, walk_annotation, walk_expr};
use ruff_python_ast::{
    Alias, BoolOp, CmpOp, Decorator, ExceptHandler, Expr, ExprCall, Identifier, Operator, Stmt,
    StmtIf, StmtImport, StmtImportFrom,
};
use ruff_text_size::{Ranged, TextSize};

use crate::sources::{FileContext, LayoutInfo};

use super::attributes::attribute_receiver;
use super::decorators::normalize_decorator;
use super::dynamic::{
    LiteralTarget, LoaderNames, PythonRun, command_word, edits_sys_path, literal_target,
    module_prefix, pytest_plugin_names, python_run, sys_path_hint,
};
use super::exports::extract_exports;
use super::lines::LineIndex;
use super::module_guard::{
    availability_test, first_availability_check, imported_module_guard, is_main_guard_test,
    raises_import_error, runs_as_main,
};
use super::platform_guard::is_platform_guard_test;
use super::relative::{module_package, resolve_relative_import, unresolved_relative_diagnostic};
use super::type_checking::is_type_checking_test;
use super::types::{
    AttributeAccess, DecoratorSite, DynamicImport, ImportContext, ImportKind, ImportRef,
    ParsedModule, SymbolDef, SymbolKind, import_context_for_file,
};

/// Whether the module edits `sys.path`, and its path-like literals (#719).
#[derive(Default)]
struct SysPathHints {
    edited: bool,
    literals: BTreeSet<String>,
}

/// Mutable parse state accumulated while visiting one module.
pub(super) struct ModuleVisitor<'a> {
    path: &'a str,
    layout: &'a LayoutInfo,
    lines: &'a LineIndex,
    default_context: ImportContext,
    in_type_checking: bool,
    try_depth: u32,
    /// Inside an `except ImportError:` handler: the roots its `try` body
    /// imports (#721).
    fallback_for: Vec<String>,
    platform_guard_depth: u32,
    /// Top-level modules an enclosing branch checked are already imported
    /// (`"x" in sys.modules`); imports of them are optional (#681).
    imported_guards: Vec<String>,
    /// Inside `if __name__ == "__main__":` of a module other than
    /// `__main__.py`; its imports are dev context (#681).
    in_main_block: bool,
    /// Where the enclosing function first checks if a package is installed;
    /// imports after it are optional (#695).
    availability_checked_at: Option<TextSize>,
    function_depth: u32,
    module_level: bool,
    /// Module-level names that are truthy only when a `try` import succeeded:
    /// set to `True` in the body (`has_x = True`) or to `None` in its
    /// `except ImportError:` handler (`x = None`, #730).
    try_flags: HashSet<String>,
    typing_aliases: HashSet<String>,
    type_checking_names: HashSet<String>,
    loader_names: LoaderNames,
    /// Names the current scope binds to a module a prefixed loader call
    /// returned (`module = import_module("pkg." + name)`), with that call's
    /// index in `dynamic_import_prefixes` (#728).
    prefix_modules: HashMap<String, usize>,
    command_words: BTreeSet<String>,
    sys_path: SysPathHints,
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
            fallback_for: Vec::new(),
            platform_guard_depth: 0,
            imported_guards: Vec::new(),
            in_main_block: false,
            availability_checked_at: None,
            function_depth: 0,
            module_level: true,
            try_flags: HashSet::new(),
            typing_aliases: HashSet::from(["typing".to_owned()]),
            type_checking_names: HashSet::from(["TYPE_CHECKING".to_owned()]),
            loader_names: LoaderNames::default(),
            prefix_modules: HashMap::new(),
            command_words: BTreeSet::new(),
            sys_path: SysPathHints::default(),
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
        // The added path is often built in a variable or another scope, so
        // every literal of the module is a hint, not just the call's own.
        if self.sys_path.edited {
            self.parsed.sys_path_hints = self.sys_path.literals.into_iter().collect();
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
    /// `optional` marks a branch that only runs when a package is installed.
    fn visit_branch<'ast>(&mut self, test: Option<&'ast Expr>, body: &'ast [Stmt], optional: bool) {
        let was_type_checking = self.in_type_checking;
        let was_platform_guard = self.platform_guard_depth;
        let was_try_depth = self.try_depth;
        let was_main_block = self.in_main_block;
        let guard_count = self.imported_guards.len();
        if optional {
            self.try_depth = self.try_depth.saturating_add(1);
        }
        if let Some(test) = test {
            self.visit_expr(test);
            if is_type_checking_test(test, &self.typing_aliases, &self.type_checking_names) {
                self.in_type_checking = true;
            }
            if is_platform_guard_test(test) {
                self.platform_guard_depth = self.platform_guard_depth.saturating_add(1);
            }
            self.imported_guards.extend(imported_module_guard(test));
            // `python -m pkg` runs `pkg/__main__.py` as `__main__`, and Jupyter
            // runs every cell as `__main__`, so there the block is the normal
            // path.
            if is_main_guard_test(test) && !runs_as_main(self.path) {
                self.in_main_block = true;
            }
            // `if has_x:` only runs when the guarded `try` import succeeded.
            if self.module_level && tests_flag(test, &self.try_flags) {
                self.try_depth = self.try_depth.saturating_add(1);
            }
        }
        self.visit_body(body);
        self.in_type_checking = was_type_checking;
        self.platform_guard_depth = was_platform_guard;
        self.try_depth = was_try_depth;
        self.in_main_block = was_main_block;
        self.imported_guards.truncate(guard_count);
    }

    /// Whether an import at `node` runs only when some package is installed.
    fn is_optional_at<R: Ranged>(&self, node: &R) -> bool {
        self.try_depth > 0
            || self
                .availability_checked_at
                .is_some_and(|at| at < node.start())
    }

    fn is_imported_guarded(&self, module: &str) -> bool {
        let root = module.split('.').next().unwrap_or(module);
        self.imported_guards.iter().any(|guard| guard == root)
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
        let saved_checked_at = self.availability_checked_at;
        let saved_prefix_modules = std::mem::take(&mut self.prefix_modules);
        self.module_level = false;
        if matches!(kind, SymbolKind::Function) {
            self.function_depth += 1;
            // A function that asks whether a package is installed treats the
            // packages it imports after the check as optional (#695, #713).
            // A nested function runs when called, possibly after the outer
            // check, so all of its imports are.
            self.availability_checked_at = if saved_checked_at.is_some() {
                Some(TextSize::default())
            } else {
                first_availability_check(body)
            };
        }
        self.visit_body(body);
        self.module_level = saved;
        self.function_depth = saved_function_depth;
        self.availability_checked_at = saved_checked_at;
        self.prefix_modules = saved_prefix_modules;
    }

    /// Track `name = import_module("pkg." + x)` for a later
    /// `getattr(name, attr)`; any other assignment drops the binding.
    /// `index` is where the value's own prefix import would be recorded.
    fn bind_prefix_module(&mut self, target: &Expr, value: &Expr, index: usize) {
        let Expr::Name(name) = target else {
            return;
        };
        let loads_prefix = matches!(
            value,
            Expr::Call(call) if self.loader_names.loader(&call.func).is_some()
                && module_prefix(call).is_some()
        );
        if loads_prefix {
            self.prefix_modules.insert(name.id.to_string(), index);
        } else {
            self.prefix_modules.remove(name.id.as_str());
        }
    }

    /// `getattr(module, <non-literal>)` on a name bound by
    /// [`Self::bind_prefix_module`] fetches an attribute the walk cannot name.
    /// A scope that first reads `getattr(module, "__all__")` walks the names
    /// that list exports (an import smoke test), so it drops the binding.
    fn record_computed_getattr(&mut self, call: &ExprCall) {
        if !matches!(&*call.func, Expr::Name(func) if func.id.as_str() == "getattr") {
            return;
        }
        let [Expr::Name(receiver), attribute, ..] = &*call.arguments.args else {
            return;
        };
        if let Expr::StringLiteral(literal) = attribute {
            if literal.value.to_str() == "__all__" {
                self.prefix_modules.remove(receiver.id.as_str());
            }
            return;
        }
        if let Some(prefix) = self
            .prefix_modules
            .get(receiver.id.as_str())
            .and_then(|index| self.parsed.dynamic_import_prefixes.get_mut(*index))
        {
            prefix.computed_getattr = true;
        }
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
                    fallback_for: Vec::new(),
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
            optional: self.is_optional_at(call),
            platform_guarded: self.platform_guard_depth > 0,
            deferred: self.function_depth > 0,
            computed_getattr: false,
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
        let optional = self.is_optional_at(import);
        let platform_guarded = self.platform_guard_depth > 0;
        let deferred = self.function_depth > 0;
        for alias in &import.names {
            let optional = optional || self.is_imported_guarded(alias.name.as_str());
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
                fallback_for: self.fallback_for.clone(),
            });
        }
    }

    fn visit_import_from(&mut self, import_from: &StmtImportFrom) {
        let line = self.line_number(import_from);
        let context = self.current_import_context();
        let optional = self.is_optional_at(import_from);
        let platform_guarded = self.platform_guard_depth > 0;
        let deferred = self.function_depth > 0;
        let level = u8::try_from(import_from.level).unwrap_or(u8::MAX);
        let module_suffix = import_from.module.as_ref().map(ToString::to_string);
        let optional = optional
            || (level == 0
                && module_suffix
                    .as_deref()
                    .is_some_and(|m| self.is_imported_guarded(m)));

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
                fallback_for: self.fallback_for.clone(),
            });
        }
    }

    fn push_import(&mut self, import: ImportRef) {
        self.parsed.imports.push(import);
    }

    fn current_import_context(&self) -> ImportContext {
        if self.in_type_checking {
            ImportContext::Type
        } else if self.in_main_block && self.default_context == ImportContext::Runtime {
            ImportContext::Dev
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
    /// Statements after `if not is_x_available(): raise ImportError(...)`
    /// only run with `x` installed (#695).
    fn visit_body(&mut self, body: &'ast [Stmt]) {
        let try_depth = self.try_depth;
        for stmt in body {
            self.visit_stmt(stmt);
            if let Stmt::If(if_stmt) = stmt
                && availability_test(&if_stmt.test) == Some(false)
                && raises_import_error(&if_stmt.body)
            {
                self.try_depth = try_depth.saturating_add(1);
            }
        }
        self.try_depth = try_depth;
    }

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
                let index = self.parsed.dynamic_import_prefixes.len();
                self.visit_expr(&assign.value);
                for target in &assign.targets {
                    self.bind_prefix_module(target, &assign.value, index);
                }
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
                    let index = self.parsed.dynamic_import_prefixes.len();
                    self.visit_expr(value);
                    self.bind_prefix_module(&ann_assign.target, value, index);
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
                // `if find_spec("x"):` / `if is_x_available():` runs only with
                // `x` installed, and so does every branch after
                // `if not is_x_available():` (#695).
                let mut after_missing = false;
                for (test, body) in if_branches(if_stmt) {
                    let installed = test.and_then(availability_test);
                    self.visit_branch(test, body, after_missing || installed == Some(true));
                    after_missing |= installed == Some(false);
                }
            },
            Stmt::Try(try_stmt) => {
                if self.module_level {
                    self.try_flags.extend(assigned_flags(
                        &try_stmt.body,
                        |value| matches!(value, Expr::BooleanLiteral(literal) if literal.value),
                    ));
                }
                self.try_depth = self.try_depth.saturating_add(1);
                self.visit_body(&try_stmt.body);
                self.try_depth = self.try_depth.saturating_sub(1);
                let primaries = imported_roots(&try_stmt.body);
                for handler in &try_stmt.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(exc_type) = &handler.type_ {
                        self.visit_expr(exc_type);
                    }
                    let import_failed = handler.type_.as_deref().is_some_and(catches_import_error);
                    if self.module_level && import_failed {
                        self.try_flags
                            .extend(assigned_flags(&handler.body, Expr::is_none_literal_expr));
                    }
                    let enclosing = match &primaries {
                        Some(roots) if import_failed => {
                            Some(std::mem::replace(&mut self.fallback_for, roots.clone()))
                        },
                        _ => None,
                    };
                    self.visit_body(&handler.body);
                    if let Some(enclosing) = enclosing {
                        self.fallback_for = enclosing;
                    }
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
                self.record_computed_getattr(call);
                self.sys_path.edited |= edits_sys_path(&call.func);
                let arguments = &call.arguments;
                if let Some(loader) = self.loader_names.loader(&call.func) {
                    if let Some(target) =
                        literal_target(call, loader, || module_package(self.path, self.layout))
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
                    .any(|arg| self.loader_names.loader(arg).is_some())
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
                if let Some(hint) = sys_path_hint(expr) {
                    self.sys_path.literals.insert(hint);
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

fn if_branches(if_stmt: &StmtIf) -> impl Iterator<Item = (Option<&Expr>, &[Stmt])> {
    std::iter::once((Some(&*if_stmt.test), if_stmt.body.as_slice())).chain(
        if_stmt
            .elif_else_clauses
            .iter()
            .map(|clause| (clause.test.as_ref(), clause.body.as_slice())),
    )
}

/// Names `body` assigns a value `is_flag` accepts (`has_x = True`, `x = None`).
fn assigned_flags(body: &[Stmt], is_flag: fn(&Expr) -> bool) -> impl Iterator<Item = String> + '_ {
    body.iter().filter_map(move |stmt| {
        let Stmt::Assign(assign) = stmt else {
            return None;
        };
        let [Expr::Name(name)] = assign.targets.as_slice() else {
            return None;
        };
        is_flag(&assign.value).then(|| name.id.to_string())
    })
}

/// Whether `test` passes only when a flag is truthy: `if x:`,
/// `if x is not None:`, or an `and` chain with either. `or`, `not x` and
/// `x is None` also pass without the package.
fn tests_flag(test: &Expr, flags: &HashSet<String>) -> bool {
    let is_flag =
        |expr: &Expr| matches!(expr, Expr::Name(name) if flags.contains(name.id.as_str()));
    match test {
        Expr::Compare(compare) => matches!(
            (&*compare.ops, &*compare.operands),
            ([CmpOp::IsNot], [flag, Expr::NoneLiteral(_)]) if is_flag(flag)
        ),
        Expr::BoolOp(bool_op) => {
            bool_op.op == BoolOp::And && bool_op.values.iter().any(|value| tests_flag(value, flags))
        },
        _ => is_flag(test),
    }
}

/// Roots a `try` body imports when it does nothing else, so only a missing
/// module can raise there; `None` for any other statement or a relative import.
fn imported_roots(body: &[Stmt]) -> Option<Vec<String>> {
    let root = |module: &str| module.split('.').next().unwrap_or(module).to_owned();
    let mut roots = Vec::new();
    for stmt in body {
        match stmt {
            Stmt::Import(import) => {
                roots.extend(import.names.iter().map(|alias| root(alias.name.as_str())));
            },
            Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                roots.push(root(import_from.module.as_ref()?.as_str()));
            },
            _ => return None,
        }
    }
    Some(roots)
}

/// `except ImportError:` / `except (ModuleNotFoundError, ...):`.
fn catches_import_error(exc_type: &Expr) -> bool {
    let is_import_error = |expr: &Expr| {
        matches!(
            subscript_name(expr),
            Some("ImportError" | "ModuleNotFoundError")
        )
    };
    match exc_type {
        Expr::Tuple(tuple) => tuple.elts.iter().any(is_import_error),
        _ => is_import_error(exc_type),
    }
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
        visit_source_at("mod.py", source)
    }

    fn visit_source_at(path: &str, source: &str) -> ParsedModule {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo::default();
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(path, &layout, FileContext::Runtime, &lines);
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
    fn marks_prefix_imports_read_by_computed_getattr() {
        let read = |source: &str| -> Vec<(String, bool)> {
            visit_source(source)
                .dynamic_import_prefixes
                .into_iter()
                .map(|prefix| (prefix.module, prefix.computed_getattr))
                .collect()
        };
        let prefix = |module: &str, computed: bool| vec![(module.to_owned(), computed)];
        assert_eq!(
            read(
                "from importlib import import_module\ndef load(name):\n    module = import_module(\"pkg.commands.\" + name)\n    return getattr(module, name.title() + \"Command\")\n"
            ),
            prefix("pkg.commands", true)
        );
        assert_eq!(
            read(
                "import importlib\ndef load(name):\n    module: object = importlib.import_module(f\"pkg.plugins.{name}\")\n    return getattr(module, attr, None)\n"
            ),
            prefix("pkg.plugins", true)
        );
        // A literal name, another scope, a rebound name, or a walk over
        // `__all__` is not this pattern.
        for source in [
            "import importlib\ndef check(name):\n    module = importlib.import_module(\"pkg.\" + name)\n    for attr in getattr(module, \"__all__\", []):\n        getattr(module, attr)\n",
            "import importlib\ndef load(name):\n    module = importlib.import_module(\"pkg.\" + name)\n    return getattr(module, \"Command\")\n",
            "import importlib\ndef load(name):\n    module = importlib.import_module(\"pkg.\" + name)\n    def get(attr):\n        return getattr(module, attr)\n    return get\n",
            "import importlib\ndef load(name):\n    module = importlib.import_module(\"pkg.\" + name)\ndef get(module, attr):\n    return getattr(module, attr)\n",
            "import importlib\ndef load(name):\n    module = importlib.import_module(\"pkg.\" + name)\n    module = other\n    return getattr(module, attr)\n",
        ] {
            assert_eq!(read(source), prefix("pkg", false), "{source}");
        }
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
    fn records_path_literals_only_when_sys_path_is_edited() {
        let source = "EXT = Path(__file__).parents[2] / \"docs/_ext\"\n";
        assert_eq!(visit_source(source).sys_path_hints, Vec::<String>::new());
        let parsed = visit_source(&format!(
            "{source}class T:\n    def setUp(self):\n        sys.path.insert(0, str(EXT))\n"
        ));
        assert_eq!(parsed.sys_path_hints, vec!["docs/_ext".to_owned()]);
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
            ..Default::default()
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
    fn resolves_dunder_import_level_against_globals() {
        let source = "__import__(\"sub\", globals(), None, (), 1)\n__import__(\"api\", globals(), level=2)\n__import__(\"\", globals(), None, (), 1)\n__import__(\"plain\", globals(), None, (), 0)\n__import__(\".x\", globals(), None, (), 1)\n__import__(\"x\", globals(), None, (), 4)\n";
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            ..Default::default()
        };
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(
            "src/acme/core/mod.py",
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
        assert_eq!(
            modules,
            vec!["acme.core.sub", "acme.api", "acme.core", "plain"]
        );
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn does_not_read_dunder_import_globals_as_package() {
        let parsed = visit_source(
            "__import__(\".x\", \"pkg\")\n__import__(\"x\", level=1)\n__import__(\"x\", None, None, (), 1)\n",
        );
        assert_eq!(parsed.dynamic_imports, []);
        assert!(!parsed.has_opaque_dynamic_import);
    }

    #[test]
    fn marks_opaque_dunder_import_with_unknown_level_or_globals() {
        for source in [
            "__import__(\"x\", globals(), None, (), depth)\n",
            "__import__(\"x\", namespace, None, (), 1)\n",
        ] {
            let parsed = visit_source(source);
            assert_eq!(parsed.dynamic_imports, [], "{source}");
            assert!(parsed.has_opaque_dynamic_import, "{source}");
        }
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

    /// Imports in an `except ImportError:` handler carry the roots the `try`
    /// body imports when the body does nothing else (#721).
    #[test]
    fn records_try_body_roots_on_import_error_fallbacks() {
        let parsed = visit_source(
            "try:\n    import threading, os.path\n    from http.server import X\nexcept (ImportError, OSError):\n    import dummy_threading\n    if flag:\n        from SimpleHTTPServer import X\nelse:\n    import else_lib\n\ntry:\n    import a\n    run()\nexcept ImportError:\n    import call_fallback\n\ntry:\n    from . import sibling\nexcept ModuleNotFoundError:\n    import relative_fallback\n\ntry:\n    from .compat import x\nexcept ImportError:\n    import relative_module_fallback\n\ntry:\n    import b\nexcept ValueError:\n    import value_fallback\nexcept ModuleNotFoundError:\n    import missing_fallback\n",
        );
        let fallback_for = |module: &str| {
            parsed
                .imports
                .iter()
                .find(|import| import.module == module)
                .map(|import| import.fallback_for.clone())
                .expect(module)
        };
        let primaries = vec!["threading".to_owned(), "os".to_owned(), "http".to_owned()];
        assert_eq!(fallback_for("dummy_threading"), primaries);
        assert_eq!(fallback_for("SimpleHTTPServer"), primaries);
        for module in [
            "threading",
            "else_lib",
            "call_fallback",
            "relative_fallback",
            "relative_module_fallback",
            "value_fallback",
        ] {
            assert!(fallback_for(module).is_empty(), "{module}");
        }
        assert_eq!(fallback_for("missing_fallback"), vec!["b".to_owned()]);
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

    /// #730: drf's `compat.py` sets `markdown = None` when the import fails
    /// and guards the rest behind `if markdown is not None and ...:`.
    #[test]
    fn none_fallback_guarded_imports_are_optional() {
        let parsed = visit_source(
            "try:
    import markdown
except ImportError:
    markdown = None
try:
    import pygments
except (ModuleNotFoundError, AttributeError):
    pygments = None
try:
    import yaml
    has_yaml = True
except ImportError:
    has_yaml = False

if markdown is not None and pygments is not None:
    from markdown.preprocessors import Preprocessor
if pygments:
    from pygments.lexers import TextLexer
if unrelated and (enabled and markdown):
    import markdown.extensions
if has_yaml and enabled:
    import yaml.nodes
",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("markdown", true),
                ("pygments", true),
                ("yaml", true),
                ("markdown.preprocessors", true),
                ("pygments.lexers", true),
                ("markdown.extensions", true),
                ("yaml.nodes", true),
            ]
        );
    }

    /// #730: a branch that also runs without the package is not a guard.
    #[test]
    fn none_fallback_guard_needs_a_truthy_flag() {
        let parsed = visit_source(
            "try:
    import yaml
except ImportError:
    yaml = None
try:
    import ujson
except ValueError:
    ujson = None

if yaml is None:
    import a
if not yaml:
    import b
if yaml or other:
    import c
if yaml is not None:
    pass
elif other:
    import d
else:
    import e
if ujson is not None:
    import f
if (yaml and other) is not None:
    import g
if yaml is not other:
    import h

def func(yaml):
    if yaml is not None:
        import i
",
        );
        let optional: Vec<_> = parsed
            .imports
            .iter()
            .filter(|import| import.optional)
            .map(|import| import.module.as_str())
            .collect();
        assert_eq!(optional, ["yaml", "ujson"]);
        assert_eq!(parsed.imports.len(), 11);
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

    /// #695: transformers and `llama_index` import packages behind
    /// `is_x_available()` / `find_spec()` checks.
    #[test]
    fn availability_checked_imports_are_optional() {
        let parsed = visit_source(
            "import importlib.util
if is_torch_available():
    import torch
else:
    import fallback_lib
if importlib.util.find_spec('wandb') is not None:
    import wandb
import plain_lib

def peft_model():
    import before_lib
    if importlib.util.find_spec('peft') is None:
        raise ImportError('pip install peft')
    from peft import AutoPeftModel

def quanto():
    if not is_optimum_quanto_available():
        raise ImportError('pip install optimum-quanto')
    elif is_quanto_greater('0.2.5'):
        from optimum.quanto import qint2
    else:
        raise ImportError('upgrade')
    import after_lib

def other():
    if version < 3:
        raise ImportError('old')
    import unguarded_lib

if not is_y_available():
    import y_fallback
else:
    import y
if is_z_available():
    raise ImportError('z conflicts')
import after_positive

if not is_x_available():
    raise ImportError('pip install x')
import x
",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("importlib.util", false),
                ("torch", true),
                ("fallback_lib", false),
                ("wandb", true),
                ("plain_lib", false),
                ("before_lib", false),
                ("peft", true),
                ("optimum.quanto", true),
                ("after_lib", true),
                ("unguarded_lib", false),
                ("y_fallback", false),
                ("y", true),
                ("after_positive", false),
                ("x", true),
            ]
        );
    }

    /// #713: only a package check before the import makes it optional; a
    /// method or a finder's `find_spec` is not a package check.
    #[test]
    fn function_availability_check_must_precede_the_import() {
        let parsed = visit_source(
            "import importlib.util
class Loader:
    def load(self):
        import requests
        if self.__is_headers_available():
            return requests.get

def run(finder):
    import yaml
    return finder.find_spec('x', None), yaml

def f():
    import before_lib
    if importlib.util.find_spec('rich'):
        pass
    import after_lib
    def inner():
        import nested_lib

def g():
    def inner():
        import early_nested_lib
    if is_x_available():
        inner()

def h():
    return importlib.import_module('orjson') if find_spec('orjson') else None

def k():
    importlib.import_module('plugin').setup(find_spec('extra'))
",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("importlib.util", false),
                ("requests", false),
                ("yaml", false),
                ("before_lib", false),
                ("after_lib", true),
                ("nested_lib", true),
                ("early_nested_lib", true),
            ]
        );
        let dynamic: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|import| (import.module.as_str(), import.optional))
            .collect();
        assert_eq!(dynamic, [("orjson", true), ("plugin", false)]);
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

    /// #681: only imports of the module the branch checked are optional.
    #[test]
    fn already_imported_checks_make_matching_imports_optional() {
        let parsed = visit_source(
            "import sys, sniffio
if \"pyspark\" in sys.modules:
    from pyspark.sql import DataFrame
    import pandas
def create_event():
    if sniffio.current_async_library() == \"trio\":
        import trio
    else:
        import asyncio
import trio_util
",
        );
        let optional: Vec<(&str, bool)> = parsed
            .imports
            .iter()
            .map(|import| (import.module.as_str(), import.optional))
            .collect();
        assert_eq!(
            optional,
            [
                ("sys", false),
                ("sniffio", false),
                ("pyspark.sql", true),
                ("pandas", false),
                ("trio", true),
                ("asyncio", false),
                ("trio_util", false),
            ]
        );
    }

    /// #681: a `__main__` block is dev context, except in `__main__.py`.
    #[test]
    fn main_block_imports_are_dev_context() {
        let source = "import os\nif __name__ == \"__main__\":\n    import requests\nimport json\n";
        let contexts = |path| {
            visit_source_at(path, source)
                .imports
                .into_iter()
                .map(|import| import.context)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            contexts("pkg/emoji.py"),
            [
                ImportContext::Runtime,
                ImportContext::Dev,
                ImportContext::Runtime
            ]
        );
        assert_eq!(contexts("pkg/__main__.py"), [ImportContext::Runtime; 3]);
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
