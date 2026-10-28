//! AST visitor for imports, symbols, and dynamic references.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rustc_hash::{FxHashMap, FxHashSet};

use ruff_python_ast::visitor::{Visitor, walk_annotation, walk_expr};
use ruff_python_ast::{
    Alias, CmpOp, Decorator, ExceptHandler, Expr, ExprAttribute, ExprCall, ExprStringLiteral,
    Identifier, Operator, Parameters, Stmt, StmtClassDef, StmtIf, StmtImport, StmtImportFrom,
    StmtMatch, StmtTry,
};
use ruff_text_size::{Ranged, TextSize};

use crate::sources::{FileContext, LayoutInfo};

use super::attributes::write_receiver;
use super::by_name::{
    adds_computed_path, callee_name, gets_own_name, path_refs, reads_own_namespace,
};
use super::condition::{self, Leaf};
use super::decorators::normalize_decorator;
use super::deprecation::warns_deprecated_on_import;
use super::dynamic::{
    LiteralTarget, Loader, LoaderNames, PythonRun, command_word, edits_sys_path, lists_directory,
    literal_target, module_prefix, module_ref_literal, named_load, pytest_plugin_names, python_run,
    sys_path_hint,
};
use super::exports::extract_exports;
use super::lines::LineIndex;
use super::module_guard::{
    async_library_names, availability_test, exits_block, first_availability_check, guarded_roots,
    imported_module_guard, is_find_spec_value, is_install_check, is_main_guard_test,
    is_module_path, loaded_test, runs_as_main,
};
use super::platform_guard::{
    PlatformModules, bound_names, imports_platform_alias, is_platform_guard_test,
    mentions_platform_test, platform_modules,
};
use super::registry::registry_decorators;
use super::relative::{module_package, resolve_relative_import, unresolved_relative_diagnostic};
use super::server_app::ServerNames;
use super::type_checking::is_type_checking_test;
use super::types::{
    AttributeAccess, DecoratorSite, DynamicImport, ImportCheck, ImportContext, ImportKind,
    ImportRef, ModuleRef, NamedLoad, ParsedModule, ServerApp, SymbolDef, SymbolKind,
    import_context_for_file,
};
use super::version_guard::{self, ALL_MINORS, Minors, VersionNames};

/// Whether the module edits `sys.path`, and its path-like literals (#719).
#[derive(Default)]
struct SysPathHints {
    edited: bool,
    literals: BTreeSet<String>,
}

/// Whether the source can hold what [`async_library_names`],
/// [`first_availability_check`] and [`platform_modules`] look for. Most
/// modules cannot, and the scans rewalk every scope's body.
struct SourceMentions {
    async_library: bool,
    availability: bool,
    platform: bool,
}

impl SourceMentions {
    fn scan(source: &str) -> Self {
        // A non-ASCII identifier is NFKC-normalized, so its spelling in the
        // source need not match the name.
        let ascii = source.is_ascii();
        let mentions = |word: &str| {
            !ascii || memchr::memmem::find(source.as_bytes(), word.as_bytes()).is_some()
        };
        Self {
            async_library: mentions("current_async_library"),
            availability: mentions("find_spec") || mentions("_available"),
            // An aliased module (`import sys as s`) is checked once the
            // module scope's names are known. `sys.version_info` tests read
            // the same `sys` bindings (#997).
            platform: mentions("sys.platform")
                || mentions("os.name")
                || mentions("platform.system")
                || mentions("sys.version_info"),
        }
    }
}

