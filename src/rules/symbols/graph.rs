//! Symbol identity and registry for usage analysis.

use std::collections::{HashMap, HashSet};

use crate::parser::{ImportKind, ParsedModule, SymbolDef};

use super::public::star_imports;

/// Rules-local symbol identifier (distinct from graph `SymbolId` if added later).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SymbolId {
    /// Dotted module name.
    pub module: String,
    /// Symbol name within the module.
    pub name: String,
}

impl SymbolId {
    /// Creates a symbol id from module and name parts.
    #[must_use]
    pub(super) fn new(module: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            name: name.into(),
        }
    }
}

/// One registered public symbol in a reachable module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryEntry {
    pub id: SymbolId,
    pub path: String,
    pub def: SymbolDef,
    pub in_all: bool,
}

/// Build the list of public symbols in reachable modules, first definition wins.
pub(super) fn build_registry(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<RegistryEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for module in modules {
        let Some(owner) = module_names.get(module.path.as_str()) else {
            continue;
        };
        for symbol in &module.symbols {
            if !symbol.is_public || symbol.in_type_checking {
                continue;
            }
            let id = SymbolId::new(owner.clone(), symbol.name.clone());
            if !seen.insert(id.clone()) {
                continue;
            }
            let in_all = module.exports.contains(&symbol.name);
            entries.push(RegistryEntry {
                id,
                path: module.path.clone(),
                def: symbol.clone(),
                in_all,
            });
        }
    }

    entries
}

/// Precomputed lookup of symbol references collected from import statements.
///
/// Usage checks run once per registered symbol, so scanning a reference list
/// each time is quadratic in project size; this index is built in one pass
/// over the parsed modules and resolves each lookup in O(1). The value marks
/// whether the symbol was referenced from a module other than its own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ReferenceIndex {
    entries: HashMap<SymbolId, bool>,
}

impl ReferenceIndex {
    /// Collect symbol references from `from … import name` statements and
    /// attribute access on module bindings (`import module; module.name`,
    /// `from package import module; module.name`).
    ///
    /// Modules without a module name (e.g. `tests/` without `__init__.py`)
    /// still count as importers: their path never equals a module name, so
    /// every reference they make is external.
    pub(super) fn build(modules: &[&ParsedModule], module_names: &HashMap<&str, String>) -> Self {
        let mut index = Self::default();
        let known_modules: HashSet<&str> = module_names.values().map(String::as_str).collect();
        let mut stars = StarImports::build(modules, module_names);

        for module in modules {
            let importer = module_names
                .get(module.path.as_str())
                .map_or(module.path.as_str(), String::as_str);
            for import in &module.imports {
                if import.module.is_empty() {
                    continue;
                }
                match import.kind {
                    ImportKind::ImportFrom => {
                        let Some((package, name)) = import.import_target() else {
                            continue;
                        };
                        // `from . import x` in the package's own `__init__` defines a re-export, not a use.
                        if import.name.is_some() || package != importer {
                            index.record(importer, SymbolId::new(package, name), &mut stars);
                        }
                        let submodule = format!("{package}.{name}");
                        if let Some(binding) = import.bound_name()
                            && known_modules.contains(submodule.as_str())
                        {
                            for target in accesses(module, &submodule, &[binding]) {
                                index.record(importer, target, &mut stars);
                            }
                        }
                    },
                    ImportKind::Import => {
                        let binding = import.alias.as_deref().unwrap_or(&import.module);
                        let receivers = [import.module.as_str(), binding];
                        for target in accesses(module, &import.module, &receivers) {
                            index.record(importer, target, &mut stars);
                        }
                    },
                }
            }
        }

        index
    }

    /// `pkg.name` also uses `name` in each module `pkg` star-imports it from
    /// (#576); the importer, not `pkg`, decides whether that use is external.
    fn record(&mut self, importer: &str, target: SymbolId, stars: &mut StarImports<'_>) {
        for &source in stars.sources(&target) {
            self.mark(importer, SymbolId::new(source, target.name.clone()));
        }
        self.mark(importer, target);
    }

    fn mark(&mut self, importer: &str, target: SymbolId) {
        let external = importer != target.module;
        self.entries
            .entry(target)
            .and_modify(|is_external| *is_external |= external)
            .or_insert(external);
    }

    /// Returns `true` when `target` is referenced from any module.
    pub(super) fn is_referenced(&self, target: &SymbolId) -> bool {
        self.entries.contains_key(target)
    }

    /// Returns `true` when `target` is referenced from a different module.
    pub(super) fn is_externally_referenced(&self, target: &SymbolId) -> bool {
        matches!(self.entries.get(target), Some(true))
    }
}

