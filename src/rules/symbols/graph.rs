//! Symbol identity and registry for usage analysis.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::parser::{Exports, ImportKind, ParsedModule, SymbolDef};
use crate::sources::{LayoutInfo, SymlinkModules};

use super::public::StarChain;

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
    module_names: &FxHashMap<&str, String>,
) -> Vec<RegistryEntry> {
    let mut entries = Vec::new();
    let mut seen = FxHashSet::default();

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
#[derive(Debug, Clone, Default)]
pub(super) struct ReferenceIndex {
    entries: FxHashMap<SymbolId, bool>,
    opaque: OpaqueStars,
}

/// Star-imported modules missing from the indexed ones, and the star
/// imports above them. Such a module may bind any name, so recording each
/// referenced name for each of them would grow as their product (#1049):
/// whether a name reaches a referenced module from one is asked instead.
#[derive(Debug, Clone, Default)]
struct OpaqueStars {
    modules: FxHashSet<Box<str>>,
    chain: StarChain<Box<str>, Exports>,
}

impl OpaqueStars {
    /// Whether `target` names an opaque module whose `name` the star imports
    /// carry to a module where `entries` has it referenced. Each such use is
    /// external: an opaque module imports nothing the index read.
    fn reaches_referenced(&self, target: &SymbolId, entries: &FxHashMap<SymbolId, bool>) -> bool {
        self.modules.contains(target.module.as_str())
            && self
                .chain
                .reaches(&target.module, &target.name, true, |module| {
                    entries.contains_key(&SymbolId::new(module, target.name.as_str()))
                })
    }
}

impl ReferenceIndex {
    /// Collect symbol references from `from … import name` statements and
    /// attribute access on module bindings (`import module; module.name`,
    /// `from package import module; module.name`).
    ///
    /// Modules without a module name (e.g. `tests/` without `__init__.py`)
    /// still count as importers: their path never equals a module name, so
    /// every reference they make is external.
    pub(super) fn build(
        modules: &[&ParsedModule],
        module_names: &FxHashMap<&str, String>,
        layout: &LayoutInfo,
    ) -> Self {
        let symlinks = SymlinkModules::new(layout);
        let known_modules: FxHashSet<&str> = module_names.values().map(String::as_str).collect();
        let stars = StarImports::build(modules, module_names);

        let partials = crate::parallel::fold_per_worker(
            modules,
            || (Self::default(), StarCache::default()),
            |(index, cache), _, module| {
                index.add_module(
                    module,
                    module_names,
                    &known_modules,
                    &symlinks,
                    &stars,
                    cache,
                );
            },
        );
        let mut index = Self::merge(partials.into_iter().map(|(index, _)| index).collect());
        index.opaque = OpaqueStars {
            modules: stars.opaque.iter().map(|&module| module.into()).collect(),
            chain: stars.chain.above(&stars.opaque),
        };
        index
    }

    /// Union of indexes built apart by workers. A mark only ORs into its
    /// entry, so the result is order-independent.
    fn merge(mut partials: Vec<Self>) -> Self {
        partials.sort_unstable_by_key(|index| std::cmp::Reverse(index.entries.len()));
        let mut partials = partials.into_iter();
        let mut merged = partials.next().unwrap_or_default();
        for partial in partials {
            for (target, external) in partial.entries {
                *merged.entries.entry(target).or_default() |= external;
            }
        }
        merged
    }

    /// The references in one importing module.
    #[expect(
        clippy::too_many_arguments,
        reason = "takes the per-pass indexes and star-import cache its caller builds once"
    )]
    fn add_module<'m, 'a>(
        &mut self,
        module: &'m ParsedModule,
        module_names: &'m FxHashMap<&str, String>,
        known_modules: &FxHashSet<&'m str>,
        symlinks: &SymlinkModules,
        stars: &StarImports<'a>,
        cache: &mut StarCache<'a>,
    ) {
        let importer = module_names
            .get(module.path.as_str())
            .map_or(module.path.as_str(), String::as_str);
        // A module reads `pkg.name` once or a thousand times, through one
        // import or one per function body: each reference is recorded once.
        let mut names_by_receiver: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
        for access in &module.attribute_accesses {
            names_by_receiver
                .entry(access.receiver.as_str())
                .or_default()
                .insert(access.name.as_str());
        }
        let mut recorded: FxHashSet<(&str, &str)> = FxHashSet::default();
        let mut record = |target_module: &'m str, name: &'m str| {
            if recorded.insert((target_module, name)) {
                // A module named through a symlink is the one at its target.
                let target_module = symlinks.canonical(target_module);
                self.record(importer, SymbolId::new(target_module, name), stars, cache);
            }
        };
        let mut submodule = String::new();
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            match import.kind {
                ImportKind::ImportFrom => {
                    let Some((package, name)) = import.import_target() else {
                        continue;
                    };
                    let package =
                        sibling_module(&module.path, package, known_modules, module_names)
                            .unwrap_or(package);
                    // `from . import x` in the package's own `__init__` defines a re-export, not a use.
                    if import.name.is_some() || package != importer {
                        record(package, name);
                    }
                    let Some(binding) = import.bound_name() else {
                        continue;
                    };
                    submodule.clear();
                    submodule.push_str(package);
                    submodule.push('.');
                    submodule.push_str(name);
                    if let Some(&submodule) = known_modules.get(&*symlinks.canonical(&submodule)) {
                        for &name in names_by_receiver.get(binding).into_iter().flatten() {
                            record(submodule, name);
                        }
                    }
                },
                ImportKind::Import => {
                    let binding = import.alias.as_deref().unwrap_or(&import.module);
                    let target_module =
                        sibling_module(&module.path, &import.module, known_modules, module_names)
                            .unwrap_or(&import.module);
                    for receiver in [import.module.as_str(), binding] {
                        for &name in names_by_receiver.get(receiver).into_iter().flatten() {
                            record(target_module, name);
                        }
                    }
                },
            }
        }
        // `import_module("pkg.m").f` reads `f` from `pkg.m` (#1144).
        for access in &module.loaded_module_accesses {
            record(access.receiver.as_str(), access.name.as_str());
        }
        // `import_string("pkg.mod.Cls")` fetches `mod` from `pkg` and `Cls`
        // from `pkg.mod` (#493).
        for dynamic in module
            .dynamic_imports
            .iter()
            .filter(|dynamic| dynamic.fetched)
        {
            let name = dynamic.module.as_str();
            for (index, _) in name.match_indices('.') {
                let segment = name[index + 1..].split('.').next().unwrap_or_default();
                record(&name[..index], segment);
            }
        }
    }

    /// `pkg.name` also uses `name` in each module `pkg` star-imports it from
    /// (#576); the importer, not `pkg`, decides whether that use is external.
    fn record<'a>(
        &mut self,
        importer: &str,
        target: SymbolId,
        stars: &StarImports<'a>,
        cache: &mut StarCache<'a>,
    ) {
        for &source in stars.sources(&target, cache) {
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
        self.lookup(target).is_some()
    }

    /// Returns `true` when `target` is referenced from a different module.
    pub(super) fn is_externally_referenced(&self, target: &SymbolId) -> bool {
        self.lookup(target) == Some(true)
    }

    /// Whether `target` is referenced, and whether from a different module.
    fn lookup(&self, target: &SymbolId) -> Option<bool> {
        let direct = self.entries.get(target).copied();
        if direct != Some(true) && self.opaque.reaches_referenced(target, &self.entries) {
            return Some(true);
        }
        direct
    }
}