/// Mutable parse state accumulated while visiting one module.
pub(super) struct ModuleVisitor<'a, 'ast> {
    path: &'a str,
    layout: &'a LayoutInfo,
    lines: &'a LineIndex,
    default_context: ImportContext,
    in_type_checking: bool,
    /// Inside a `try:` body or `with suppress(ImportError):`, which catch a
    /// failed import only while they run, so a nested function's body is
    /// outside them (#968).
    caught_depth: u32,
    /// Inside code that runs only once some package imported: a `try`'s
    /// `else:`, a branch a flag or availability check rules out, or the
    /// statements after `if not is_x_available(): raise ImportError`. A
    /// function defined there exists only then, so it stays inside.
    gated_depth: u32,
    /// Inside an `except ImportError:` handler: the roots its `try` body
    /// imports (#721).
    fallback_for: Vec<String>,
    platform_guard_depth: u32,
    /// Python 3 minors the enclosing `sys.version_info` tests let the code
    /// run on (#997).
    version_minors: Minors,
    /// Names the current scope binds to `sys` / `os` / `platform` (#957);
    /// empty when the source tests none of them.
    platform_modules: PlatformModules,
    /// Module-level names bound once to a `sys.version_info` test that the
    /// current scope does not rebind (#1272).
    version_names: VersionNames,
    /// Top-level modules an enclosing branch checked are already imported
    /// (`"x" in sys.modules`); imports of them are optional (#681).
    imported_guards: Vec<String>,
    /// Names the current scope binds to `sniffio.current_async_library()`
    /// (#731).
    async_library_names: HashSet<String>,
    mentions: SourceMentions,
    /// Inside `if __name__ == "__main__":` of a module other than
    /// `__main__.py`; its imports are dev context (#681).
    in_main_block: bool,
    /// Where the enclosing function first checks if a package is installed;
    /// imports after it are optional (#695).
    availability_checked_at: Option<TextSize>,
    /// Dotted callees of the calls made as statements so far in the blocks
    /// that enclose the walk (#1269), written out only for an import after
    /// them.
    called_before: Vec<&'ast Expr>,
    /// Dotted callees of the calls the enclosing branch tests need truthy
    /// (#1272), written out like `called_before`.
    tested_before: Vec<&'ast Expr>,
    function_depth: u32,
    module_level: bool,
    /// Module-level names that are truthy only when a `try` import succeeded:
    /// set to `True` in the body (`has_x = True`, #730). Inside a function or
    /// class, the ones its scope does not rebind (#1272).
    try_flags: HashSet<String>,
    /// Module-level names set to `None` in an `except ImportError:` handler
    /// (`x = None`), which `x is None` also reads; scoped as `try_flags`.
    none_flags: HashSet<String>,
    typing_aliases: HashSet<String>,
    type_checking_names: HashSet<String>,
    loader_names: LoaderNames,
    server_names: ServerNames,
    /// Names the current scope binds to a module a prefixed loader call
    /// returned (`module = import_module("pkg." + name)`), with that call's
    /// index in `dynamic_import_prefixes` (#728).
    prefix_modules: FxHashMap<String, usize>,
    /// Names the current scope binds to the module a literal
    /// `import_module("pkg.m")` returned, with that module's name (#1144).
    literal_modules: HashMap<String, String>,
    command_words: BTreeSet<String>,
    sys_path: SysPathHints,
    /// Each distinct dotted-name literal at its first line (#1026).
    module_refs: BTreeMap<String, u32>,
    loaded_names: FxHashSet<String>,
    /// Every `x.attr` the walk meets; [`Self::into_parsed`] keeps
    /// the ones read through an import binding, which only the whole
    /// module's imports tell.
    attribute_sites: Vec<&'ast ExprAttribute>,
    /// The receiver, name and line of every `getattr(x, "attr")` and kin,
    /// read like `x.attr` (#1143).
    literal_getattr_sites: Vec<(&'ast Expr, &'ast str, u32)>,
    /// The `x.attr` sites inside string annotations, which are parsed into
    /// trees that do not outlive the walk: receiver, name and the offset of
    /// the outermost string (#1144).
    string_attribute_sites: Vec<StringAttributeSite>,
    /// The enclosing functions, innermost last: name and parameters (#1274).
    function_frames: Vec<(&'ast str, &'ast Parameters)>,
    /// The module-level classes, which a call may hand to a function that
    /// enumerates their subclasses (#1274).
    module_classes: FxHashSet<&'ast str>,
    /// The enclosing classes, each with the number of function frames open
    /// when it began, so a method's `cls` names its class.
    class_frames: Vec<(&'ast str, usize)>,
    parsed: ParsedModule,
}

impl<'a, 'ast> ModuleVisitor<'a, 'ast> {
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
            caught_depth: 0,
            gated_depth: 0,
            fallback_for: Vec::new(),
            platform_guard_depth: 0,
            version_minors: ALL_MINORS,
            platform_modules: PlatformModules::default(),
            version_names: VersionNames::new(),
            imported_guards: Vec::new(),
            async_library_names: HashSet::new(),
            mentions: SourceMentions {
                async_library: true,
                availability: true,
                platform: true,
            },
            in_main_block: false,
            availability_checked_at: None,
            called_before: Vec::new(),
            tested_before: Vec::new(),
            function_depth: 0,
            module_level: true,
            try_flags: HashSet::new(),
            none_flags: HashSet::new(),
            typing_aliases: HashSet::from(["typing".to_owned()]),
            type_checking_names: HashSet::from(["TYPE_CHECKING".to_owned()]),
            loader_names: LoaderNames::default(),
            server_names: ServerNames::default(),
            prefix_modules: FxHashMap::default(),
            literal_modules: HashMap::new(),
            command_words: BTreeSet::new(),
            sys_path: SysPathHints::default(),
            module_refs: BTreeMap::new(),
            loaded_names: FxHashSet::default(),
            attribute_sites: Vec::new(),
            literal_getattr_sites: Vec::new(),
            string_attribute_sites: Vec::new(),
            function_frames: Vec::new(),
            module_classes: FxHashSet::default(),
            class_frames: Vec::new(),
            parsed: ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            },
        }
    }

    /// Consume the visitor and return the accumulated parse result.
    #[must_use]
    pub(super) fn into_parsed(mut self) -> ParsedModule {
        self.parsed.attribute_accesses = self.import_attribute_accesses();
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
        self.parsed.module_refs = self
            .module_refs
            .into_iter()
            .map(|(text, line)| ModuleRef { text, line })
            .collect();
        let used: BTreeSet<String> = self
            .parsed
            .imports
            .iter()
            .filter_map(ImportRef::bound_name)
            .filter(|binding| self.loaded_names.contains(*binding))
            .map(str::to_owned)
            .collect();
        self.parsed.used_import_bindings = used.into_iter().collect();
        for symbol in &mut self.parsed.symbols {
            symbol.used_in_module = self.loaded_names.contains(&symbol.name);
        }
        let facts = &mut self.parsed.subclass_facts;
        facts.bases.sort_unstable();
        facts.bases.dedup_by(|later, kept| later.0 == kept.0);
        facts.enumerators.sort_unstable();
        facts.enumerators.dedup();
        facts.forwards.sort_unstable();
        facts.forwards.dedup();
        facts.passed.sort_unstable();
        facts
            .passed
            .dedup_by(|later, kept| (&later.0, &later.1) == (&kept.0, &kept.1));
        self.parsed.path_refs.sort_unstable();
        self.parsed.path_refs.dedup();
        self.parsed
    }

    /// The attribute sites read through a receiver symbol tracking follows:
    /// an `import a.b [as c]`'s `a.b` and binding, or a `from` import's
    /// binding. The rest are most accesses (`self.x`) and never read.
    fn import_attribute_accesses(&self) -> Vec<AttributeAccess> {
        let receivers: FxHashSet<&str> = self
            .parsed
            .imports
            .iter()
            .flat_map(|import| match import.kind {
                ImportKind::Import => [
                    Some(import.module.as_str()),
                    Some(import.alias.as_deref().unwrap_or(&import.module)),
                ],
                ImportKind::ImportFrom => [import.bound_name(), None],
            })
            .flatten()
            .collect();
        let attribute_sites = self.attribute_sites.iter().map(|attribute| {
            (
                &*attribute.value,
                attribute.attr.as_str(),
                self.line_number(*attribute),
            )
        });
        let mut receiver = String::new();
        let strings = self
            .string_attribute_sites
            .iter()
            .filter(|site| receivers.contains(site.receiver.as_str()))
            .map(|site| AttributeAccess {
                receiver: site.receiver.clone(),
                name: site.name.clone(),
                line: self.lines.line(site.at),
            });
        attribute_sites
            .chain(self.literal_getattr_sites.iter().copied())
            .filter_map(|(value, name, line)| {
                receiver.clear();
                (write_receiver(value, &mut receiver) && receivers.contains(receiver.as_str()))
                    .then(|| AttributeAccess {
                        receiver: receiver.clone(),
                        name: name.to_owned(),
                        line,
                    })
            })
            .chain(strings)
            .collect()
    }

    /// Visit module-level statements.
    ///
    /// `__all__` is read first because [`Self::record_symbol`] consults the export
    /// list to decide whether an underscore-prefixed symbol is public.
    pub(super) fn visit_module(&mut self, source: &str, stmts: &'ast [Stmt]) {
        self.mentions = SourceMentions::scan(source);
        self.parsed.exports = extract_exports(stmts, self.lines, &mut self.parsed.diagnostics);
        self.parsed.registry_decorators = registry_decorators(stmts);
        self.module_classes = stmts
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::ClassDef(class) => Some(class.name.as_str()),
                _ => None,
            })
            .collect();
        self.parsed.import_checks = import_checks(stmts);
        self.async_library_names = self.scope_async_library_names(None, stmts);
        // Without a plain `sys.platform`-style mention, only an aliased
        // import (`import sys as s`) can make a platform test, so most
        // modules skip the module scope's binding scan (#1087).
        if self.mentions.platform || imports_platform_alias(stmts) {
            let modules = platform_modules(None, None, stmts);
            if self.mentions.platform || mentions_platform_test(source, &modules) {
                self.platform_modules = modules;
                self.mentions.platform = true;
            }
        }
        if self.mentions.platform {
            self.version_names = version_guard::version_names(stmts, &self.platform_modules);
        }
        self.visit_body(stmts);
        self.parsed.deprecated_shim = warns_deprecated_on_import(stmts, &self.parsed.imports);
    }

    fn scope_async_library_names(
        &self,
        parameters: Option<&Parameters>,
        body: &[Stmt],
    ) -> HashSet<String> {
        if self.mentions.async_library {
            async_library_names(parameters, body)
        } else {
            HashSet::new()
        }
    }

    fn scope_platform_modules(
        &self,
        parameters: Option<&Parameters>,
        body: &[Stmt],
    ) -> PlatformModules {
        if self.mentions.platform {
            platform_modules(Some(&self.platform_modules), parameters, body)
        } else {
            PlatformModules::default()
        }
    }

    /// Visit one `if`/`elif`/`else` branch; `test` is `None` for `else`, and
    /// `earlier` holds the tests of the chain's branches before it.
    fn visit_branch(
        &mut self,
        earlier: &[&'ast Expr],
        test: Option<&'ast Expr>,
        body: &'ast [Stmt],
    ) {
        let was_type_checking = self.in_type_checking;
        let was_platform_guard = self.platform_guard_depth;
        let was_version_minors = self.version_minors;
        let was_gated_depth = self.gated_depth;
        let was_main_block = self.in_main_block;
        let guard_count = self.imported_guards.len();
        let excluded = |leaf: &dyn Fn(&Expr) -> Leaf| condition::excluded(earlier, test, leaf);
        let type_checking = excluded(&|expr| self.type_checking_leaf(expr));
        let platform_guard = excluded(&|expr| {
            if is_platform_guard_test(expr, &self.platform_modules) {
                Leaf::Free
            } else {
                Leaf::Opaque
            }
        });
        // `python -m pkg` runs `pkg/__main__.py` as `__main__`, and Jupyter
        // runs every cell as `__main__`, so there the block is the normal
        // path.
        let main_block = !runs_as_main(self.path)
            && excluded(&|expr| {
                if is_main_guard_test(expr) {
                    Leaf::Fixed(false)
                } else {
                    self.type_checking_leaf(expr)
                }
            });
        // Only the guard itself marks a script: `TYPE_CHECKING` also rules a
        // branch out, but says nothing about how the file is run.
        let main_guard = main_block
            && excluded(&|expr| {
                if is_main_guard_test(expr) {
                    Leaf::Fixed(false)
                } else {
                    Leaf::Opaque
                }
            });
        let optional = excluded(&|expr| self.installed_leaf(expr));
        let version_minors = version_guard::branch_minors(
            earlier,
            test,
            &self.platform_modules,
            &self.version_names,
        );
        let mut guards: Vec<String> = condition::leaves(earlier.iter().copied().chain(test))
            .into_iter()
            .filter_map(|expr| imported_module_guard(expr, &self.async_library_names))
            .collect();
        guards.sort();
        guards.dedup();
        guards.retain(|guard| {
            excluded(&|expr| {
                if imported_module_guard(expr, &self.async_library_names).as_ref() == Some(guard) {
                    Leaf::Fixed(false)
                } else {
                    self.installed_leaf(expr)
                }
            })
        });
        // The test runs only when the chain reaches it, and `and` stops at a
        // missing package, so an import in it shares the branch's optionality.
        // An import placed before the check in the same `and` is read as
        // optional too: real code checks first.
        if optional {
            self.gated_depth = self.gated_depth.saturating_add(1);
        }
        if let Some(test) = test {
            self.visit_expr(test);
        }
        self.in_type_checking |= type_checking;
        if platform_guard {
            self.platform_guard_depth = self.platform_guard_depth.saturating_add(1);
        }
        self.in_main_block |= main_block;
        self.version_minors &= version_minors;
        // A guard inside a function or class body says nothing about how the
        // file is run.
        if main_guard && self.module_level {
            self.parsed.has_main_block = true;
        }
        self.imported_guards.extend(guards);
        let tested_before = self.tested_before.len();
        self.tested_before.extend(needed_calls(earlier, test));
        self.visit_body(body);
        self.tested_before.truncate(tested_before);
        self.in_type_checking = was_type_checking;
        self.platform_guard_depth = was_platform_guard;
        self.version_minors = was_version_minors;
        self.gated_depth = was_gated_depth;
        self.in_main_block = was_main_block;
        self.imported_guards.truncate(guard_count);
    }

    /// `TYPE_CHECKING` is false whenever the code runs.
    fn type_checking_leaf(&self, expr: &Expr) -> Leaf {
        if is_type_checking_test(expr, &self.typing_aliases, &self.type_checking_names) {
            Leaf::Fixed(false)
        } else {
            Leaf::Opaque
        }
    }

    /// What `expr` is when some package is missing: `find_spec("x")` and
    /// `is_x_available()` are false (#695), `find_spec("x") is None` true,
    /// `"x" in sys.modules` false and `"x" not in sys.modules` true (#1272),
    /// and so is a module-level flag a failed `try` import leaves falsy.
    fn installed_leaf(&self, expr: &Expr) -> Leaf {
        if let Some(installed) = availability_test(expr).or_else(|| loaded_test(expr)) {
            Leaf::Fixed(!installed)
        } else if let Some(value) = flag_test(expr, &self.try_flags, &self.none_flags) {
            Leaf::Fixed(value)
        } else {
            Leaf::Opaque
        }
    }

    /// Enter a function or class scope: drop the module-level flags and
    /// version names it rebinds, which may hold anything there (#1272).
    /// Returns the outer ones for [`Self::leave_scope_names`].
    fn enter_scope_names(
        &mut self,
        parameters: Option<&Parameters>,
        body: &[Stmt],
    ) -> (HashSet<String>, HashSet<String>, VersionNames) {
        let outer = (
            self.try_flags.clone(),
            self.none_flags.clone(),
            self.version_names.clone(),
        );
        if !(self.try_flags.is_empty()
            && self.none_flags.is_empty()
            && self.version_names.is_empty())
        {
            let bound = bound_names(parameters, body);
            self.try_flags
                .retain(|name| !bound.contains_key(name.as_str()));
            self.none_flags
                .retain(|name| !bound.contains_key(name.as_str()));
            self.version_names
                .retain(|name, _| !bound.contains_key(name.as_str()));
        }
        outer
    }

    fn leave_scope_names(&mut self, outer: (HashSet<String>, HashSet<String>, VersionNames)) {
        (self.try_flags, self.none_flags, self.version_names) = outer;
    }

    /// Whether an import at `node` runs only when some package is installed.
    fn is_optional_at<R: Ranged>(&self, node: &R) -> bool {
        self.caught_depth > 0
            || self.gated_depth > 0
            || self
                .availability_checked_at
                .is_some_and(|at| at < node.start())
    }

    fn is_imported_guarded(&self, module: &str) -> bool {
        let root = module.split('.').next().unwrap_or(module);
        self.imported_guards
            .iter()
            .any(|guard| guarded_roots(guard).contains(&root))
    }

    fn visit_decorators(&mut self, decorators: &'ast [Decorator]) {
        self.record_decorators(decorators);
        for decorator in decorators {
            self.visit_expr(&decorator.expression);
        }
    }

    /// Record a def/class symbol and walk its body; the caller has already
    /// walked the decorators and signature.
    fn visit_def_body(
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
        let saved_literal_modules = std::mem::take(&mut self.literal_modules);
        let saved_caught_depth = self.caught_depth;
        self.module_level = false;
        if matches!(kind, SymbolKind::Function) {
            self.function_depth += 1;
            // The body runs when called, after the enclosing `try` is gone.
            // `fallback_for` stays: a handler that never runs defines nothing.
            self.caught_depth = 0;
            // A function that asks whether a package is installed treats the
            // packages it imports after the check as optional (#695, #713).
            // A nested function runs when called, possibly after the outer
            // check, so all of its imports are.
            self.availability_checked_at = if saved_checked_at.is_some() {
                Some(TextSize::default())
            } else {
                self.mentions
                    .availability
                    .then(|| first_availability_check(body))
                    .flatten()
            };
        }
        self.visit_body(body);
        self.module_level = saved;
        self.function_depth = saved_function_depth;
        self.availability_checked_at = saved_checked_at;
        self.prefix_modules = saved_prefix_modules;
        self.literal_modules = saved_literal_modules;
        self.caught_depth = saved_caught_depth;
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
        match self.loaded_module(value) {
            Some(module) => {
                self.literal_modules.insert(name.id.to_string(), module);
            },
            None => {
                self.literal_modules.remove(name.id.as_str());
            },
        }
    }

    /// The module `expr` evaluates to when it is a literal
    /// `importlib.import_module("pkg.m")`, or a name the current scope bound
    /// to one. `__import__("pkg.m")` returns `pkg`, so it is left out.
    fn loaded_module(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Call(call) => match self.loader_names.loader(&call.func)? {
                Loader::ImportModule => match literal_target(call, Loader::ImportModule, || {
                    module_package(self.path, self.layout)
                })? {
                    LiteralTarget::Module(module) => Some(module),
                    LiteralTarget::Opaque | LiteralTarget::Nothing => None,
                },
                Loader::DunderImport => None,
            },
            Expr::Name(name) => self.literal_modules.get(name.id.as_str()).cloned(),
            _ => None,
        }
    }

    /// The names in the arguments typing reads as type expressions. Kept out
    /// of [`Self::visit_expr`], whose frame every nesting level pays for.
    fn visit_type_expression_arguments(&mut self, call: &ExprCall) {
        for argument in type_expression_arguments(call, &self.typing_aliases) {
            AnnotationNames::new(&mut self.loaded_names, &mut self.string_attribute_sites)
                .visit_expr(argument);
        }
    }

    /// Keep the site for [`Self::import_attribute_accesses`]; and
    /// `import_module("pkg.m").f` reads `f` from `pkg.m` (#1144). Kept out of
    /// [`Self::visit_expr`], whose frame every nesting level pays for.
    fn record_attribute(&mut self, attribute: &'ast ExprAttribute) {
        self.attribute_sites.push(attribute);
        if let Some(module) = self.loaded_module(&attribute.value) {
            let line = self.line_number(attribute);
            self.parsed.loaded_module_accesses.push(AttributeAccess {
                receiver: module,
                name: attribute.attr.to_string(),
                line,
            });
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

    /// Keep `getattr(x, "attr")` and its kin for [`Self::into_parsed`],
    /// which reads them like `x.attr`, and like [`Self::record_attribute`]
    /// on a module `import_module("pkg.m")` returned. A method of its own so
    /// its locals stay out of the frame [`Self::visit_expr`] recurses with
    /// (#1045).
    fn record_literal_getattr(&mut self, call: &'ast ExprCall) {
        let Some((receiver, name)) = literal_getattr(call) else {
            return;
        };
        let line = self.line_number(call);
        self.literal_getattr_sites.push((receiver, name, line));
        if let Some(module) = self.loaded_module(receiver) {
            self.parsed.loaded_module_accesses.push(AttributeAccess {
                receiver: module,
                name: name.to_owned(),
                line,
            });
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
                    version_minors: None,
                    called_before: Vec::new(),
                    tested_before: Vec::new(),
                });
            },
            Some(PythonRun::File) => self.parsed.runs_python_file = true,
            None => {},
        }
    }

    fn record_dynamic_imports(&mut self, call: &ExprCall) {
        if let Some((callee, name, lazy_command)) = named_load(call) {
            let load = DynamicImport {
                fetched: true,
                ..self.dynamic_import(name, call)
            };
            self.parsed.named_loads.push(NamedLoad {
                call: callee.to_owned(),
                lazy_command,
                load,
            });
        }
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
    }

    fn dynamic_import(&self, module: String, call: &ExprCall) -> DynamicImport {
        DynamicImport {
            module,
            line: self.line_number(call),
            optional: self.is_optional_at(call),
            platform_guarded: self.platform_guard_depth > 0,
            deferred: self.function_depth > 0,
            computed_getattr: false,
            fetched: false,
        }
    }

    /// `x += 1` reads `x` before it writes it, though a name target is in
    /// store context (#1142). Kept out of `visit_stmt` so its frame, which
    /// every nesting level pays for, does not grow.
    fn visit_aug_assign_target(&mut self, target: &'ast Expr) {
        if let Expr::Name(name) = target {
            insert_name(&mut self.loaded_names, &name.id);
        }
        self.visit_expr(target);
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
            self.server_names.record_import(alias);
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
                version_minors: self.version_minor_runs(),
                called_before: Vec::new(),
                tested_before: Vec::new(),
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
                self.server_names
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
                let imported_name = module_suffix.is_none().then(|| alias.name.as_str());
                let resolved = resolve_relative_import(
                    self.path,
                    self.layout,
                    level,
                    module_suffix.as_deref(),
                    imported_name,
                );
                let module = resolved.unwrap_or_else(|| {
                    self.parsed.diagnostics.push(unresolved_relative_diagnostic(
                        line,
                        self.path,
                        self.layout,
                    ));
                    String::new()
                });
                let name = (module_suffix.is_some() || is_star).then(|| alias.name.to_string());
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
                version_minors: self.version_minor_runs(),
                called_before: Vec::new(),
                tested_before: Vec::new(),
            });
        }
    }

    fn version_minor_runs(&self) -> Option<Vec<[u32; 2]>> {
        (self.version_minors != ALL_MINORS).then(|| version_guard::minor_runs(self.version_minors))
    }

    /// Record `import` with the calls it runs after, which may yet make it
    /// optional; an import that already is keeps none.
    fn push_import(&mut self, mut import: ImportRef) {
        if !import.optional {
            for (funcs, callees) in [
                (&self.called_before, &mut import.called_before),
                (&self.tested_before, &mut import.tested_before),
            ] {
                for func in funcs {
                    let mut callee = String::new();
                    write_receiver(func, &mut callee);
                    if !callees.contains(&callee) {
                        callees.push(callee);
                    }
                }
            }
        }
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
        let is_public = !name.starts_with('_') || self.parsed.exports.contains(&name);
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

    /// The first read of the module's own namespace by a computed name.
    fn record_self_reference<R: Ranged>(&mut self, node: &R) {
        if self.parsed.self_reference.is_none() {
            self.parsed.self_reference = Some(self.line_number(node));
        }
    }

    /// A module-level class's bases, and whether it registers its
    /// subclasses with `__init_subclass__` (#1274).
    fn record_class_facts(&mut self, class: &'ast StmtClassDef) {
        if !self.module_level {
            return;
        }
        self.module_classes.insert(class.name.as_str());
        let name = class.name.to_string();
        // `Plugin[Row]` subclasses `Plugin`.
        let bases: Vec<String> = class
            .bases()
            .iter()
            .map(|base| match base {
                Expr::Subscript(subscript) => &*subscript.value,
                other => other,
            })
            .filter_map(|base| callee_name(base).map(str::to_owned))
            .collect();
        let facts = &mut self.parsed.subclass_facts;
        if !bases.is_empty() {
            facts.classes.push((name.clone(), bases));
        }
        let registers = class.body.iter().any(
            |stmt| matches!(stmt, Stmt::FunctionDef(def) if def.name.as_str() == "__init_subclass__"),
        );
        if registers {
            let line = self.line_number(&class.name);
            self.parsed.subclass_facts.bases.push((name, line));
        }
    }

    /// The class a `__subclasses__()` receiver names: the enclosing class for
    /// the first parameter of one of its methods (`cls`, `self`), the name
    /// itself otherwise.
    fn receiver_class(&self, receiver: &str) -> String {
        let method_of = self.class_frames.last().filter(|(_, depth)| {
            *depth + 1 == self.function_frames.len()
                && self.function_frames.last().is_some_and(|(_, parameters)| {
                    positional_names(parameters).next() == Some(receiver)
                })
        });
        method_of.map_or(receiver, |(class, _)| class).to_owned()
    }

    /// What a call tells about names and files used without an import
    /// (#1274): `__subclasses__()` and what is handed to the functions that
    /// call it, and a `.py` path given to a file call.
    fn record_by_name(&mut self, call: &'ast ExprCall) {
        self.parsed.path_refs.extend(path_refs(call));
        let Some(callee) = callee_name(&call.func) else {
            return;
        };
        if callee == "__subclasses__"
            && let Expr::Attribute(attribute) = &*call.func
        {
            if let Some((function, _)) = self.function_frames.last() {
                let function = (*function).to_owned();
                self.parsed.subclass_facts.enumerators.push(function);
            }
            if let Expr::Name(base) = &*attribute.value {
                let base = self.receiver_class(base.id.as_str());
                let line = self.line_number(call);
                self.parsed.subclass_facts.bases.push((base, line));
            }
            return;
        }
        let facts = &mut self.parsed.subclass_facts;
        for arg in &call.arguments.args {
            let Expr::Name(name) = arg else {
                continue;
            };
            let name = name.id.as_str();
            if self.module_classes.contains(name) {
                let line = self.lines.line(call.start());
                facts
                    .passed
                    .push((callee.to_owned(), name.to_owned(), line));
            }
            // A method of a parameter (`self.layer(x)`) is no module-level
            // function, so it is not followed; skipping it keeps the facts of
            // a model zoo small.
            if let Some((function, parameters)) = self.function_frames.last()
                && positional_names(parameters).any(|parameter| parameter == name)
                && !receiver_root(&call.func).is_some_and(|root| {
                    positional_names(parameters).any(|parameter| parameter == root)
                })
            {
                facts
                    .forwards
                    .push(((*function).to_owned(), callee.to_owned()));
            }
        }
    }

    /// Kept out of `visit_stmt` so its frame, paid once per nesting level,
    /// does not grow (#1173).
    fn visit_match(&mut self, match_stmt: &'ast StmtMatch) {
        self.visit_expr(&match_stmt.subject);
        for case in &match_stmt.cases {
            // `case m.CONST:` and `case Point(x=0):` read the names in their
            // value and class patterns (#1141).
            self.visit_pattern(&case.pattern);
            if let Some(guard) = &case.guard {
                self.visit_expr(guard);
            }
            self.visit_body(&case.body);
        }
    }
}