/// `target_module.name` read through any of `receivers` in `module`.
fn accesses<'m>(
    module: &'m ParsedModule,
    target_module: &'m str,
    receivers: &'m [&'m str],
) -> impl Iterator<Item = SymbolId> + 'm {
    module
        .attribute_accesses
        .iter()
        .filter(|access| receivers.contains(&access.receiver.as_str()))
        .map(move |access| SymbolId::new(target_module, access.name.clone()))
}

/// `from m import *` edges between named modules.
#[derive(Default)]
struct StarImports<'a> {
    /// Names each star-importing module defines itself, shadowing
    /// star-imported ones.
    defines: HashMap<&'a str, HashSet<&'a str>>,
    /// The modules that star-import each module.
    parents: HashMap<&'a str, Vec<&'a str>>,
    /// Star-imported modules that define or import each name. A forwarded
    /// reference can only meet a symbol or a re-export there, so the
    /// packages a name merely passes through are never recorded (#803).
    binders: HashMap<&'a str, Vec<&'a str>>,
    /// Star-imported modules missing from the parsed ones, such as reachable
    /// modules left out of the `--production` evidence: they may bind any name.
    opaque: Vec<&'a str>,
    /// `__all__` of the modules that declare one.
    exports: HashMap<&'a str, HashSet<&'a str>>,
    cache: HashMap<SymbolId, Vec<&'a str>>,
}

impl<'a> StarImports<'a> {
    fn build(modules: &[&'a ParsedModule], module_names: &'a HashMap<&str, String>) -> Self {
        let mut stars = Self::default();
        for module in modules {
            let Some(name) = module_names.get(module.path.as_str()) else {
                continue;
            };
            let targets: Vec<_> = star_imports(module)
                .filter(|target| target != name)
                .collect();
            for &target in &targets {
                stars.parents.entry(target).or_default().push(name.as_str());
            }
            if !targets.is_empty() {
                stars.defines.entry(name.as_str()).or_default().extend(
                    module
                        .symbols
                        .iter()
                        .filter(|symbol| !symbol.in_type_checking)
                        .map(|symbol| symbol.name.as_str()),
                );
            }
            if !module.exports.names().is_empty() {
                stars.exports.insert(
                    name.as_str(),
                    module.exports.names().iter().map(String::as_str).collect(),
                );
            }
        }
        let mut parsed = HashSet::new();
        for module in modules {
            let Some(name) = module_names.get(module.path.as_str()) else {
                continue;
            };
            parsed.insert(name.as_str());
            if !stars.parents.contains_key(name.as_str()) {
                continue;
            }
            for bound in bound_names(module) {
                stars.binders.entry(bound).or_default().push(name.as_str());
            }
        }
        stars.opaque = stars
            .parents
            .keys()
            .copied()
            .filter(|module| !parsed.contains(module))
            .collect();
        stars
    }

    /// Cached per symbol: a module that imports a package in many function
    /// bodies records each `pkg.name` access once per import.
    fn sources(&mut self, target: &SymbolId) -> &[&'a str] {
        if !self.defines.contains_key(target.module.as_str()) {
            return &[];
        }
        if !self.cache.contains_key(target) {
            let sources = self
                .binders
                .get(target.name.as_str())
                .into_iter()
                .flatten()
                .chain(&self.opaque)
                .copied()
                .filter(|&binder| self.reaches(&target.module, binder, &target.name))
                .collect();
            self.cache.insert(target.clone(), sources);
        }
        &self.cache[target]
    }

    /// Whether `module` reaches `name` in `binder` through chained star
    /// imports. Each hop hands on only the names in its `__all__`, or without
    /// one, the names not starting with `_`, and stops at a module defining
    /// `name` itself. Walks up from `binder`: a name has few binders, while a
    /// package can star-import hundreds of modules.
    fn reaches(&self, module: &str, binder: &'a str, name: &str) -> bool {
        if !self.hands_on(binder, name) {
            return false;
        }
        let mut queue = vec![binder];
        let mut seen = HashSet::from([binder]);
        while let Some(current) = queue.pop() {
            for &parent in self.parents.get(current).into_iter().flatten() {
                if self.defines[parent].contains(name) {
                    continue;
                }
                if parent == module {
                    return true;
                }
                if !self.hands_on(parent, name) {
                    continue;
                }
                if seen.insert(parent) {
                    queue.push(parent);
                }
            }
        }
        false
    }

    fn hands_on(&self, module: &str, name: &str) -> bool {
        match self.exports.get(module) {
            Some(all) => all.contains(name),
            None => !name.starts_with('_'),
        }
    }
}

/// Names `module` binds by definition or `from … import`: every symbol and
/// re-export a reference to `module` can be asked about.
fn bound_names(module: &ParsedModule) -> HashSet<&str> {
    let defined = module.symbols.iter().map(|symbol| symbol.name.as_str());
    let imported = module
        .imports
        .iter()
        .filter(|import| import.kind == ImportKind::ImportFrom)
        .filter_map(|import| import.bound_name());
    defined.chain(imported).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ImportContext, ImportRef};