/// `from m import *` edges between named modules.
#[derive(Default)]
struct StarImports<'a> {
    chain: StarChain<&'a str, &'a Exports>,
    /// The modules each module star-imports: [`StarChain::parents`] turned
    /// around.
    children: FxHashMap<&'a str, Vec<&'a str>>,
    /// Star-imported modules that define or import each name. A forwarded
    /// reference can only meet a symbol or a re-export there, so the
    /// packages a name merely passes through are never recorded (#803).
    binders: FxHashMap<&'a str, Vec<&'a str>>,
    /// Star-imported modules missing from the parsed ones, such as reachable
    /// modules left out of the `--production` evidence: they may bind any
    /// name, so [`ReferenceIndex`] asks about them when queried (#1049).
    opaque: Vec<&'a str>,
}

/// [`StarImports::sources`] per symbol, kept by each indexing worker.
type StarCache<'a> = FxHashMap<SymbolId, Vec<&'a str>>;

impl<'a> StarImports<'a> {
    fn build(modules: &[&'a ParsedModule], module_names: &'a FxHashMap<&str, String>) -> Self {
        let mut stars = Self {
            chain: StarChain::build(modules.iter().filter_map(|&module| {
                module_names
                    .get(module.path.as_str())
                    .map(|name| (name.as_str(), module))
            })),
            ..Self::default()
        };
        let mut parsed = FxHashSet::default();
        for module in modules {
            let Some(name) = module_names.get(module.path.as_str()) else {
                continue;
            };
            parsed.insert(name.as_str());
            if !stars.chain.parents.contains_key(name.as_str()) {
                continue;
            }
            for bound in bound_names(module) {
                stars.binders.entry(bound).or_default().push(name.as_str());
            }
        }
        stars.opaque = stars
            .chain
            .parents
            .keys()
            .copied()
            .filter(|module| !parsed.contains(module))
            .collect();
        for (&child, parents) in &stars.chain.parents {
            for &parent in parents {
                stars.children.entry(parent).or_default().push(child);
            }
        }
        stars
    }

    /// The parsed modules whose binding of `target.name` the star imports
    /// carry into `target.module`. Cached per symbol: a module that imports
    /// a package in many function bodies records each `pkg.name` access once
    /// per import.
    ///
    /// The same as [`StarChain::reaches`] from each binder, in two walks
    /// shared by all of them (#810): up from every binder to the modules the
    /// name climbs to, then down from `target.module` through those. Binders
    /// of a common name share their ancestors, which one walk per binder
    /// would climb again each time.
    fn sources<'c>(&self, target: &SymbolId, cache: &'c mut StarCache<'a>) -> &'c [&'a str] {
        if !self.chain.defines.contains_key(target.module.as_str()) {
            return &[];
        }
        if !cache.contains_key(target) {
            let sources = self.climb_down(&target.module, &target.name);
            cache.insert(target.clone(), sources);
        }
        &cache[target]
    }

    fn climb_down(&self, goal: &str, name: &str) -> Vec<&'a str> {
        let binders = self.binders.get(name).map_or(&[][..], Vec::as_slice);
        let mut climbed: FxHashSet<&str> = binders
            .iter()
            .copied()
            .filter(|&binder| self.chain.hands_on(binder, name))
            .collect();
        let mut queue: Vec<&str> = climbed.iter().copied().collect();
        let mut arrives = false;
        while let Some(current) = queue.pop() {
            for &parent in self.chain.parents.get(current).into_iter().flatten() {
                if self.chain.shadows(parent, name) {
                    continue;
                }
                if parent == goal {
                    arrives = true;
                } else if self.chain.hands_on(parent, name) && climbed.insert(parent) {
                    queue.push(parent);
                }
            }
        }
        if !arrives {
            return Vec::new();
        }
        // Every module the walk down meets hands `name` on to `goal`.
        let mut carried = FxHashSet::default();
        let mut queue = vec![goal];
        let mut expanded = FxHashSet::from_iter([goal]);
        while let Some(current) = queue.pop() {
            if self.chain.shadows(current, name) {
                continue;
            }
            for &child in self.children.get(current).into_iter().flatten() {
                if climbed.contains(child) {
                    carried.insert(child);
                    if expanded.insert(child) {
                        queue.push(child);
                    }
                }
            }
        }
        binders
            .iter()
            .copied()
            .filter(|binder| carried.contains(binder))
            .collect()
    }
}