impl<'ast> Visitor<'ast> for ModuleVisitor<'_, 'ast> {
    /// Statements after `if not is_x_available(): raise ImportError(...)`
    /// or `if "x" not in sys.modules: return` only run with `x` installed
    /// (#695, #1272).
    fn visit_body(&mut self, body: &'ast [Stmt]) {
        let gated_depth = self.gated_depth;
        let called_before = self.called_before.len();
        let tested_before = self.tested_before.len();
        for stmt in body {
            self.visit_stmt(stmt);
            // The statements after a call run only once it returned, which a
            // first-party install check does only with its package (#1269).
            if let Some(func) = statement_callee(stmt) {
                self.called_before.push(func);
            }
            // The statements after an `if` whose body returns or raises run
            // only when its test was false, which it cannot be with the
            // package missing (#695, #958, #1272).
            let exit_test = match stmt {
                Stmt::If(if_stmt) if exits_block(&if_stmt.body) => Some(&*if_stmt.test),
                _ => None,
            };
            let exit_guard = exit_test.is_some_and(|test| {
                condition::excluded(&[test], None, &|expr| self.installed_leaf(expr))
            });
            if exit_guard || is_install_check(stmt) {
                self.gated_depth = gated_depth.saturating_add(1);
            }
            // `if not check(): return` leaves the rest to run only when the
            // call was truthy, as in the body of `if check():` (#1272).
            if let Some(test) = exit_test {
                self.tested_before.extend(needed_calls(&[test], None));
            }
        }
        self.gated_depth = gated_depth;
        self.called_before.truncate(called_before);
        self.tested_before.truncate(tested_before);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one short arm per statement kind; splitting the match would scatter them"
    )]
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
                let names = self.scope_async_library_names(Some(&def.parameters), &def.body);
                let outer_names = std::mem::replace(&mut self.async_library_names, names);
                let modules = self.scope_platform_modules(Some(&def.parameters), &def.body);
                let outer_modules = std::mem::replace(&mut self.platform_modules, modules);
                let outer_flags = self.enter_scope_names(Some(&def.parameters), &def.body);
                self.function_frames
                    .push((def.name.as_str(), &def.parameters));
                self.visit_def_body(
                    &def.name,
                    &def.decorator_list,
                    &def.body,
                    SymbolKind::Function,
                );
                self.function_frames.pop();
                self.leave_scope_names(outer_flags);
                self.async_library_names = outer_names;
                self.platform_modules = outer_modules;
            },
            Stmt::ClassDef(def) => {
                self.visit_decorators(&def.decorator_list);
                if let Some(type_params) = &def.type_params {
                    self.visit_type_params(type_params);
                }
                if let Some(arguments) = &def.arguments {
                    self.visit_arguments(arguments);
                }
                let names = self.scope_async_library_names(None, &def.body);
                let outer_names = std::mem::replace(&mut self.async_library_names, names);
                let modules = self.scope_platform_modules(None, &def.body);
                let outer_modules = std::mem::replace(&mut self.platform_modules, modules);
                self.record_class_facts(def);
                let outer_flags = self.enter_scope_names(None, &def.body);
                self.class_frames
                    .push((def.name.as_str(), self.function_frames.len()));
                self.visit_def_body(&def.name, &def.decorator_list, &def.body, SymbolKind::Class);
                self.class_frames.pop();
                self.leave_scope_names(outer_flags);
                self.async_library_names = outer_names;
                self.platform_modules = outer_modules;
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
                    // `has_x = is_x_available()` is falsy without `x`, as a
                    // `try` flag is, and `spec = find_spec("x")` is `None`
                    // as an `except` flag is (#1242).
                    if let [Expr::Name(name)] = assign.targets.as_slice() {
                        if is_find_spec_value(&assign.value) {
                            self.none_flags.insert(name.id.to_string());
                        } else if condition::excluded(&[], Some(&assign.value), &|expr| {
                            self.installed_leaf(expr)
                        }) {
                            self.try_flags.insert(name.id.to_string());
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
                    if subscript_name(&ann_assign.annotation) == Some("TypeAlias") {
                        AnnotationNames::new(
                            &mut self.loaded_names,
                            &mut self.string_attribute_sites,
                        )
                        .visit_expr(value);
                    }
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
                self.visit_annotation(&alias.value);
            },
            Stmt::AugAssign(aug_assign) => {
                if self.module_level
                    && matches!(aug_assign.op, Operator::Add)
                    && let Expr::Name(name) = &*aug_assign.target
                {
                    self.record_pytest_plugins(name.id.as_str(), &aug_assign.value);
                }
                self.visit_aug_assign_target(&aug_assign.target);
                self.visit_expr(&aug_assign.value);
            },
            Stmt::Return(return_stmt) => {
                if let Some(value) = &return_stmt.value {
                    self.visit_expr(value);
                }
            },
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::If(if_stmt) => {
                let mut earlier = Vec::new();
                for (test, body) in if_branches(if_stmt) {
                    self.visit_branch(&earlier, test, body);
                    earlier.extend(test);
                }
            },
            Stmt::Try(try_stmt) => {
                if self.module_level {
                    self.try_flags.extend(assigned_flags(
                        &try_stmt.body,
                        |value| matches!(value, Expr::BooleanLiteral(literal) if literal.value),
                    ));
                }
                self.caught_depth = self.caught_depth.saturating_add(1);
                self.visit_body(&try_stmt.body);
                self.caught_depth = self.caught_depth.saturating_sub(1);
                let primaries = imported_roots(&try_stmt.body);
                let imports = try_stmt
                    .body
                    .iter()
                    .any(|stmt| matches!(stmt, Stmt::Import(_) | Stmt::ImportFrom(_)));
                for handler in &try_stmt.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(exc_type) = &handler.type_ {
                        self.visit_expr(exc_type);
                    }
                    // A bare `except:` catches everything, a failed import included.
                    let import_failed = handler.type_.as_deref().is_none_or(catches_import_error);
                    // A broad handler also catches whatever else the body
                    // raises, so `x = None` there marks a missing package
                    // only when the body imports one.
                    let flags = import_failed
                        && (imports || handler.type_.as_deref().is_some_and(names_import_error));
                    if self.module_level && flags {
                        self.none_flags
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
                self.gated_depth = self.gated_depth.saturating_add(1);
                self.visit_body(&try_stmt.orelse);
                self.gated_depth = self.gated_depth.saturating_sub(1);
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
                    self.caught_depth = self.caught_depth.saturating_add(1);
                }
                self.visit_body(&with_stmt.body);
                if suppresses {
                    self.caught_depth = self.caught_depth.saturating_sub(1);
                }
            },
            Stmt::Match(match_stmt) => self.visit_match(match_stmt),
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
        AnnotationNames::new(&mut self.loaded_names, &mut self.string_attribute_sites)
            .visit_expr(expr);
        walk_annotation(self, expr);
    }

    /// Walk one expression for dynamic imports and module attribute accesses.
    ///
    /// Both used to be separate full-tree passes; folding them into the statement
    /// walk keeps cold parse to a single traversal per file.
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Some(word) = command_word(expr) {
            self.command_words.insert(word.to_owned());
        }
        match expr {
            Expr::Name(name) if name.ctx.is_load() => insert_name(&mut self.loaded_names, &name.id),
            Expr::Attribute(attribute) => self.record_attribute(attribute),
            Expr::Subscript(subscript) if reads_own_namespace(subscript) => {
                self.record_self_reference(subscript);
            },
            Expr::Call(call) => {
                self.visit_type_expression_arguments(call);
                self.record_computed_getattr(call);
                self.record_literal_getattr(call);
                self.record_by_name(call);
                if gets_own_name(call) {
                    self.record_self_reference(call);
                }
                if edits_sys_path(&call.func) {
                    self.sys_path.edited = true;
                    self.parsed.adds_computed_sys_path |= adds_computed_path(call);
                }
                self.parsed.lists_directory |= lists_directory(&call.func);
                if let Some((label, app, app_dir)) = self.server_names.app(call) {
                    let line = self.line_number(call);
                    self.parsed.server_apps.push(ServerApp {
                        call: label.to_owned(),
                        app: app.to_owned(),
                        line,
                        app_dir,
                    });
                }
                self.record_dynamic_imports(call);
            },
            Expr::List(list) => self.record_python_run(expr, &list.elts),
            Expr::Tuple(tuple) => self.record_python_run(expr, &tuple.elts),
            Expr::StringLiteral(_) => {
                if let Some(hint) = sys_path_hint(expr)
                    && !self.sys_path.literals.contains(hint.as_ref())
                {
                    self.sys_path.literals.insert(hint.into_owned());
                }
                if let Some(text) = module_ref_literal(expr)
                    && !self.module_refs.contains_key(text)
                {
                    let line = self.line_number(expr);
                    self.module_refs.insert(text.to_owned(), line);
                }
            },
            _ => {},
        }
        let Expr::Lambda(lambda) = expr else {
            walk_expr(self, expr);
            return;
        };
        // The defaults are evaluated where the lambda is defined.
        if let Some(parameters) = &lambda.parameters {
            self.visit_parameters(parameters);
        }
        let caught_depth = std::mem::take(&mut self.caught_depth);
        self.function_depth += 1;
        self.visit_expr(&lambda.body);
        self.function_depth -= 1;
        self.caught_depth = caught_depth;
    }
}