    fn import(
        kind: ImportKind,
        module: &str,
        name: Option<&str>,
        alias: Option<&str>,
    ) -> ImportRef {
        ImportRef {
            module: module.to_owned(),
            name: name.map(str::to_owned),
            alias: alias.map(str::to_owned),
            line: 1,
            kind,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            deferred: false,
            relative_level: 0,
            fallback_for: Vec::new(),
        }
    }

    #[test]
    fn bound_names_cover_from_imports_but_not_plain_or_star_imports() {
        let module = ParsedModule {
            imports: vec![
                import(ImportKind::ImportFrom, "pkg.core", Some("helper"), None),
                import(
                    ImportKind::ImportFrom,
                    "pkg.core",
                    Some("tool"),
                    Some("renamed"),
                ),
                import(ImportKind::ImportFrom, "pkg.sub", None, None),
                import(ImportKind::ImportFrom, "pkg.other", Some("*"), None),
                import(ImportKind::Import, "os.path", None, None),
                import(ImportKind::Import, "json", None, Some("js")),
            ],
            ..ParsedModule::default()
        };
        let names = bound_names(&module);
        assert_eq!(names, HashSet::from(["helper", "renamed", "sub"]));
    }

    mod props {
        use std::collections::{BTreeMap, BTreeSet};

        use proptest::prelude::*;
        use proptest::sample::Index;

        use super::*;
        use crate::parser::{Exports, SymbolKind};

        const MODULES: usize = 5;
        const NAMES: [&str; 3] = ["alpha", "beta", "_gamma"];
        /// A star target no generated module is: it may bind any name.
        const OPAQUE: &str = "vendored.blob";

        #[derive(Debug, Clone)]
        struct Module {
            /// Defined names, each maybe under `TYPE_CHECKING`.
            symbols: Vec<(usize, bool)>,
            /// Names bound by `from ext import name`.
            imported: Vec<usize>,
            /// Star targets: a module index, or `MODULES` for [`OPAQUE`].
            stars: Vec<usize>,
            all: Option<Vec<usize>>,
        }

        fn module() -> impl Strategy<Value = Module> {
            let name = 0..NAMES.len();
            (
                prop::collection::vec((name.clone(), any::<bool>()), 0..3),
                prop::collection::vec(name.clone(), 0..2),
                prop::collection::vec(0..=MODULES, 0..3),
                prop::option::of(prop::collection::vec(name, 0..3)),
            )
                .prop_map(|(symbols, imported, stars, all)| Module {
                    symbols,
                    imported,
                    stars,
                    all,
                })
        }

        fn modules() -> impl Strategy<Value = Vec<Module>> {
            prop::collection::vec(module(), MODULES)
        }

        fn module_name(index: usize) -> String {
            if index == MODULES {
                OPAQUE.to_owned()
            } else {
                format!("pkg.m{index}")
            }
        }

        fn parsed(index: usize, module: &Module) -> ParsedModule {
            let symbols = module
                .symbols
                .iter()
                .map(|&(name, in_type_checking)| SymbolDef {
                    name: NAMES[name].to_owned(),
                    kind: SymbolKind::Function,
                    line: 1,
                    is_public: !NAMES[name].starts_with('_'),
                    decorators: Vec::new(),
                    in_type_checking,
                    used_in_module: false,
                })
                .collect();
            let imported = module
                .imported
                .iter()
                .map(|&name| import(ImportKind::ImportFrom, "ext", Some(NAMES[name]), None));
            let stars = module.stars.iter().map(|&target| {
                import(
                    ImportKind::ImportFrom,
                    &module_name(target),
                    Some("*"),
                    None,
                )
            });
            ParsedModule {
                path: format!("pkg/m{index}.py"),
                symbols,
                imports: imported.chain(stars).collect(),
                exports: module.all.as_ref().map_or(Exports::Absent, |all| {
                    Exports::Known(all.iter().map(|&name| NAMES[name].to_owned()).collect())
                }),
                ..ParsedModule::default()
            }
        }