/// The module of the file or package beside `importer_path` that a bare `name` names
/// when no module of the layout has that name: a script's own directory is
/// first on `sys.path` (airflow's `from common_prek_utils import X`, #977).
fn sibling_module<'n>(
    importer_path: &str,
    name: &str,
    known_modules: &FxHashSet<&str>,
    module_names: &'n FxHashMap<&str, String>,
) -> Option<&'n str> {
    if known_modules.contains(name) {
        return None;
    }
    // Source paths are `/`-separated on every platform.
    let mut file = String::with_capacity(importer_path.len() + name.len() + "/__init__.py".len());
    if let Some((dir, _)) = importer_path.rsplit_once('/') {
        file.push_str(dir);
        file.push('/');
    }
    file.extend(name.chars().map(|c| if c == '.' { '/' } else { c }));
    let stem = file.len();
    file.push_str(".py");
    if let Some(module) = module_names.get(file.as_str()) {
        return Some(module);
    }
    file.truncate(stem);
    file.push_str("/__init__.py");
    module_names.get(file.as_str()).map(String::as_str)
}

/// Names `module` binds by definition or `from … import`: every symbol and
/// re-export a reference to `module` can be asked about.
fn bound_names(module: &ParsedModule) -> FxHashSet<&str> {
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
            version_minors: None,
            called_before: Vec::new(),
            tested_before: Vec::new(),
            caller_guarded: false,
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
        assert_eq!(names, FxHashSet::from_iter(["helper", "renamed", "sub"]));
    }

    #[test]
    fn bare_import_of_a_sibling_file_references_its_module() {
        // airflow's `scripts/in_container/run.py` runs as a script and
        // imports `in_container_utils` beside it by its bare name.
        let utils = ParsedModule {
            path: "in_container/in_container_utils.py".to_owned(),
            ..ParsedModule::default()
        };
        let run = ParsedModule {
            path: "in_container/run.py".to_owned(),
            imports: vec![import(
                ImportKind::ImportFrom,
                "in_container_utils",
                Some("run_command"),
                None,
            )],
            ..ParsedModule::default()
        };
        let names = FxHashMap::from_iter([
            (
                utils.path.as_str(),
                "in_container.in_container_utils".to_owned(),
            ),
            (run.path.as_str(), "in_container.run".to_owned()),
        ]);
        let index = ReferenceIndex::build(&[&utils, &run], &names, &LayoutInfo::default());
        assert!(index.is_externally_referenced(&SymbolId::new(
            "in_container.in_container_utils",
            "run_command"
        )));
    }

    #[test]
    fn bare_import_of_a_sibling_package_references_its_init() {
        let helpers = ParsedModule {
            path: "tools/helpers/__init__.py".to_owned(),
            ..ParsedModule::default()
        };
        let run = ParsedModule {
            path: "tools/run.py".to_owned(),
            imports: vec![import(
                ImportKind::ImportFrom,
                "helpers",
                Some("load"),
                None,
            )],
            ..ParsedModule::default()
        };
        let names = FxHashMap::from_iter([
            (helpers.path.as_str(), "tools.helpers".to_owned()),
            (run.path.as_str(), "tools.run".to_owned()),
        ]);
        let index = ReferenceIndex::build(&[&helpers, &run], &names, &LayoutInfo::default());
        assert!(index.is_externally_referenced(&SymbolId::new("tools.helpers", "load")));
    }

    #[test]
    fn opaque_star_targets_are_asked_about_not_recorded_per_name() {
        // `pkg` star-imports modules the index did not read, which may bind
        // any name: one entry per (name, target) pair grows as their
        // product (#1049).
        const TARGETS: usize = 40;
        const NAMES: usize = 40;
        let package = ParsedModule {
            path: "pkg/__init__.py".to_owned(),
            symbols: vec![SymbolDef {
                name: "n0".to_owned(),
                kind: crate::parser::SymbolKind::Function,
                line: 1,
                is_public: true,
                decorators: Vec::new(),
                in_type_checking: false,
                used_in_module: false,
            }],
            imports: (0..TARGETS)
                .map(|index| {
                    import(
                        ImportKind::ImportFrom,
                        &format!("vendor.m{index}"),
                        Some("*"),
                        None,
                    )
                })
                .collect(),
            ..ParsedModule::default()
        };
        let app = ParsedModule {
            path: "app.py".to_owned(),
            imports: (0..NAMES)
                .map(|index| {
                    import(
                        ImportKind::ImportFrom,
                        "pkg",
                        Some(&format!("n{index}")),
                        None,
                    )
                })
                .collect(),
            ..ParsedModule::default()
        };
        let names = FxHashMap::from_iter([
            (package.path.as_str(), "pkg".to_owned()),
            (app.path.as_str(), "app".to_owned()),
        ]);
        let index = ReferenceIndex::build(&[&package, &app], &names, &LayoutInfo::default());
        // One entry per name `app` imports, and one per star import.
        assert_eq!(index.entries.len(), NAMES + TARGETS);
        assert!(index.is_externally_referenced(&SymbolId::new("vendor.m7", "n3")));
        assert!(index.is_referenced(&SymbolId::new("vendor.m39", "n39")));
        // `pkg` defines `n0` itself, which shadows a star-imported one.
        assert!(!index.is_referenced(&SymbolId::new("vendor.m7", "n0")));
        assert!(!index.is_referenced(&SymbolId::new("vendor.m7", "unused")));
    }

    #[test]
    fn index_merges_marks_from_every_worker() {
        // Enough modules to spread over workers. Each `paired` is read by its
        // own module and by the one half the project away, likely on another
        // worker: the external mark must survive the merge in either order.
        const COUNT: usize = 100;
        let modules: Vec<ParsedModule> = (0..COUNT)
            .map(|index| {
                let own = format!("lib.m{index}");
                let across = format!("lib.m{}", (index + COUNT / 2) % COUNT);
                ParsedModule {
                    path: format!("lib/m{index}.py"),
                    imports: vec![
                        import(ImportKind::ImportFrom, &own, Some("solo"), None),
                        import(ImportKind::ImportFrom, &own, Some("paired"), None),
                        import(ImportKind::ImportFrom, &across, Some("paired"), None),
                    ],
                    ..ParsedModule::default()
                }
            })
            .collect();
        let names: FxHashMap<&str, String> = modules
            .iter()
            .enumerate()
            .map(|(index, module)| (module.path.as_str(), format!("lib.m{index}")))
            .collect();
        let refs: Vec<&ParsedModule> = modules.iter().collect();
        let index = ReferenceIndex::build(&refs, &names, &LayoutInfo::default());
        for module in 0..COUNT {
            let name = format!("lib.m{module}");
            let solo = SymbolId::new(&name, "solo");
            assert!(index.is_referenced(&solo) && !index.is_externally_referenced(&solo));
            assert!(
                index.is_externally_referenced(&SymbolId::new(&name, "paired")),
                "{name}"
            );
        }
    }

    #[test]
    fn merge_keeps_an_external_mark_from_any_partial() {
        let index = |entries: &[(&str, bool)]| ReferenceIndex {
            entries: entries
                .iter()
                .map(|&(name, external)| (SymbolId::new("lib", name), external))
                .collect(),
            opaque: OpaqueStars::default(),
        };
        let merged = ReferenceIndex::merge(vec![
            index(&[("shared", true), ("only_small", true)]),
            index(&[("shared", false), ("internal", false), ("external", true)]),
        ]);
        assert_eq!(
            merged.entries,
            index(&[
                ("shared", true),
                ("only_small", true),
                ("internal", false),
                ("external", true),
            ])
            .entries
        );
    }

    mod props {
        use std::collections::{BTreeMap, BTreeSet};

        use proptest::prelude::*;
        use proptest::sample::Index;

        use super::super::super::public::{PublicApi, is_public_module};
        use super::*;
        use crate::parser::SymbolKind;

        const MODULES: usize = 5;
        const NAMES: [&str; 3] = ["alpha", "beta", "_gamma"];
        /// A star target no generated module is: it may bind any name.
        const OPAQUE: &str = "vendored.blob";

        /// A module's `__all__` as the parser reads it (#951).
        #[derive(Debug, Clone)]
        enum All {
            Absent,
            Known(Vec<usize>),
            /// These names and possibly others.
            Partial(Vec<usize>),
            Unknown,
        }

        impl All {
            fn listed(&self) -> &[usize] {
                match self {
                    Self::Known(names) | Self::Partial(names) => names,
                    Self::Absent | Self::Unknown => &[],
                }
            }
        }

        #[derive(Debug, Clone)]
        struct Module {
            /// Defined names, each maybe under `TYPE_CHECKING`.
            symbols: Vec<(usize, bool)>,
            /// Names bound by `from ext import name`.
            imported: Vec<usize>,
            /// Star targets: a module index, or `MODULES` for [`OPAQUE`].
            stars: Vec<usize>,
            all: All,
        }

        fn all() -> impl Strategy<Value = All> {
            let names = || prop::collection::vec(0..NAMES.len(), 0..3);
            prop_oneof![
                Just(All::Absent),
                names().prop_map(All::Known),
                names().prop_map(All::Partial),
                Just(All::Unknown),
            ]
        }

        fn module() -> impl Strategy<Value = Module> {
            let name = 0..NAMES.len();
            (
                prop::collection::vec((name.clone(), any::<bool>()), 0..3),
                prop::collection::vec(name, 0..2),
                prop::collection::vec(0..=MODULES, 0..3),
                all(),
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

        /// Odd modules are private, so a star import from them puts nothing
        /// in the library's API on its own.
        fn module_name(index: usize) -> String {
            match index {
                MODULES => OPAQUE.to_owned(),
                _ if index % 2 == 1 => format!("pkg._m{index}"),
                _ => format!("pkg.m{index}"),
            }
        }

        fn parsed(index: usize, module: &Module) -> ParsedModule {
            let listed = |name: usize| module.all.listed().contains(&name);
            let symbols = module
                .symbols
                .iter()
                .map(|&(name, in_type_checking)| SymbolDef {
                    name: NAMES[name].to_owned(),
                    kind: SymbolKind::Function,
                    line: 1,
                    is_public: !NAMES[name].starts_with('_') || listed(name),
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
            let names =
                |names: &[usize]| names.iter().map(|&name| NAMES[name].to_owned()).collect();
            ParsedModule {
                path: format!("pkg/m{index}.py"),
                symbols,
                imports: imported.chain(stars).collect(),
                exports: match &module.all {
                    All::Absent => Exports::Absent,
                    All::Known(all) => Exports::Known(names(all)),
                    All::Partial(all) => Exports::Partial(names(all)),
                    All::Unknown => Exports::Unknown,
                },
                ..ParsedModule::default()
            }
        }

        type Sources = BTreeMap<(String, &'static str), BTreeSet<String>>;

        /// The modules handed over in `order`, and their names.
        fn project(modules: &[Module], order: &[usize]) -> (Vec<ParsedModule>, Vec<String>) {
            let parsed = order
                .iter()
                .map(|&index| parsed(index, &modules[index]))
                .collect();
            let names = order.iter().map(|&index| module_name(index)).collect();
            (parsed, names)
        }

        /// `StarImports::sources` for every (module, name) pair, with the
        /// modules handed over in `order`.
        fn star_sources(modules: &[Module], order: &[usize]) -> Sources {
            let (parsed, module_names) = project(modules, order);
            let refs: Vec<&ParsedModule> = parsed.iter().collect();
            let names: FxHashMap<&str, String> = parsed
                .iter()
                .map(|module| module.path.as_str())
                .zip(module_names)
                .collect();
            let stars = StarImports::build(&refs, &names);
            let opaque = OpaqueStars {
                modules: stars.opaque.iter().map(|&module| module.into()).collect(),
                chain: stars.chain.above(&stars.opaque),
            };
            let mut cache = StarCache::default();
            let mut sources = Sources::new();
            for index in 0..MODULES {
                for name in NAMES {
                    let target = SymbolId::new(module_name(index), name);
                    let mut found: BTreeSet<String> = stars
                        .sources(&target, &mut cache)
                        .iter()
                        .map(|&source| source.to_owned())
                        .collect();
                    // The index asks about an opaque binder when queried:
                    // with only `target` referenced, `OPAQUE.name` is
                    // referenced exactly when it reaches `target`.
                    let referenced = FxHashMap::from_iter([(target, true)]);
                    if opaque.reaches_referenced(&SymbolId::new(OPAQUE, name), &referenced) {
                        found.insert(OPAQUE.to_owned());
                    }
                    sources.insert((module_name(index), name), found);
                }
            }
            sources
        }

        /// The (name, binder) pairs each module's namespace gets from its
        /// star imports under Python's rules, iterated to a fixed point
        /// (§symbols, #576). A target hands on its own bindings and what it
        /// got itself, filtered by its `__all__`: a known list hands on what
        /// it lists (empty counts as none), a partial or unread one may hand
        /// on anything (#951), and without one the names not starting with
        /// `_` go. With `shadow`, a module's own definitions shadow what it
        /// would get.
        fn namespaces(modules: &[Module], shadow: bool) -> Vec<BTreeSet<(&'static str, usize)>> {
            let hands_on =
                |target: usize, name: &str| match modules.get(target).map(|module| &module.all) {
                    Some(All::Known(all)) if !all.is_empty() => {
                        all.iter().any(|&export| NAMES[export] == name)
                    },
                    Some(All::Partial(_) | All::Unknown) => true,
                    _ => !name.starts_with('_'),
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
                        .filter(|&&(_, in_type_checking)| shadow && !in_type_checking)
                        .map(|&(name, _)| NAMES[name])
                        .collect();
                    for &target in module.stars.iter().filter(|&&target| target != index) {
                        let own = bindings(target).into_iter().map(|name| (name, target));
                        let passed = got.get(target).cloned().unwrap_or_default();
                        let handed: Vec<_> = own
                            .chain(passed)
                            .filter(|&(name, _)| hands_on(target, name) && !defines.contains(name))
                            .collect();
                        let before = got[index].len();
                        got[index].extend(handed);
                        changed |= got[index].len() != before;
                    }
                }
                if !changed {
                    break;
                }
            }
            got
        }

        /// `pkg.name` uses the binder of `name` exactly when the star imports
        /// put `name` from that binder into `pkg`.
        fn reference(modules: &[Module]) -> Sources {
            let mut sources = Sources::new();
            for (index, pairs) in namespaces(modules, true).iter().enumerate() {
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

        /// The registered symbols on which `PublicApi` and the star imports
        /// under Python's rules, without shadowing, disagree: no `__all__`
        /// lists them, and either the star imports put them into a public
        /// module that `PublicApi` leaves out of the library's API, or the
        /// other way round (#960).
        fn mismatched_star_symbols(modules: &[Module]) -> Vec<SymbolId> {
            let order: Vec<_> = (0..modules.len()).collect();
            let (parsed, module_names) = project(modules, &order);
            let refs: Vec<&ParsedModule> = parsed.iter().collect();
            let names: FxHashMap<&str, String> = parsed
                .iter()
                .map(|module| module.path.as_str())
                .zip(module_names)
                .collect();
            let api = PublicApi::build(&refs, &names, ReferenceIndex::default());
            let namespaces = namespaces(modules, false);
            build_registry(&refs, &names)
                .into_iter()
                .filter(|entry| {
                    let index = order
                        .iter()
                        .position(|&index| module_name(index) == entry.id.module)
                        .expect("registered module");
                    let reached = namespaces.iter().enumerate().any(|(public, pairs)| {
                        is_public_module(&module_name(public))
                            && pairs.contains(&(
                                NAMES
                                    .into_iter()
                                    .find(|&known| known == entry.id.name)
                                    .expect("generated name"),
                                index,
                            ))
                    });
                    !entry.in_all && reached != api.exports_symbol(entry)
                })
                .map(|entry| entry.id)
                .collect()
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
                modules[target].all = All::Absent;
                let before = star_sources(&modules, &identity());
                modules[target].all = All::Known(all.iter().copied().collect());
                let after = star_sources(&modules, &identity());
                for (key, sources) in &after {
                    if all.iter().any(|&export| NAMES[export] == key.1) {
                        prop_assert_eq!(sources, &before[key], "{:?}", key);
                    } else {
                        prop_assert!(sources.is_subset(&before[key]), "{:?}", key);
                    }
                }
            }

            /// Losing part of `__all__` only adds names it may hand on.
            #[test]
            fn a_partial_all_hands_on_at_least_what_its_list_does(
                mut modules in modules(),
                target in any::<Index>(),
            ) {
                let target = target.index(MODULES);
                let listed = modules[target].all.listed().to_vec();
                modules[target].all = All::Known(listed.clone());
                let known = star_sources(&modules, &identity());
                modules[target].all = All::Partial(listed);
                let partial = star_sources(&modules, &identity());
                for (key, sources) in &known {
                    prop_assert!(sources.is_subset(&partial[key]), "{:?}", key);
                }
            }

            #[test]
            fn star_symbols_in_a_public_module_are_exactly_the_api(modules in modules()) {
                prop_assert_eq!(mismatched_star_symbols(&modules), Vec::<SymbolId>::new());
            }
        }

        /// Star-import shapes over three modules: a chain, a chain into an
        /// unparsed target, a fork, a diamond, a cycle, and a self import next
        /// to an unparsed target (#951). `star_imports_match_cpython_on_*`
        /// below has one test per shape: under nextest (and so under
        /// cargo-mutants) the slowest single test bounds every run.
        const SHAPES: [[&[usize]; 3]; 6] = [
            [&[1], &[2], &[]],
            [&[1], &[2], &[MODULES]],
            [&[1, 2], &[], &[]],
            [&[1, 2], &[2], &[]],
            [&[1], &[2], &[0]],
            [&[0, 1], &[MODULES], &[1]],
        ];

        /// Every `__all__` state and every set of `alpha` / `_gamma`
        /// definitions on three modules wired as `SHAPES[shape]`.
        fn star_imports_match_cpython_on_every_small_tree(shape: usize) {
            let shape = SHAPES[shape];
            let alls = || {
                [
                    All::Absent,
                    All::Known(Vec::new()),
                    All::Known(vec![0]),
                    All::Known(vec![2]),
                    All::Partial(Vec::new()),
                    All::Partial(vec![0]),
                    All::Unknown,
                ]
            };
            let symbols: [&[(usize, bool)]; 4] =
                [&[], &[(0, false)], &[(2, false)], &[(0, false), (2, false)]];
            let states: Vec<(Vec<(usize, bool)>, All)> = symbols
                .iter()
                .flat_map(|symbols| alls().map(|all| (symbols.to_vec(), all)))
                .collect();
            for a in &states {
                for b in &states {
                    for c in &states {
                        let modules: Vec<_> = [a, b, c]
                            .into_iter()
                            .zip(shape)
                            .map(|((symbols, all), stars)| Module {
                                symbols: symbols.clone(),
                                imported: Vec::new(),
                                stars: stars.to_vec(),
                                all: all.clone(),
                            })
                            .collect();
                        let order = [0, 1, 2];
                        assert_eq!(
                            star_sources(&modules, &order),
                            reference(&modules),
                            "{modules:?}"
                        );
                        assert_eq!(
                            mismatched_star_symbols(&modules),
                            Vec::<SymbolId>::new(),
                            "{modules:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn star_imports_match_cpython_on_a_chain() {
            star_imports_match_cpython_on_every_small_tree(0);
        }

        #[test]
        fn star_imports_match_cpython_on_a_chain_into_an_unparsed_target() {
            star_imports_match_cpython_on_every_small_tree(1);
        }

        #[test]
        fn star_imports_match_cpython_on_a_fork() {
            star_imports_match_cpython_on_every_small_tree(2);
        }

        #[test]
        fn star_imports_match_cpython_on_a_diamond() {
            star_imports_match_cpython_on_every_small_tree(3);
        }

        #[test]
        fn star_imports_match_cpython_on_a_cycle() {
            star_imports_match_cpython_on_every_small_tree(4);
        }

        #[test]
        fn star_imports_match_cpython_on_a_self_import_and_an_unparsed_target() {
            star_imports_match_cpython_on_every_small_tree(5);
        }

        /// A shape added to `SHAPES` needs a test above, or it is never run.
        #[test]
        fn every_star_import_shape_has_a_test() {
            assert_eq!(SHAPES.len(), 6);
        }
    }
}