/// The receiver and name of `getattr(x, "name")`, `hasattr`, `setattr` or
/// `delattr`: the same reference as `x.name`. A computed name, or a call
/// with too few arguments, is `None` and names nothing.
fn literal_getattr(call: &ExprCall) -> Option<(&Expr, &str)> {
    let Expr::Name(func) = &*call.func else {
        return None;
    };
    if !matches!(
        func.id.as_str(),
        "getattr" | "hasattr" | "setattr" | "delattr"
    ) {
        return None;
    }
    let [receiver, Expr::StringLiteral(name), ..] = &*call.arguments.args else {
        return None;
    };
    Some((receiver, name.value.to_str()))
}

/// The name a method call's receiver starts from: `self` for
/// `self.layer.forward(x)`.
fn receiver_root(func: &Expr) -> Option<&str> {
    let Expr::Attribute(attribute) = func else {
        return None;
    };
    let mut receiver = &*attribute.value;
    loop {
        match receiver {
            Expr::Attribute(inner) => receiver = &inner.value,
            Expr::Name(name) => return Some(name.id.as_str()),
            _ => return None,
        }
    }
}

/// The names a call may bind positionally.
fn positional_names(parameters: &Parameters) -> impl Iterator<Item = &str> {
    parameters
        .posonlyargs
        .iter()
        .chain(&parameters.args)
        .map(|parameter| parameter.parameter.name.as_str())
        .chain(
            parameters
                .vararg
                .as_deref()
                .map(|vararg| vararg.name.as_str()),
        )
}

/// Adds `name`, allocating only the first time: a module reads most names
/// many times over.
fn insert_name(names: &mut FxHashSet<String>, name: &str) {
    if !names.contains(name) {
        names.insert(name.to_owned());
    }
}

/// An `x.attr` inside a string annotation (`"m.Foo"`).
struct StringAttributeSite {
    receiver: String,
    name: String,
    /// Start of the outermost string literal in the module source.
    at: TextSize,
}

/// Every name is collected, the ones outside strings included, because
/// a parsed string annotation is a fresh tree the module walk never reaches.
/// For the same reason the attribute sites inside a string are collected
/// here; the ones outside are the module walk's.
struct AnnotationNames<'n> {
    names: &'n mut FxHashSet<String>,
    attributes: &'n mut Vec<StringAttributeSite>,
    /// The outermost string literal being walked, if any.
    string_at: Option<TextSize>,
}

impl<'n> AnnotationNames<'n> {
    fn new(names: &'n mut FxHashSet<String>, attributes: &'n mut Vec<StringAttributeSite>) -> Self {
        Self {
            names,
            attributes,
            string_at: None,
        }
    }

    /// An `x.attr` inside a string; the module walk keeps the ones outside.
    /// This and [`Self::visit_string`] stay out of `visit_expr`, whose frame
    /// every nesting level pays for.
    fn record_attribute(&mut self, attribute: &ExprAttribute) {
        if let Some(at) = self.string_at {
            let mut receiver = String::new();
            if write_receiver(&attribute.value, &mut receiver) {
                self.attributes.push(StringAttributeSite {
                    receiver,
                    name: attribute.attr.to_string(),
                    at,
                });
            }
        }
    }

    fn visit_string(&mut self, string: &ExprStringLiteral) {
        // One nested past the walk limit (#1045) reads like one
        // that does not parse: its names are not collected.
        if let Some(parsed) = super::depth::parse_walkable_expression(string.value.to_str()) {
            let outer = self.string_at;
            self.string_at = Some(outer.unwrap_or_else(|| string.start()));
            self.visit_expr(&parsed);
            self.string_at = outer;
        }
    }
}

impl<'a> Visitor<'a> for AnnotationNames<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => insert_name(self.names, &name.id),
            Expr::Attribute(attribute) => {
                self.record_attribute(attribute);
                walk_expr(self, expr);
            },
            Expr::StringLiteral(string) => self.visit_string(string),
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

/// Arguments typing reads as type expressions, where a string is a forward
/// reference (#577): the type of `cast("T", x)` and the constraints, bound
/// and default of `TypeVar("T", bound="Foo")`. The first `TypeVar` argument
/// is the variable's own name.
fn type_expression_arguments<'e>(
    call: &'e ExprCall,
    typing_aliases: &HashSet<String>,
) -> impl Iterator<Item = &'e Expr> {
    // Unlike `Literal[...]`, a method may share the name: `col.cast("float")`.
    let callee = match &*call.func {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => match &*attribute.value {
            Expr::Name(module)
                if typing_aliases.contains(module.id.as_str())
                    || module.id.as_str() == "typing_extensions" =>
            {
                Some(attribute.attr.as_str())
            },
            _ => None,
        },
        _ => None,
    };
    let arguments = &call.arguments;
    let (positional, keywords): (&[Expr], &[&str]) = match callee {
        Some("cast") => (arguments.args.get(..1).unwrap_or_default(), &["typ"]),
        Some("TypeVar") => (
            arguments.args.get(1..).unwrap_or_default(),
            &["bound", "default"],
        ),
        _ => (&[], &[]),
    };
    positional.iter().chain(
        keywords
            .iter()
            .filter_map(|name| arguments.find_keyword(name))
            .map(|keyword| &keyword.value),
    )
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

/// What `test` is when the flags are unset: `x` is false, and for an
/// `x = None` flag `x is not None` is false and `x is None` true. `None` for
/// anything else, which includes `is` tests of a `has_x = True` flag: it is
/// `False` there, not `None`.
fn flag_test(test: &Expr, flags: &HashSet<String>, none_flags: &HashSet<String>) -> Option<bool> {
    let named = |expr: &Expr, names: &HashSet<String>| matches!(expr, Expr::Name(name) if names.contains(name.id.as_str()));
    match test {
        Expr::Compare(compare) => match (&*compare.ops, &*compare.operands) {
            ([op @ (CmpOp::Is | CmpOp::IsNot)], [flag, Expr::NoneLiteral(_)])
                if named(flag, none_flags) =>
            {
                Some(*op == CmpOp::Is)
            },
            _ => None,
        },
        _ => (named(test, flags) || named(test, none_flags)).then_some(false),
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

/// The top-level functions whose body, unconditionally, tries to import
/// packages and raises from a handler a failed import lands in: a call to
/// one returns only when they are installed (#1269). The shape decides, not
/// the name. A handler that returns `None` / `False` instead lets the call
/// return either way, so only a caller that tests the result knows the
/// packages are there (#1272); a function with both kinds of `try` raises.
fn import_checks(stmts: &[Stmt]) -> Vec<ImportCheck> {
    stmts
        .iter()
        .filter_map(|stmt| {
            let Stmt::FunctionDef(def) = stmt else {
                return None;
            };
            let tries: Vec<(&StmtTry, ImportFailure)> = def
                .body
                .iter()
                .filter_map(|stmt| match stmt {
                    Stmt::Try(try_stmt) => Some((try_stmt, import_failure(try_stmt)?)),
                    _ => None,
                })
                .collect();
            let returns_falsy = tries
                .iter()
                .all(|(_, failure)| *failure == ImportFailure::ReturnsFalsy);
            let mut modules: Vec<String> = tries
                .iter()
                .flat_map(|(try_stmt, _)| &try_stmt.body)
                .flat_map(|stmt| match stmt {
                    Stmt::Import(import) => import
                        .names
                        .iter()
                        .map(|alias| alias.name.to_string())
                        .collect(),
                    Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                        import_from.module.iter().map(ToString::to_string).collect()
                    },
                    _ => Vec::new(),
                })
                .collect();
            modules.sort();
            modules.dedup();
            (!modules.is_empty()).then(|| ImportCheck {
                function: def.name.to_string(),
                modules,
                returns_falsy,
            })
        })
        .collect()
}

/// How a function leaves when its `try` body's import fails.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportFailure {
    Raises,
    /// `return`, `return None` or `return False`.
    ReturnsFalsy,
}

/// How the first handler a missing package lands in ends, when it raises or
/// returns a falsy value: a later `except ImportError: raise` never sees the
/// `ModuleNotFoundError` an earlier handler logs and swallows.
fn import_failure(try_stmt: &StmtTry) -> Option<ImportFailure> {
    let handler = try_stmt
        .handlers
        .iter()
        .map(|handler| {
            let ExceptHandler::ExceptHandler(handler) = handler;
            handler
        })
        .find(|handler| handler.type_.as_deref().is_none_or(catches_import_error))?;
    match handler.body.last()? {
        Stmt::Raise(_) => Some(ImportFailure::Raises),
        Stmt::Return(ret) => match ret.value.as_deref() {
            None | Some(Expr::NoneLiteral(_)) => Some(ImportFailure::ReturnsFalsy),
            Some(Expr::BooleanLiteral(literal)) if !literal.value => {
                Some(ImportFailure::ReturnsFalsy)
            },
            Some(_) => None,
        },
        _ => None,
    }
}

/// The dotted callees of the calls in the tests of an `if` chain that must be
/// truthy for the branch tested by `test` (`None` for `else`) to run, given
/// the `earlier` tests: `check()` in `if check():`, `if check() and x:`, or
/// the `else` of `if not check():` (#1272). A method of `self` / `cls` is
/// left out, as for [`statement_callee`].
fn needed_calls<'e>(earlier: &[&'e Expr], test: Option<&'e Expr>) -> Vec<&'e Expr> {
    condition::leaves(earlier.iter().copied().chain(test))
        .into_iter()
        .filter_map(|leaf| match leaf {
            Expr::Call(call) if is_module_path(&call.func) => Some((leaf, &*call.func)),
            _ => None,
        })
        .filter(|&(call, _)| {
            condition::excluded(earlier, test, &|expr| {
                if std::ptr::eq(expr, call) {
                    Leaf::Fixed(false)
                } else {
                    Leaf::Opaque
                }
            })
        })
        .map(|(_, func)| func)
        .collect()
}

/// The dotted callee of a call made as a statement, its result dropped or
/// bound to names: `check(x)`, `pd = utils.check(x)` (#1269). A method of
/// `self` / `cls` is left out, as no import binds it.
fn statement_callee(stmt: &Stmt) -> Option<&Expr> {
    let value = match stmt {
        Stmt::Expr(expr) => &*expr.value,
        Stmt::Assign(assign) => &*assign.value,
        Stmt::AnnAssign(assign) => assign.value.as_deref()?,
        _ => return None,
    };
    let Expr::Call(call) = value else {
        return None;
    };
    is_module_path(&call.func).then_some(&*call.func)
}

/// `except ImportError:` / `except (ModuleNotFoundError, ...):`, or a class
/// a failed import also lands in: `Exception` / `BaseException` (#967).
/// Any other name, or an expression that is not a name, does not catch it.
fn catches_import_error(exc_type: &Expr) -> bool {
    match exc_type {
        Expr::Tuple(tuple) => tuple.elts.iter().any(is_import_error_class),
        _ => is_import_error_class(exc_type),
    }
}

/// `except ImportError:` / `except (ModuleNotFoundError, ...):`, which only
/// a failed import lands in.
fn names_import_error(exc_type: &Expr) -> bool {
    let named = |expr: &Expr| {
        matches!(
            subscript_name(expr),
            Some("ImportError" | "ModuleNotFoundError")
        )
    };
    match exc_type {
        Expr::Tuple(tuple) => tuple.elts.iter().any(named),
        _ => named(exc_type),
    }
}

fn is_import_error_class(expr: &Expr) -> bool {
    matches!(
        subscript_name(expr),
        Some("ImportError" | "ModuleNotFoundError" | "Exception" | "BaseException")
    )
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
    call.arguments.args.iter().any(is_import_error_class)
}