        type Sources = BTreeMap<(String, &'static str), BTreeSet<String>>;

        /// `StarImports::sources` for every (module, name) pair, with the
        /// modules handed over in `order`.
        fn star_sources(modules: &[Module], order: &[usize]) -> Sources {
            let parsed: Vec<_> = order
                .iter()
                .map(|&index| parsed(index, &modules[index]))
                .collect();
            let refs: Vec<&ParsedModule> = parsed.iter().collect();
            let names: HashMap<&str, String> = parsed
                .iter()
                .zip(order)
                .map(|(module, &index)| (module.path.as_str(), module_name(index)))
                .collect();
            let mut stars = StarImports::build(&refs, &names);
            let mut sources = Sources::new();
            for index in 0..MODULES {
                for name in NAMES {
                    let found = stars
                        .sources(&SymbolId::new(module_name(index), name))
                        .iter()
                        .map(|&source| source.to_owned())
                        .collect();
                    sources.insert((module_name(index), name), found);
                }
            }
            sources
        }

        /// The naive reading of the spec (§symbols, #576): iterate to a fixed
        /// point the (name, binder) pairs each module's namespace gets from
        /// its star imports. A target hands on its own bindings and what it
        /// got itself, filtered by its `__all__` (empty counts as none), and
        /// a module's own definitions shadow what it would get.
        fn reference(modules: &[Module]) -> Sources {
            let hands_on = |target: usize, name: &str| match modules
                .get(target)
                .and_then(|module| module.all.as_ref())
                .filter(|all| !all.is_empty())
            {
                Some(all) => all.iter().any(|&export| NAMES[export] == name),
                None => !name.starts_with('_'),
            };
            let bindings = |target: usize| -> BTreeSet<&'static str> {
                match modules.get(target) {
                    Some(module) => module
                        .symbols
                        .iter()
                        .map(|&(name, _)| NAMES[name])
                        .chain(module.imported.iter().map(|&name| NAMES[name]))
                        .collect(),
                    None => NAMES.into_iter().collect(),
                }
            };
            let mut got: Vec<BTreeSet<(&'static str, usize)>> = vec![BTreeSet::new(); MODULES];
            loop {
                let mut changed = false;
                for (index, module) in modules.iter().enumerate() {
                    let defines: BTreeSet<_> = module
                        .symbols
                        .iter()
                        .filter(|&&(_, in_type_checking)| !in_type_checking)
                        .map(|&(name, _)| NAMES[name])
                        .collect();
                    for &target in module.stars.iter().filter(|&&target| target != index) {
                        let own = bindings(target).into_iter().map(|name| (name, target));
                        let passed = got.get(target).cloned().unwrap_or_default();
                        let handed: Vec<_> = own
                            .chain(passed)
                            .filter(|&(name, _)| hands_on(target, name) && !defines.contains(name))
                            .collect();
                        for pair in handed {
                            changed |= got[index].insert(pair);
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            let mut sources = Sources::new();
            for (index, pairs) in got.iter().enumerate() {
                for name in NAMES {
                    let found = pairs
                        .iter()
                        .filter(|&&(bound, _)| bound == name)
                        .map(|&(_, source)| module_name(source))
                        .collect();
                    sources.insert((module_name(index), name), found);
                }
            }
            sources
        }

        fn identity() -> Vec<usize> {
            (0..MODULES).collect()
        }

        proptest! {
            #[test]
            fn star_sources_match_the_fixed_point(modules in modules()) {
                prop_assert_eq!(star_sources(&modules, &identity()), reference(&modules));
            }

            #[test]
            fn star_sources_ignore_module_order(
                modules in modules(),
                order in Just(identity()).prop_shuffle(),
            ) {
                prop_assert_eq!(
                    star_sources(&modules, &order),
                    star_sources(&modules, &identity())
                );
            }

            #[test]
            fn adding_all_only_drops_the_names_it_leaves_out(
                mut modules in modules(),
                target in any::<Index>(),
                all in prop::collection::btree_set(0..NAMES.len() - 1, 1..NAMES.len()),
            ) {
                let target = target.index(MODULES);
                modules[target].all = None;
                let before = star_sources(&modules, &identity());
                modules[target].all = Some(all.iter().copied().collect());
                let after = star_sources(&modules, &identity());
                for (key, sources) in &after {
                    if all.iter().any(|&export| NAMES[export] == key.1) {
                        prop_assert_eq!(sources, &before[key], "{:?}", key);
                    } else {
                        prop_assert!(sources.is_subset(&before[key]), "{:?}", key);
                    }
                }
            }
        }
    }
}