fn alias_as_name(alias: &Alias) -> Option<String> {
    alias.asname.as_ref().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit_source(source: &str) -> ParsedModule {
        visit_source_at("mod.py", source)
    }

    fn visit_source_at(path: &str, source: &str) -> ParsedModule {
        visit_source_in(path, FileContext::Runtime, source)
    }

    fn visit_source_in(path: &str, file_context: FileContext, source: &str) -> ParsedModule {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo::default();
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(path, &layout, file_context, &lines);
        visitor.visit_module(source, module.suite());
        visitor.into_parsed()
    }

    /// Dotted string literals, `pkg.mod:attr` included (#493), are kept once
    /// each at their first line, wherever they appear; computed strings are
    /// not seen (#1026).
    #[test]
    fn collects_dotted_string_literals_once_at_their_first_line() {
        let parsed = visit_source(
            "\"\"\"Loads pkg.cli.run by name.\"\"\"\nX = {\"a\": \"pkg.cli.run\"}\ndef load(name):\n    return [\"pkg.cli.run\", \"python:pkg.config\", f\"pkg.{name}\", \"json\", \"pkg.mod:attr\"]\n",
        );
        let refs: Vec<(&str, u32)> = parsed
            .module_refs
            .iter()
            .map(|reference| (reference.text.as_str(), reference.line))
            .collect();
        assert_eq!(
            refs,
            [
                ("pkg.cli.run", 2),
                ("pkg.mod:attr", 4),
                ("python:pkg.config", 4),
            ]
        );
    }

    /// A call that lists a directory or a package's modules marks the file,
    /// matched by the callee's last name; `walk` only through `os` (#1267).
    #[test]
    fn marks_files_that_list_a_directory() {
        let lists = |source: &str| visit_source(source).lists_directory;
        for source in [
            "import pkgutil\nfor _ in pkgutil.iter_modules(pkg.__path__):\n    pass\n",
            "from pkgutil import walk_packages\nwalk_packages(path)\n",
            "import glob\ndef find(path):\n    return glob.glob(path + \"/*.py\")\n",
            "from glob import iglob\niglob(\"*.py\")\n",
            "Path(here).rglob(\"*.py\")\n",
            "list(here.iterdir())\n",
            "import os\nos.listdir(here)\n",
            "import os\nos.scandir(here)\n",
            "import os\nos.walk(here)\n",
        ] {
            assert!(lists(source), "{source}");
        }
        for source in [
            "import ast\nast.walk(tree)\n",
            "import os\nos.path.join(here, \"x\")\n",
            "glob = 1\n",
        ] {
            assert!(!lists(source), "{source}");
        }
    }

    /// Literal `module:attr` apps passed to uvicorn's `run` and `Config`,
    /// aliases and submodule imports included, with whether `app_dir=` moves
    /// the import root; app objects, computed strings, bare modules and
    /// look-alike calls are not read (#1004).
    #[test]
    fn records_literal_apps_passed_to_uvicorn() {
        let parsed = visit_source(concat!(
            "import uvicorn\n",
            "import uvicorn as uv\n",
            "from uvicorn import run as serve, Config\n",
            "from uvicorn.config import Config as C\n",
            "uvicorn.run(\"pkg.asgi:app\", port=8000)\n",
            "uv.Config(app=\"pkg.cfg:application\")\n",
            "def main():\n",
            "    serve(\"pkg.main:create()\", factory=True)\n",
            "    Config(\"pkg.other:app\")\n",
            "    C(\"pkg.sub:app\", app_dir=\"src\")\n",
            "uvicorn.run(app)\n",
            "uvicorn.run(f\"pkg.{name}:app\")\n",
            "uvicorn.run(\"pkg.bare\")\n",
            "other.run(\"pkg.not_uvicorn:app\")\n",
            "run(\"pkg.not_imported:app\")\n",
        ));
        let submodules = visit_source(concat!(
            "import uvicorn.config\n",
            "from uvicorn.main import run\n",
            "uvicorn.Config(\"pkg.a:app\")\n",
            "run(\"pkg.b:app\")\n",
        ));
        let look_alikes = visit_source(concat!(
            "import uvicornish\n",
            "import starlette as server\n",
            "uvicorn.run(\"pkg.unbound:app\")\n",
            "server.run(\"pkg.aliased:app\")\n",
        ));
        let apps: Vec<(&str, &str, u32, bool)> = parsed
            .server_apps
            .iter()
            .chain(&submodules.server_apps)
            .chain(&look_alikes.server_apps)
            .map(|server| {
                (
                    server.call.as_str(),
                    server.app.as_str(),
                    server.line,
                    server.app_dir,
                )
            })
            .collect();
        assert_eq!(
            apps,
            [
                ("uvicorn.run", "pkg.asgi:app", 5, false),
                ("uvicorn.Config", "pkg.cfg:application", 6, false),
                ("uvicorn.run", "pkg.main:create()", 8, false),
                ("uvicorn.Config", "pkg.other:app", 9, false),
                ("uvicorn.Config", "pkg.sub:app", 10, true),
                ("uvicorn.Config", "pkg.a:app", 3, false),
                ("uvicorn.run", "pkg.b:app", 4, false),
            ]
        );
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

        // A tuple command reads like a list.
        let parsed = visit_source(
            "import subprocess, sys\nsubprocess.run((sys.executable, \"-m\", \"pip\"))\n",
        );
        assert!(parsed.imports.iter().any(|import| import.module == "pip"));
    }

    /// `from typing import TYPE_CHECKING as TC` names the flag; the same
    /// name from another module or a relative `typing`, or another name from
    /// `typing`, does not. A relative import of `import_module` is no loader.
    #[test]
    fn only_typing_type_checking_and_absolute_loaders_are_bound() {
        let context = |source: &str| {
            visit_source(source)
                .imports
                .iter()
                .find(|import| import.module == "guarded")
                .map(|import| import.context)
                .expect("guarded import")
        };
        assert_eq!(
            context("from typing import TYPE_CHECKING as TC\nif TC:\n    import guarded\n"),
            ImportContext::Type
        );
        for source in [
            "from other import TYPE_CHECKING as TC\nif TC:\n    import guarded\n",
            "from .typing import TYPE_CHECKING as TC\nif TC:\n    import guarded\n",
            "from typing import cast as TC\nif TC:\n    import guarded\n",
        ] {
            assert_eq!(context(source), ImportContext::Runtime, "{source}");
        }
        let dynamic = |source: &str| visit_source(source).dynamic_imports.len();
        assert_eq!(
            dynamic("from importlib import import_module\nimport_module(\"acme.plugin\")\n"),
            1
        );
        assert_eq!(
            dynamic("from .importlib import import_module\nimport_module(\"acme.plugin\")\n"),
            0
        );
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
    fn reads_literal_getattr_as_an_attribute_access() {
        let parsed = visit_source(
            "import acme.utils\nfrom acme import m\ngetattr(m, \"a\")\nhasattr(acme.utils, \"b\")\nsetattr(m, \"c\", 1)\ndelattr(m, \"d\")\ngetattr(m, name)\ngetattr(m)\ngetattr(other, \"e\")\nobj.getattr(m, \"f\")\n",
        );
        let accesses: Vec<(&str, &str, u32)> = parsed
            .attribute_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str(), access.line))
            .collect();
        assert_eq!(
            accesses,
            [
                ("m", "a", 3),
                ("acme.utils", "b", 4),
                ("m", "c", 5),
                ("m", "d", 6),
            ]
        );
    }

    #[test]
    fn reads_literal_getattr_on_an_import_module_result() {
        let parsed = visit_source(
            "import importlib\nmod = importlib.import_module(\"acme.m\")\ngetattr(mod, \"a\")\nhasattr(importlib.import_module(\"acme.n\"), \"b\")\ngetattr(mod, name)\n",
        );
        let accesses: Vec<(&str, &str, u32)> = parsed
            .loaded_module_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str(), access.line))
            .collect();
        assert_eq!(accesses, [("acme.m", "a", 3), ("acme.n", "b", 4)]);
    }

    #[test]
    fn source_mentions_rule_out_scans_only_on_ascii_sources() {
        let scan = |source: &str| {
            let mentions = SourceMentions::scan(source);
            (mentions.async_library, mentions.availability)
        };
        assert_eq!(scan("import os\n"), (false, false));
        assert_eq!(
            scan("lib = sniffio.current_async_library()\n"),
            (true, false)
        );
        assert_eq!(scan("if find_spec('x'): pass\n"), (false, true));
        assert_eq!(scan("if is_torch_available(): pass\n"), (false, true));
        assert_eq!(scan("# caf\u{e9}\n"), (true, true));
    }

    #[test]
    fn collects_attribute_accesses_inside_string_annotations() {
        let parsed = visit_source(
            "\
from typing import cast
from acme import m
import acme.n

def f(v, y: \"m.Param\" = None) -> \"list['acme.n.Ret']\":
    cast(\"m.Cast\", v)
    z: \"Literal['m.Value']\" = v
    return \"m.NotAType\"
",
        );
        let accesses: Vec<(&str, &str, u32)> = parsed
            .attribute_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str(), access.line))
            .collect();
        assert_eq!(
            accesses,
            [("m", "Param", 5), ("acme.n", "Ret", 5), ("m", "Cast", 6)]
        );
    }

    #[test]
    fn collects_attribute_accesses_on_literally_loaded_modules() {
        let parsed = visit_source(
            "\
import importlib
from importlib import import_module as im
importlib.import_module(\"acme.a\").direct
bound = im(\"acme.b\")
bound.through_name
alias = bound
alias.through_alias
__import__(\"acme.c\").not_c
rebound = importlib.import_module(\"acme.d\")
rebound = other
rebound.not_d

def scoped():
    bound.outer_scope
    inner = importlib.import_module(\"acme.e\")
    inner.inside
",
        );
        let accesses: Vec<(&str, &str, u32)> = parsed
            .loaded_module_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str(), access.line))
            .collect();
        assert_eq!(
            accesses,
            [
                ("acme.a", "direct", 3),
                ("acme.b", "through_name", 5),
                ("acme.b", "through_alias", 7),
                ("acme.e", "inside", 16),
            ]
        );
    }

    #[test]
    fn keeps_attribute_accesses_through_import_receivers_only() {
        // Symbol tracking reads an access only through a receiver an import
        // binds, wherever the import sits; the rest (`self.x`) is dropped.
        let parsed = visit_source(
            "\
import subprocess, sys
import acme.utils as au
from acme import models

def run(self, obj):
    self.cache.clear()
    obj.attr
    au.helper
    models.Model
    late.thing
    subprocess.run([sys.executable, \"-m\", \"tool\"])
    tool.main
    acme.utils.CONFIG

def later():
    import late
",
        );
        let accesses: Vec<(&str, &str)> = parsed
            .attribute_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str()))
            .collect();
        assert_eq!(
            accesses,
            [
                ("au", "helper"),
                ("models", "Model"),
                ("late", "thing"),
                ("subprocess", "run"),
                ("sys", "executable"),
                ("tool", "main"),
                ("acme.utils", "CONFIG"),
            ]
        );
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

    /// An augmented assignment reads its target: the name, the subscripted
    /// container and the attribute site all count (#1142).
    #[test]
    fn augmented_assignment_targets_are_reads() {
        let parsed = visit_source(
            "import m\nTOTAL = 0\nCACHE = {}\nSPARE = 0\n\ndef bump():\n    global TOTAL\n    TOTAL += 1\n    CACHE[\"a\"] += 1\n    m.counter += 1\n    m.REGISTRY[\"k\"] += 1\n",
        );
        let used: Vec<_> = parsed
            .symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.used_in_module))
            .collect();
        assert_eq!(
            used,
            [
                ("TOTAL", true),
                ("CACHE", true),
                ("SPARE", false),
                ("bump", false)
            ]
        );
        let accesses: Vec<(&str, &str)> = parsed
            .attribute_accesses
            .iter()
            .map(|access| (access.receiver.as_str(), access.name.as_str()))
            .collect();
        assert_eq!(accesses, [("m", "counter"), ("m", "REGISTRY")]);
    }

    #[test]
    fn marks_symbols_read_in_match_patterns() {
        // Capture names (`rest`, `y`) bind; only value and class patterns read (#1141).
        let parsed = visit_source(
            "class Point:\n    pass\n\nColor = 1\nrest = 1\ny = 1\n\ndef f(v):\n    match v:\n        case Point(x=0):\n            pass\n        case [*rest]:\n            pass\n        case Color.RED as y:\n            pass\n",
        );
        let used: Vec<_> = parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.used_in_module)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(used, ["Point", "Color"]);
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
    fn marks_symbols_read_in_string_type_expressions() {
        let parsed = visit_source(
            "A = 1\nB = 1\nC = 1\nD = 1\nE = 1\nF = 1\nG = 1\nU = TypeVar(\"U\")\nH = 1\n\n\
             import typing as t\nx = cast(\"A\", 1)\ny = t.cast(typ=\"list[B]\", val=1)\n\
             M = TypeVar(\"M\", bound=\"C\")\nN = TypeVar(\"N\", \"D\", \"E\", default=\"F\")\n\
             Alias: TypeAlias = \"list[G]\"\ntype Other = \"H\"\n",
        );
        let used: Vec<_> = parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.used_in_module)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(used, ["A", "B", "C", "D", "E", "F", "G", "H"]);

        let other_calls =
            visit_source("Assignee = 1\nB = 1\nz = Field(\"Assignee\")\nb = view.cast(\"B\")\n");
        assert!(
            other_calls
                .symbols
                .iter()
                .all(|symbol| !symbol.used_in_module)
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
    fn records_command_argument_list_heads_only_with_subprocess() {
        let source = "cmd = [\"transformers\", \"serve\", model]\nother = (\"ruff\", \"check\", path)\nunknown = [program, \"boto3\"]\n";
        assert!(visit_source(source).shell_commands.is_empty());
        let parsed = visit_source(&format!("import subprocess\n{source}"));
        assert_eq!(parsed.shell_commands, ["ruff", "transformers"]);
    }

    /// What tells a class's subclasses may be picked at runtime: bases,
    /// `__init_subclass__`, `__subclasses__()` callers and what they are
    /// handed, functions passing a parameter on (#1274).
    #[test]
    fn records_subclass_facts() {
        let source = "\
def all_subclasses(cls):
    queue = [cls]
    while queue:
        for child in queue.pop().__subclasses__():
            queue.append(child)

def platform_subclass(cls, *rest):
    return all_subclasses(cls)

class Group:
    def __new__(cls):
        return platform_subclass(Group)

class Linux(Group, base.Mixin):
    pass

class Plugin:
    def __init_subclass__(cls):
        pass

Plugin.__subclasses__()
print(Linux)

class Backend:
    @classmethod
    def every(cls):
        return cls.__subclasses__()

    @classmethod
    def others(cls):
        return Other.__subclasses__()

class RowPlugin(Plugin[Row]):
    pass

def later():
    return all_subclasses(Defined)

class Defined:
    pass
";
        let facts = visit_source(source).subclass_facts;
        assert_eq!(
            facts.classes,
            [
                (
                    "Linux".to_owned(),
                    vec!["Group".to_owned(), "Mixin".to_owned()]
                ),
                ("RowPlugin".to_owned(), vec!["Plugin".to_owned()]),
            ]
        );
        assert_eq!(
            facts.bases,
            [
                ("Backend".to_owned(), 27),
                ("Other".to_owned(), 31),
                ("Plugin".to_owned(), 17)
            ],
            "the first line of each class; a method's `cls` names its class"
        );
        assert_eq!(facts.enumerators, ["all_subclasses", "every", "others"]);
        assert_eq!(
            facts.forwards,
            [("platform_subclass".to_owned(), "all_subclasses".to_owned())]
        );
        assert_eq!(
            facts.passed,
            [
                ("all_subclasses".to_owned(), "Defined".to_owned(), 37),
                ("platform_subclass".to_owned(), "Group".to_owned(), 12),
                ("print".to_owned(), "Linux".to_owned(), 22),
            ]
        );
    }

    /// The module's own namespace read by a computed name, `.py` paths
    /// handed to file calls, and a computed `sys.path` entry (#1274).
    #[test]
    fn records_names_and_files_used_without_an_import() {
        let source = "\
import os, sys
globals()['X'] = 1
_ts = sys.modules[__name__]
DRIVER = os.path.join(here, 'driver', 'driver.py')
open('native.py', 'w')
sys.path.insert(0, 'src')
";
        let parsed = visit_source(source);
        assert_eq!(parsed.self_reference, Some(3));
        assert_eq!(parsed.path_refs, ["driver/driver.py"]);
        assert!(!parsed.adds_computed_sys_path);
        let parsed = visit_source("import sys\nsys.path.append(root)\nglobals()[name] = 1\n");
        assert!(parsed.adds_computed_sys_path);
        assert_eq!(parsed.self_reference, None);
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
        let layout = LayoutInfo::new("src", vec!["acme".to_owned()]);
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(
            "src/acme/__init__.py",
            &layout,
            FileContext::Runtime,
            &lines,
        );
        visitor.visit_module(source, module.suite());
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
        let layout = LayoutInfo::new("src", vec!["acme".to_owned()]);
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new(
            "src/acme/core/mod.py",
            &layout,
            FileContext::Runtime,
            &lines,
        );
        visitor.visit_module(source, module.suite());
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
match value:
    case utils.N | [utils.O]:
        pass
    case {utils.P: _}:
        pass
    case utils.Q(x=utils.R):
        pass
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
            ("N", 22),
            ("O", 22),
            ("P", 24),
            ("Q", 26),
            ("R", 26),
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

    /// A function body runs when called, after the `try` / `suppress` that
    /// wrapped its definition has finished, so its imports are not caught
    /// there (#968). A guard that only lets the definition happen when a
    /// package is installed (`else:`, a flag, an availability check) still
    /// holds when it is called, and so does a class body, which runs at once.
    #[test]
    fn caught_imports_stop_at_the_function_boundary() {
        let parsed = visit_source(
            "import importlib\n\ntry:\n    import yaml\n    import yaml_a\n\n    def load():\n        import requests\n\n    class Loader:\n        import yaml_b\n\n        def run(self):\n            import yaml_c\n\n    caller = lambda: importlib.import_module(\"yaml_d\")\n    eager = lambda m=importlib.import_module(\"yaml_i\"): m\nexcept ImportError:\n    yaml = None\n\n    def fallback():\n        import fallback_dep\nelse:\n    def dump():\n        import yaml_e\n\nwith suppress(ImportError):\n    def suppressed():\n        import yaml_f\n\nif yaml is not None:\n    def flagged():\n        import yaml_g\n\nif find_spec(\"yaml\") is None:\n    raise ImportError(\"yaml\")\n\ndef checked():\n    import yaml_h\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("importlib", false),
                ("yaml", true),
                ("yaml_a", true),
                ("requests", false),
                ("yaml_b", true),
                ("yaml_c", false),
                ("fallback_dep", false),
                ("yaml_e", true),
                ("yaml_f", false),
                ("yaml_g", true),
                ("yaml_h", true),
            ]
        );
        // A handler that never runs defines nothing, so a function there
        // keeps the fallback link.
        let handler = visit_source(
            "try:\n    import threading\nexcept ImportError:\n    def start():\n        import dummy_threading\n",
        );
        assert_eq!(handler.imports[1].fallback_for, ["threading"]);
        // A lambda's defaults are evaluated where it is defined.
        let lambdas: Vec<_> = parsed
            .dynamic_imports
            .iter()
            .map(|import| (import.module.as_str(), import.optional))
            .collect();
        assert_eq!(lambdas, [("yaml_d", false), ("yaml_i", true)]);
    }

    /// Nested `sys.version_info` tests narrow the minors an import runs on,
    /// inside functions too; outside them nothing bounds it (#997).
    #[test]
    fn version_info_tests_record_the_minors_an_import_runs_on() {
        let parsed = visit_source(
            "import sys\nif sys.version_info >= (3, 9):\n    import a\n    if sys.version_info < (3, 12):\n        import b\n    def f():\n        import c\nelse:\n    import d\nimport e\n\ndef g(sys):\n    if sys.version_info >= (3, 9):\n        import h\n",
        );
        let minors = |module: &str| {
            parsed
                .imports
                .iter()
                .find(|import| import.module == module)
                .and_then(|import| import.version_minors.clone())
        };
        assert_eq!(minors("a"), Some(vec![[9, u32::MAX]]));
        assert_eq!(minors("b"), Some(vec![[9, 11]]));
        assert_eq!(minors("c"), Some(vec![[9, u32::MAX]]));
        assert_eq!(minors("d"), Some(vec![[0, 8]]));
        assert_eq!(minors("e"), None);
        // A parameter named `sys` is not the module.
        assert_eq!(minors("h"), None);
        // An alias is read whether or not the file also tests the platform.
        let aliased =
            visit_source("import sys as s\nif s.version_info < (3, 9):\n    import old\n");
        assert_eq!(aliased.imports[1].version_minors, Some(vec![[0, 8]]));
    }

    /// A bare `except:` and `except Exception:` / `BaseException` catch a
    /// failed import too, so they fall back and set flags like
    /// `except ImportError:` (#967). Any other class does not.
    #[test]
    fn broad_handlers_catch_a_failed_import() {
        let parsed = visit_source(
            "try:\n    import threading\nexcept:\n    import dummy_threading\n\ntry:\n    import json\nexcept Exception:\n    import simplejson\n\ntry:\n    import pickle\nexcept (OSError, builtins.BaseException):\n    import cPickle\n\ntry:\n    import zlib\nexcept OSError:\n    import zlib_fallback\n\ntry:\n    import yaml\nexcept:\n    yaml = None\n\nif yaml is not None:\n    import yaml.nodes\n",
        );
        let fallback_for = |module: &str| {
            parsed
                .imports
                .iter()
                .find(|import| import.module == module)
                .map(|import| import.fallback_for.clone())
                .expect(module)
        };
        assert_eq!(fallback_for("dummy_threading"), ["threading"]);
        assert_eq!(fallback_for("simplejson"), ["json"]);
        assert_eq!(fallback_for("cPickle"), ["pickle"]);
        assert!(fallback_for("zlib_fallback").is_empty());
        assert!(
            parsed
                .imports
                .iter()
                .any(|import| import.module == "yaml.nodes" && import.optional)
        );
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

    /// #1269: a top-level function is an install check by its body alone:
    /// an unconditional `try` that imports absolute modules and raises from
    /// the first handler a missing package lands in, or returns `None` /
    /// `False` there (#1272).
    #[test]
    fn install_checks_are_read_from_function_bodies() {
        let parsed = visit_source(
            "def needs(caller):\n    try:\n        from x.sub import y\n        from .pkg import local\n        import z, w.core\n    except (ImportError, OSError) as e:\n        raise RuntimeError(caller) from e\n\ndef relative_only():\n    try:\n        from .pkg import local\n    except ImportError:\n        raise\n\ndef returns():\n    try:\n        import a\n    except ImportError:\n        return False\n\ndef narrow():\n    try:\n        import b\n    except ValueError:\n        raise\n\ndef nested():\n    if flag:\n        try:\n            import c\n        except ImportError:\n            raise\n\ndef returns_none():\n    try:\n        import d\n        return d\n    except ImportError:\n        return None\n\ndef returns_bare():\n    try:\n        import e\n    except ImportError:\n        log()\n        return\n    return True\n\ndef returns_value():\n    try:\n        import f\n    except ImportError:\n        return 0\n\ndef returns_true():\n    try:\n        import g\n    except ImportError:\n        return True\n\ndef mixed():\n    try:\n        import h\n    except ImportError:\n        return False\n    try:\n        import i\n    except ImportError:\n        raise\n",
        );
        let checks: Vec<(&str, Vec<&str>, bool)> = parsed
            .import_checks
            .iter()
            .map(|check| {
                (
                    check.function.as_str(),
                    check.modules.iter().map(String::as_str).collect(),
                    check.returns_falsy,
                )
            })
            .collect();
        assert_eq!(
            checks,
            [
                ("needs", vec!["w.core", "x.sub", "z"], false),
                ("returns", vec!["a"], true),
                ("returns_none", vec!["d"], true),
                ("returns_bare", vec!["e"], true),
                ("mixed", vec!["h", "i"], false),
            ]
        );
    }

    /// #1269: the statement calls an import runs after, results bound or
    /// dropped; a method of `self` and a call in a branch that is over do
    /// not count, and an optional import keeps none.
    #[test]
    fn imports_record_the_calls_they_run_after() {
        let parsed = visit_source(
            "import first\n\ndef f(self):\n    self.check()\n    import a\n    pd: object = utils.check_y()\n    import b\n    check_z()\n    check_z()\n    if flag:\n        later()\n    from c import d\n    try:\n        import e\n    except ImportError:\n        pass\n",
        );
        let called: Vec<(&str, Vec<&str>)> = parsed
            .imports
            .iter()
            .map(|import| {
                (
                    import.module.as_str(),
                    import.called_before.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            called,
            [
                ("first", vec![]),
                ("a", vec![]),
                ("b", vec!["utils.check_y"]),
                ("c", vec!["utils.check_y", "check_z"]),
                ("e", vec![]),
            ]
        );
    }

    /// #1272: the calls a branch test needs truthy, in its body or after an
    /// `if not ...: return`; a call the branch runs either way does not count.
    #[test]
    fn imports_record_the_calls_their_branch_tests_need() {
        let parsed = visit_source(
            "def f():\n    if has_x() and flag:\n        import a\n    if not utils.has_y():\n        import b\n    else:\n        import c\n    if has_z() or flag:\n        import d\n    if self.ready():\n        import e\n    if not probe():\n        return\n    import g\n\ndef h():\n    import i\n",
        );
        let tested: Vec<(&str, Vec<&str>)> = parsed
            .imports
            .iter()
            .map(|import| {
                (
                    import.module.as_str(),
                    import.tested_before.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            tested,
            [
                ("a", vec!["has_x"]),
                ("b", vec![]),
                ("c", vec!["utils.has_y"]),
                ("d", vec![]),
                ("e", vec![]),
                ("g", vec!["probe"]),
                ("i", vec![]),
            ]
        );
    }

    /// #1272: a module that is not installed is never loaded, so the body of
    /// a `sys.modules` check, and the rest of a block it returns from, run
    /// only with it, whatever they import (pandas' `printing.py`).
    #[test]
    fn loaded_module_checks_and_early_exits_gate_every_import() {
        let parsed = visit_source(
            "import sys\n\ndef enable():\n    if \"IPython\" not in sys.modules:\n        return\n    from IPython import get_ipython\n    from traitlets.traitlets import ObjectName\n\ndef approx():\n    np = sys.modules.get(\"numpy\")\n    if sys.modules.get(\"numpy\") is not None:\n        import numpy.testing\n    import plain\n\ndef raised():\n    if not is_torch_available():\n        raise RuntimeError(\"install torch\")\n    import safetensors\n\ndef returned_value():\n    if find_spec(\"x\") is None:\n        return None\n    import x_helper\n\ndef not_an_exit():\n    if \"IPython\" not in sys.modules:\n        log()\n    import y\n\ndef positive():\n    if \"IPython\" in sys.modules:\n        return\n    import z\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("sys", false),
                ("IPython", true),
                ("traitlets.traitlets", true),
                ("numpy.testing", true),
                ("plain", false),
                ("safetensors", true),
                ("x_helper", true),
                ("y", false),
                ("z", false),
            ]
        );
    }

    /// #1272: a module-level name bound once to a version test reads as the
    /// test, at module level and in a function that does not rebind it.
    #[test]
    fn version_names_bound_the_minors_like_the_test() {
        let parsed = visit_source(
            "import sys\nPY2 = sys.version_info[0] == 2\nPY3 = sys.version_info[0] == 3\nPY38 = sys.version_info < (3, 9)\nif PY3:\n    import io\nelse:\n    import StringIO\nif PY38:\n    import importlib_metadata\n\ndef f():\n    if PY2:\n        import cPickle\n\ndef g(PY2):\n    if PY2:\n        import shadowed\n",
        );
        let minors: Vec<(&str, Option<Vec<[u32; 2]>>)> = parsed
            .imports
            .iter()
            .map(|import| (import.module.as_str(), import.version_minors.clone()))
            .collect();
        assert_eq!(
            minors,
            [
                ("sys", None),
                ("io", None),
                ("StringIO", Some(vec![])),
                ("importlib_metadata", Some(vec![[0, 8]])),
                ("cPickle", Some(vec![])),
                ("shadowed", None),
            ]
        );
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

    /// #1242: redash's `utils/pandas.py` keeps `find_spec` results in a
    /// module-level flag and imports behind `if pandas_installed:`.
    #[test]
    fn availability_flags_guard_imports_like_try_flags() {
        let parsed = visit_source(
            "from importlib.util import find_spec
pandas_installed = find_spec(\"pandas\") and find_spec(\"numpy\")
has_torch = is_torch_available()
both = has_torch and other
either = has_torch or other
if pandas_installed:
    import numpy
if has_torch:
    import torch
if both:
    import torch_both
if either:
    import torch_either
if not pandas_installed:
    import fallback
spec = find_spec(\"yaml\")
ok = find_spec(\"toml\") is not None
if spec is not None:
    import yaml
if spec is None:
    import yaml_fallback
if ok is not None:
    import toml
",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("importlib.util", false),
                ("numpy", true),
                ("torch", true),
                ("torch_both", true),
                ("torch_either", false),
                ("fallback", false),
                ("yaml", true),
                ("yaml_fallback", false),
                ("toml", false),
            ]
        );
    }

    /// #1242: mlflow's `optimizers/dspy.py` calls `_check_dspy_installed()`
    /// at top level before `import dspy`; only the statements after it count.
    #[test]
    fn install_check_calls_gate_the_statements_after_them() {
        let parsed = visit_source(
            "import before
def checked():
    import early
    utils.require_torch_available()
    import torch
_check_dspy_installed()
import dspy
def run():
    import inner
",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("before", false),
                ("early", false),
                ("torch", true),
                ("dspy", true),
                ("inner", true),
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

    /// `x = None` in the handler makes `x is None` true without the package,
    /// so its `else:` and the statements after `if x is None: raise
    /// ImportError` are optional. A `has_x = True` flag is `False` there, not
    /// `None`, so `has_x is not None` guards nothing.
    #[test]
    fn none_flags_read_is_none_both_ways() {
        let parsed = visit_source(
            "try:\n    import yaml\nexcept ImportError:\n    yaml = None\ntry:\n    import toml\n    HAS_TOML = True\nexcept ImportError:\n    HAS_TOML = False\n\nif yaml is None:\n    pass\nelse:\n    import yaml.a\nif HAS_TOML is not None:\n    import toml.c\nif yaml is None:\n    raise ImportError(\"yaml\")\nimport yaml.b\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("yaml", true),
                ("toml", true),
                ("yaml.a", true),
                ("toml.c", false),
                ("yaml.b", true),
            ]
        );
    }

    /// A broad handler also catches whatever else the body raises, so its
    /// `x = None` is a flag only when the body imports something.
    #[test]
    fn broad_handler_flags_need_an_import_in_the_body() {
        let parsed = visit_source(
            "import os\ntry:\n    token = os.environ[\"TOKEN\"]\nexcept Exception:\n    token = None\ntry:\n    yaml = load()\nexcept ImportError:\n    yaml = None\ntry:\n    import toml\nexcept:\n    toml = None\ntry:\n    lxml = load()\nexcept (OSError, ModuleNotFoundError):\n    lxml = None\n\nif token:\n    import requests\nif yaml is not None:\n    import yaml.a\nif toml is not None:\n    import toml.a\nif lxml is not None:\n    import lxml.a\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [
                ("os", false),
                ("toml", true),
                ("requests", false),
                ("yaml.a", true),
                ("toml.a", true),
                ("lxml.a", true),
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

    /// A `raise ImportError` reads its test like an `if` body does (#958):
    /// a module-level try flag counts, through `not` / `and` / `or`, and a
    /// test that can be true with the package installed guards nothing.
    #[test]
    fn raise_guards_read_flags_like_branches() {
        let parsed = visit_source(
            "try:\n    import yaml\n    HAS_YAML = True\nexcept ImportError:\n    HAS_YAML = False\n\nif not HAS_YAML:\n    raise ImportError(\"pip install pyyaml\")\nimport yaml.representer\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [("yaml", true), ("yaml.representer", true)]
        );

        let parsed = visit_source(
            "try:\n    import yaml\nexcept ImportError:\n    yaml = None\n\nif not yaml or os.environ.get(\"NO_YAML\"):\n    raise ModuleNotFoundError(\"yaml\")\nimport yaml.a\n",
        );
        assert_eq!(optional_by_module(&parsed)[1], ("yaml.a", true));

        // `and` with an unknown leaf can be false with the package missing.
        let parsed = visit_source(
            "try:\n    import yaml\n    HAS_YAML = True\nexcept ImportError:\n    HAS_YAML = False\n\nif not HAS_YAML and os.environ.get(\"STRICT\"):\n    raise ImportError(\"yaml\")\nimport yaml.b\n",
        );
        assert_eq!(optional_by_module(&parsed)[1], ("yaml.b", false));

        // An unknown test, and a flag inside a function, guard nothing.
        let parsed = visit_source(
            "if not os.environ.get(\"YAML\"):\n    raise ImportError(\"yaml\")\nimport yaml\n\ndef load():\n    HAS_YAML = False\n    if not HAS_YAML:\n        raise ImportError(\"yaml\")\n    import yaml.c\n",
        );
        assert_eq!(
            optional_by_module(&parsed),
            [("yaml", false), ("yaml.c", false)]
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

    /// A branch's test runs only when the chain reaches it, so an import in
    /// the test of a branch that needs a package is optional too (#861).
    #[test]
    fn imports_in_a_guarded_test_are_optional() {
        let parsed = visit_source(
            "import importlib
if not is_yaml_available():
    pass
elif importlib.import_module('yaml').__version__ >= '6':
    pass
if is_toml_available() and importlib.import_module('toml'):
    pass
if importlib.import_module('ujson'):
    pass
",
        );
        let optional: Vec<(&str, bool)> = parsed
            .dynamic_imports
            .iter()
            .map(|import| (import.module.as_str(), import.optional))
            .collect();
        assert_eq!(optional, [("yaml", true), ("toml", true), ("ujson", false)]);
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

    /// `sys.platform.startswith(...)`, `os.name` and `platform.system()` are
    /// platform tests like a `sys.platform` comparison (#957), read through
    /// `not` and `elif` / `else`. The receiver must be the stdlib module the
    /// module imported; a local of the same name is an unknown test.
    #[test]
    fn platform_tests_read_every_stdlib_spelling() {
        let parsed = visit_source(
            "import os\nimport platform as plat\nimport sys\n\nif sys.platform.startswith(\"win\"):\n    import a1\nif not sys.platform.startswith((\"linux\", \"darwin\")):\n    import a2\nif os.name == \"nt\":\n    import a3\nelif os.name != \"posix\":\n    import a4\nelse:\n    import a5\nif plat.system() == \"Windows\":\n    import a6\nif sys.platform in (\"win32\", \"cygwin\"):\n    import a7\n\ndef local(platform, sys):\n    if platform.system() == \"Windows\":\n        import b1\n    if sys.platform == \"win32\":\n        import b2\n    import os\n    if os.name == \"nt\":\n        import a8\n\ndef rebound():\n    os = make_os()\n    if os.name == \"nt\":\n        import b3\n\nif platform.system() == \"Windows\":\n    import b4\nif os.name.upper() == \"NT\":\n    import b5\n",
        );
        let guarded: Vec<_> = parsed
            .imports
            .iter()
            .filter(|import| import.platform_guarded)
            .map(|import| import.module.as_str())
            .collect();
        assert_eq!(guarded, ["a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8"]);
    }

    /// A function importing its own platform module is found by the source
    /// scan, an aliased one by the module scope's names.
    #[test]
    fn platform_tests_outside_the_plain_module_spelling() {
        for source in [
            "import sys as s\nif s.platform == \"win32\":\n    import winreg\n",
            "def f():\n    import sys\n    if sys.platform == \"win32\":\n        import winreg\n",
            "def f():\n    import os\n    if os.name == \"nt\":\n        import winreg\n",
            "def f():\n    import platform\n    if platform.system() == \"Windows\":\n        import winreg\n",
        ] {
            let parsed = visit_source(source);
            let winreg = parsed
                .imports
                .iter()
                .find(|import| import.module == "winreg");
            assert!(
                winreg.is_some_and(|import| import.platform_guarded),
                "{source}"
            );
        }
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
        assert_eq!(find("plain_lib").context, ImportContext::Runtime);
        // Both run only off Windows (#861).
        assert!(find("typed_lib").platform_guarded);
        assert!(find("plain_lib").platform_guarded);
    }

    /// #681: an `is_x` predicate or sniffio check makes only imports of its
    /// module optional; `"x" in sys.modules` is a condition on `x` like
    /// `find_spec("x")`, so every import under it is (#1272).
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
                ("pandas", true),
                ("trio", true),
                ("asyncio", false),
                ("trio_util", false),
            ]
        );
    }

    /// #731: the sniffio result read back from a variable of the same scope
    /// guards like the call itself.
    #[test]
    fn async_library_variable_checks_make_matching_imports_optional() {
        let parsed = visit_source(
            "import sniffio
def via_variable():
    library = sniffio.current_async_library()
    if library == \"trio\":
        import trio
        import anyio
    elif \"curio\" == library:
        import curio
def reassigned(flag):
    library = sniffio.current_async_library()
    if flag:
        library = \"trio\"
    if library == \"trio\":
        import trio_typing
def from_caller(library=None):
    if library is None:
        library = sniffio.current_async_library()
    if library == \"trio\":
        import trio_util
def outer():
    library = sniffio.current_async_library()
    def inner():
        if library == \"trio\":
            import trio_asyncio
library = \"trio\"
if library == \"trio\":
    import trio_websocket
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
                ("sniffio", false),
                ("trio", true),
                ("anyio", false),
                ("curio", true),
                ("trio_typing", false),
                ("trio_util", false),
                ("trio_asyncio", false),
                ("trio_websocket", false),
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

    /// #1026: a module-level `__main__` block marks the file as a script;
    /// one inside a function, an unread guard form, and `__main__.py` do not.
    #[test]
    fn module_level_main_block_marks_the_file() {
        let has_block = |path, source| visit_source_at(path, source).has_main_block;
        assert!(has_block(
            "pkg/example.py",
            "def run():\n    pass\n\nif __name__ == \"__main__\":\n    run()\n"
        ));
        assert!(has_block(
            "pkg/example.py",
            "if DEBUG:\n    pass\nelif '__main__' == __name__:\n    pass\n"
        ));
        assert!(!has_block(
            "pkg/example.py",
            "def main():\n    if __name__ == \"__main__\":\n        pass\n"
        ));
        assert!(!has_block(
            "pkg/example.py",
            "class Tool:\n    if __name__ == \"__main__\":\n        DEBUG = True\n"
        ));
        assert!(!has_block(
            "pkg/example.py",
            "if __name__ in (\"__main__\",):\n    pass\n"
        ));
        assert!(has_block(
            "pkg/example.py",
            "if __name__ == \"__main__\" and not TYPE_CHECKING:\n    pass\n"
        ));
        assert!(!has_block(
            "pkg/example.py",
            "if __name__ == \"__main__\" or DEBUG:\n    pass\n"
        ));
        assert!(!has_block(
            "pkg/example.py",
            "from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    pass\n"
        ));
        assert!(!has_block(
            "pkg/__main__.py",
            "if __name__ == \"__main__\":\n    pass\n"
        ));
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// A `sys.path` edit or a plain assignment holding a path literal.
        fn statement() -> impl Strategy<Value = (String, String)> {
            (
                prop::collection::vec(prop::sample::select(&["lib", "utils", "a"][..]), 1..3),
                prop::sample::select(&["", "./", "../"][..]),
                any::<bool>(),
                0usize..4,
            )
                .prop_map(|(parts, prefix, trailing, form)| {
                    let hint = parts.join("/");
                    let literal = format!("\"{prefix}{hint}{}\"", if trailing { "/" } else { "" });
                    let statement = match form {
                        0 => format!("sys.path.insert(0, {literal})"),
                        1 => format!("sys.path.append({literal})"),
                        2 => format!("site.addsitedir({literal})"),
                        _ => format!("EXT = {literal}"),
                    };
                    (statement, hint)
                })
        }

        proptest! {
            /// The hints of a file are its path literals sorted and once, so
            /// their order in the file decides nothing (#719, #861).
            #[test]
            fn sys_path_hints_ignore_statement_order(
                statements in prop::collection::vec(statement(), 1..6),
                shift in any::<prop::sample::Index>(),
            ) {
                let render = |statements: &[(String, String)]| {
                    let body: String = statements
                        .iter()
                        .flat_map(|(statement, _)| [statement.as_str(), "\n"])
                        .collect();
                    visit_source(&format!("import site, sys\nsys.path.append(ROOT)\n{body}"))
                        .sys_path_hints
                };
                let mut expected: Vec<String> =
                    statements.iter().map(|(_, hint)| hint.clone()).collect();
                expected.sort_unstable();
                expected.dedup();
                let mut moved = statements.clone();
                let len = moved.len();
                moved.rotate_left(shift.index(len));
                moved.reverse();
                prop_assert_eq!(render(&statements), expected.clone());
                prop_assert_eq!(render(&moved), expected);
            }
        }

        /// What a generated `try` catches.
        #[derive(Clone, Copy, Debug)]
        enum Handler {
            ImportError,
            Tuple,
            /// `except ValueError:`, which a missing module does not raise.
            Other,
            /// A bare `except:`, which catches a failed import too.
            Bare,
            /// `except Exception:`, a base class of `ImportError`.
            Broad,
        }

        /// The test of a generated `if`; `condition::tests` covers how
        /// these combine in conditions and `elif` / `else` chains (#861).
        #[derive(Clone, Copy, Debug)]
        enum Guard {
            TypeChecking,
            Platform,
            Main,
            Opaque,
        }

        #[derive(Clone, Debug)]
        enum Block {
            Import { from: bool },
            Check,
            Try(Try),
            With { suppress: bool, body: Vec<Self> },
            If(Guard, Vec<Self>),
            Def(Vec<Self>),
            Class(Vec<Self>),
        }

        #[derive(Clone, Debug)]
        struct Try {
            body: Vec<Block>,
            handler: Handler,
            handled: Vec<Block>,
            orelse: Vec<Block>,
            finalbody: Vec<Block>,
        }

        const HEADER: &str = "import importlib.util\nimport os\nimport sys\n\
                              from contextlib import nullcontext, suppress\n\
                              from typing import TYPE_CHECKING\n";

        fn block() -> impl Strategy<Value = Block> {
            prop_oneof![
                3 => any::<bool>().prop_map(|from| Block::Import { from }),
                1 => Just(Block::Check),
            ]
            .prop_recursive(4, 24, 3, |inner| {
                let body = prop::collection::vec(inner, 0..3);
                let handler = prop_oneof![
                    Just(Handler::ImportError),
                    Just(Handler::Tuple),
                    Just(Handler::Other),
                    Just(Handler::Bare),
                    Just(Handler::Broad),
                ];
                let guard = prop_oneof![
                    Just(Guard::TypeChecking),
                    Just(Guard::Platform),
                    Just(Guard::Main),
                    Just(Guard::Opaque),
                ];
                prop_oneof![
                    (
                        body.clone(),
                        handler,
                        body.clone(),
                        body.clone(),
                        body.clone()
                    )
                        .prop_map(
                            |(body, handler, handled, orelse, finalbody)| {
                                Block::Try(Try {
                                    body,
                                    handler,
                                    handled,
                                    orelse,
                                    finalbody,
                                })
                            }
                        ),
                    (any::<bool>(), body.clone())
                        .prop_map(|(suppress, body)| Block::With { suppress, body }),
                    (guard, body.clone()).prop_map(|(guard, body)| Block::If(guard, body)),
                    body.clone().prop_map(Block::Def),
                    body.prop_map(Block::Class),
                ]
            })
        }

        /// Whether `blocks` check for a package outside nested scopes, as
        /// `first_availability_check` looks for one.
        fn checks(blocks: &[Block]) -> bool {
            blocks.iter().any(|block| match block {
                Block::Check => true,
                Block::Import { .. } | Block::Def(_) | Block::Class(_) => false,
                Block::Try(Try {
                    body,
                    handled,
                    orelse,
                    finalbody,
                    ..
                }) => checks(body) || checks(handled) || checks(orelse) || checks(finalbody),
                Block::With { body, .. } | Block::If(_, body) => checks(body),
            })
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        struct Flags {
            optional: bool,
            platform_guarded: bool,
            deferred: bool,
            context: ImportContext,
            fallback_for: Vec<String>,
        }

        #[expect(
            clippy::struct_excessive_bools,
            reason = "independent scope flags pushed and popped together with the frame"
        )]
        #[derive(Clone, Default)]
        struct Frame {
            /// Inside a `try` body or a suppressing `with` (#614), up to the
            /// nearest function (#968).
            caught: bool,
            /// Inside a `try`'s `else:` or after a sibling check that raises
            /// `ImportError` (#695), nested functions included.
            gated: bool,
            platform: bool,
            type_checking: bool,
            main_block: bool,
            /// The innermost def / class is a function.
            function_scope: bool,
            /// Roots of the `try` body an enclosing `except ImportError:`
            /// falls back for (#721).
            fallback_for: Vec<String>,
        }

        struct Function {
            /// An enclosing function checks for a package, so every import
            /// here may run after that check (#713).
            all: bool,
            /// This function checks for a package outside nested scopes.
            checks: bool,
            /// The check comes before the current statement.
            seen: bool,
        }

        /// Renders blocks as Python and records the flags each import
        /// should get, in source order.
        struct Model {
            default_context: ImportContext,
            /// `__name__ == "__main__"` is false when the file is imported.
            main_is_dev: bool,
            source: String,
            names: usize,
            functions: Vec<Function>,
            expected: Vec<(String, Flags)>,
        }

        impl Model {
            fn line(&mut self, depth: usize, text: &str) {
                self.source.push_str(&"    ".repeat(depth));
                self.source.push_str(text);
                self.source.push('\n');
            }

            fn fresh(&mut self, prefix: &str) -> String {
                self.names += 1;
                format!("{prefix}{}", self.names)
            }

            fn body(&mut self, blocks: &[Block], depth: usize, frame: &Frame) {
                if blocks.is_empty() {
                    self.line(depth, "pass");
                }
                let mut frame = frame.clone();
                for block in blocks {
                    self.block(block, depth, &frame);
                    if matches!(block, Block::Check) {
                        frame.gated = true;
                    }
                }
            }

            fn import(&mut self, from: bool, depth: usize, frame: &Frame) {
                let module = self.fresh("zzm");
                let statement = if from {
                    format!("from {module} import x")
                } else {
                    format!("import {module}")
                };
                self.line(depth, &statement);
                let checked = self
                    .functions
                    .last()
                    .is_some_and(|function| function.all || function.seen);
                let context = if frame.type_checking {
                    ImportContext::Type
                } else if frame.main_block && self.main_is_dev {
                    ImportContext::Dev
                } else {
                    self.default_context
                };
                let flags = Flags {
                    optional: frame.caught || frame.gated || checked,
                    platform_guarded: frame.platform,
                    deferred: !self.functions.is_empty(),
                    context,
                    fallback_for: frame.fallback_for.clone(),
                };
                self.expected.push((module, flags));
            }

            fn try_block(&mut self, block: &Try, depth: usize, frame: &Frame) {
                let Try {
                    body,
                    handler,
                    handled,
                    orelse,
                    finalbody,
                } = block;
                let caught = Frame {
                    caught: true,
                    ..frame.clone()
                };
                let gated = Frame {
                    gated: true,
                    ..frame.clone()
                };
                self.line(depth, "try:");
                let first = self.names + 1;
                self.body(body, depth + 1, &caught);
                let primaries = (!body.is_empty()
                    && body
                        .iter()
                        .all(|block| matches!(block, Block::Import { .. })))
                .then(|| (first..=self.names).map(|n| format!("zzm{n}")).collect());
                let (clause, import_failed) = match handler {
                    Handler::ImportError => ("except ImportError:", true),
                    Handler::Tuple => ("except (ValueError, ModuleNotFoundError):", true),
                    Handler::Other => ("except ValueError:", false),
                    Handler::Bare => ("except:", true),
                    Handler::Broad => ("except Exception:", true),
                };
                self.line(depth, clause);
                let mut handling = frame.clone();
                if import_failed && let Some(primaries) = primaries {
                    handling.fallback_for = primaries;
                }
                self.body(handled, depth + 1, &handling);
                if !orelse.is_empty() {
                    self.line(depth, "else:");
                    self.body(orelse, depth + 1, &gated);
                }
                if !finalbody.is_empty() {
                    self.line(depth, "finally:");
                    self.body(finalbody, depth + 1, frame);
                }
            }

            fn block(&mut self, block: &Block, depth: usize, frame: &Frame) {
                match block {
                    Block::Import { from } => self.import(*from, depth, frame),
                    Block::Check => {
                        self.line(depth, "if importlib.util.find_spec(\"zzc\") is None:");
                        self.line(depth + 1, "raise ImportError(\"zzc\")");
                        if frame.function_scope
                            && let Some(function) = self.functions.last_mut()
                        {
                            function.seen = true;
                        }
                    },
                    Block::Try(block) => self.try_block(block, depth, frame),
                    Block::With { suppress, body } => {
                        let mut inner = frame.clone();
                        if *suppress {
                            self.line(depth, "with suppress(ImportError):");
                            inner.caught = true;
                        } else {
                            self.line(depth, "with nullcontext():");
                        }
                        self.body(body, depth + 1, &inner);
                    },
                    Block::If(guard, body) => {
                        let mut inner = frame.clone();
                        let test = match guard {
                            Guard::TypeChecking => {
                                inner.type_checking = true;
                                "TYPE_CHECKING"
                            },
                            Guard::Platform => {
                                inner.platform = true;
                                "sys.platform == \"win32\""
                            },
                            Guard::Main => {
                                inner.main_block = true;
                                "__name__ == \"__main__\""
                            },
                            Guard::Opaque => "os.environ.get(\"CHOKKIN\")",
                        };
                        self.line(depth, &format!("if {test}:"));
                        self.body(body, depth + 1, &inner);
                    },
                    Block::Def(body) => {
                        let name = self.fresh("f");
                        self.line(depth, &format!("def {name}():"));
                        let all = self
                            .functions
                            .last()
                            .is_some_and(|function| function.all || function.checks);
                        self.functions.push(Function {
                            all,
                            checks: checks(body),
                            seen: false,
                        });
                        let inner = Frame {
                            caught: false,
                            function_scope: true,
                            ..frame.clone()
                        };
                        self.body(body, depth + 1, &inner);
                        self.functions.pop();
                    },
                    Block::Class(body) => {
                        let name = self.fresh("C");
                        self.line(depth, &format!("class {name}:"));
                        let inner = Frame {
                            function_scope: false,
                            ..frame.clone()
                        };
                        self.body(body, depth + 1, &inner);
                    },
                }
            }
        }

        const PATHS: [&str; 3] = ["pkg/mod.py", "pkg/__main__.py", "notebooks/nb.ipynb"];

        fn file_context() -> impl Strategy<Value = FileContext> {
            prop_oneof![
                Just(FileContext::Runtime),
                Just(FileContext::Test),
                Just(FileContext::Docs),
                Just(FileContext::Dev),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(256)))]

            /// Every import gets the `optional` / `platform_guarded` /
            /// `deferred` / context flags and the fallback roots spec §10
            /// gives its enclosing statements, and leaving a statement
            /// restores them: the import the file ends with is plain (#953).
            #[test]
            fn import_flags_follow_the_enclosing_statements(
                blocks in prop::collection::vec(block(), 1..4),
                path in prop::sample::select(&PATHS[..]),
                file_context in file_context(),
            ) {
                let default_context = if file_context == FileContext::Test {
                    ImportContext::Test
                } else {
                    ImportContext::Runtime
                };
                let mut model = Model {
                    default_context,
                    main_is_dev: path == "pkg/mod.py" && default_context == ImportContext::Runtime,
                    source: HEADER.to_owned(),
                    names: 0,
                    functions: Vec::new(),
                    expected: Vec::new(),
                };
                // A module-level check that raises makes the rest of the
                // module optional; nothing else outlives its statement.
                let checked = blocks.iter().any(|block| matches!(block, Block::Check));
                let mut blocks = blocks;
                blocks.push(Block::Import { from: false });
                model.body(&blocks, 0, &Frame::default());
                let plain = Flags {
                    optional: checked,
                    platform_guarded: false,
                    deferred: false,
                    context: default_context,
                    fallback_for: Vec::new(),
                };
                prop_assert_eq!(&model.expected.last().expect("trailing import").1, &plain);
                let actual: Vec<(String, Flags)> =
                    visit_source_in(path, file_context, &model.source)
                        .imports
                        .into_iter()
                        .filter(|import| import.module.starts_with("zzm"))
                        .map(|import| {
                            let flags = Flags {
                                optional: import.optional,
                                platform_guarded: import.platform_guarded,
                                deferred: import.deferred,
                                context: import.context,
                                fallback_for: import.fallback_for,
                            };
                            (import.module, flags)
                        })
                        .collect();
                prop_assert_eq!(actual, model.expected, "{}", model.source);
            }
        }
    }
}
