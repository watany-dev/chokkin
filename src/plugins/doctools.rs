//! Documentation tool plugin extractors.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ruff_python_ast::{Expr, Stmt, Suite};
use ruff_text_size::{Ranged, TextSize};

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::{assigned_value, parse_module, string_list, string_value};
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::parser::LineIndex;
use crate::path_util::rel_to_root;
use crate::sources::{DiscoveredSources, FileContext, path_to_module};

use super::context::PluginContext;
use super::types::{
    ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin, SysPathImport,
    UnreadSettings,
};
use super::util::{origin_for_file, push_binary};
use super::warnings::PluginsWarning;

/// The root documents `sphinx-build` starts from, one of which marks a
/// `conf.py` beside it as a Sphinx configuration (#814).
const SPHINX_ROOT_DOCUMENTS: [&str; 2] = ["index.rst", "index.md"];

/// Extract static Sphinx and `MkDocs` hints.
#[must_use]
pub(super) fn extract(plugin: PluginId, ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Sphinx => {
            let modules = ConfModules::new(ctx.root.path.as_path());
            for (conf, label) in sphinx_confs(ctx) {
                read_conf(ctx, &conf, &label, &modules, &mut contrib);
            }
        },
        PluginId::MkDocs => extract_mkdocs(ctx.root.path.as_path(), &mut contrib),
        _ => {},
    }

    contrib
}

/// The Sphinx `conf.py` files and the label of each entry: `docs/conf.py` at
/// the root and in each member (`providers/*/docs/conf.py`), then any
/// discovered `conf.py` beside a root document (#814), which `sphinx-build`
/// may be given as its source directory wherever it is
/// (`docs/api_reference/source/conf.py`). A `conf.py` with neither is some
/// other module's configuration (`django/urls/conf.py`) and is left alone.
fn sphinx_confs(ctx: &PluginContext<'_>) -> Vec<(PathBuf, String)> {
    let root = ctx.root.path.as_path();
    let mut confs: Vec<(PathBuf, String)> = Vec::new();
    let members = ctx
        .sources
        .layout
        .members
        .iter()
        .map(|member| root.join(&member.path));
    for dir in std::iter::once(root.to_path_buf()).chain(members) {
        let conf = dir.join("docs").join("conf.py");
        if conf.is_file() && !confs.iter().any(|(known, _)| *known == conf) {
            confs.push((conf, "docs/conf.py".to_owned()));
        }
    }
    for file in ctx.sources.python_files() {
        let path = root.join(&file.path);
        if path.file_name().is_none_or(|name| name != "conf.py")
            || confs.iter().any(|(known, _)| *known == path)
        {
            continue;
        }
        let document = path.parent().and_then(|dir| {
            SPHINX_ROOT_DOCUMENTS
                .into_iter()
                .find(|document| dir.join(document).is_file())
        });
        if let Some(document) = document {
            confs.push((path, format!("conf.py beside {document}")));
        }
    }
    confs
}

/// Record a `conf.py` as a docs entry `sphinx-build` runs, and the
/// extensions and theme it names.
///
/// A top-level `from m import *` of a first-party module runs `m` as part of
/// `conf.py` (airflow's provider `conf.py` and the `provider_conf` they
/// share, #1062), so `m`'s own top-level `extensions` statements count too,
/// one level deep. Their names are referenced from the star import's line in
/// `conf.py` (#830) and resolve with `conf.py`'s `sys.path` hints, which take
/// those of `m`'s imports as well (#779); a name `m` binds resolves through
/// `m`'s own imports. The names merge as a union, so a later
/// `extensions = [...]` in `conf.py` that replaces `m`'s list keeps both.
///
/// What is not read is a gap (ADR 0005): an `extensions` value of a form not
/// read, in either file, and a star import not followed when neither file
/// assigns `extensions` (the unread module may be the one that does).
fn read_conf(
    ctx: &PluginContext<'_>,
    conf: &Path,
    label: &str,
    modules: &ConfModules,
    contrib: &mut PluginContribution,
) {
    let root = ctx.root.path.as_path();
    push_entry(contrib, root, conf, FileContext::Docs, label);
    push_binary(contrib, "sphinx-build", origin_for_file(root, conf, label));
    let Some((contents, stmts)) = read_module(conf) else {
        return;
    };
    let file = rel_to_root(root, conf);
    // Sphinx reads its settings from the namespace, so a `conf.py` inside a
    // package (`devel-common/src/docs/rtd-deprecation/conf.py`) is no API.
    contrib.scanned_files.push(file.clone());
    let module_files = modules.files(ctx.sources);
    let imports = ConfImports::new(&stmts, module_files);
    let lines = LineIndex::new(&contents);
    // Extensions often live in a directory an imported constants module
    // appends to `sys.path` (airflow's `conf_constants`).
    for imported in &imports.files {
        push_sys_path_import(contrib, &file, imported);
    }
    let read = sphinx_extensions(&stmts, &|name| imports.strings(modules, &stmts, name));
    push_unread_extensions(contrib, &file, &lines, &read.unread);
    let mut assigned = read.assigned;
    let mut refs: Vec<(String, TextSize, &str)> = read
        .names
        .into_iter()
        .map(|(module, start)| (module, start, "extensions"))
        .collect();
    let mut unfollowed: Vec<(String, String, u32)> = Vec::new();
    for star in &imports.stars {
        let followed = star
            .file
            .as_ref()
            .and_then(|path| Some((path, modules.parsed(path)?)));
        let Some((path, star_module)) = followed else {
            unfollowed.push((file.clone(), star.module.clone(), lines.line(star.start)));
            continue;
        };
        let (star_contents, star_stmts) = &*star_module;
        let star_imports = ConfImports::new(star_stmts, module_files);
        for imported in &star_imports.files {
            push_sys_path_import(contrib, &file, imported);
        }
        let star_read = sphinx_extensions(star_stmts, &|name| {
            star_imports.strings(modules, star_stmts, name)
        });
        let star_lines = LineIndex::new(star_contents);
        push_unread_extensions(contrib, path, &star_lines, &star_read.unread);
        assigned |= star_read.assigned;
        refs.extend(
            star_read
                .names
                .into_iter()
                .map(|(module, _)| (module, star.start, "extensions")),
        );
        // One level: what the star-imported module star-imports is not read.
        unfollowed.extend(star_imports.stars.iter().map(|nested| {
            (
                path.clone(),
                nested.module.clone(),
                star_lines.line(nested.start),
            )
        }));
    }
    if !assigned {
        for (path, module, line) in unfollowed {
            push_unread(contrib, path, module, line);
        }
    }
    let theme = declared_theme(&stmts, ctx.manifest);
    refs.extend(theme.map(|(module, start)| (module, start, "html_theme")));
    for (module, start, label) in refs {
        contrib.module_refs.push(ModuleReference {
            module,
            origin: ReferenceOrigin {
                file: file.clone(),
                line: Some(lines.line(start)),
                label: label.to_owned(),
            },
        });
    }
}

/// A module's source and statements.
type Module = (String, Suite);

fn read_module(path: &Path) -> Option<Module> {
    let contents = std::fs::read_to_string(path).ok()?;
    let stmts = parse_module(&contents)?;
    Some((contents, stmts))
}

/// The first-party modules the `conf.py` files import, each found and read
/// once: airflow's ~100 provider `conf.py` star-import the same
/// `provider_conf`, which takes its extensions from `conf_constants` (#1025).
struct ConfModules<'a> {
    root: &'a Path,
    files: OnceCell<HashMap<String, String>>,
    parsed: RefCell<HashMap<String, Option<Rc<Module>>>>,
}

impl<'a> ConfModules<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: OnceCell::new(),
            parsed: RefCell::default(),
        }
    }

    fn files(&self, sources: &DiscoveredSources) -> &HashMap<String, String> {
        self.files.get_or_init(|| module_files(sources))
    }

    /// The source and statements of a root-relative file.
    fn parsed(&self, path: &str) -> Option<Rc<Module>> {
        self.parsed
            .borrow_mut()
            .entry(path.to_owned())
            .or_insert_with(|| read_module(&self.root.join(path)).map(Rc::new))
            .clone()
    }
}

/// `conf` takes the `sys.path` hints of `module`, a file it imports (#779).
fn push_sys_path_import(contrib: &mut PluginContribution, conf: &str, module: &str) {
    contrib.sys_path_imports.push(SysPathImport {
        importer: conf.to_owned(),
        imported: module.to_owned(),
    });
}

fn push_unread_extensions(
    contrib: &mut PluginContribution,
    file: &str,
    lines: &LineIndex,
    unread: &[TextSize],
) {
    for start in unread {
        push_unread(
            contrib,
            file.to_owned(),
            "extensions".to_owned(),
            lines.line(*start),
        );
    }
}

/// One warning per gap the Sphinx plugin recorded, so the lowered findings
/// are not the only trace of it (ADR 0005 §3).
pub(super) fn unread_warnings(contrib: &PluginContribution) -> Vec<PluginsWarning> {
    contrib
        .unread_settings
        .iter()
        .filter_map(|unread| {
            let (what, line) = unread.site.as_ref()?;
            Some(PluginsWarning::SphinxExtensionsUnread {
                path: unread.file.clone(),
                line: (*line)?,
                what: what.clone(),
            })
        })
        .collect()
}

/// One gap per site: a module many `conf.py` files star-import is read once
/// for each of them.
fn push_unread(contrib: &mut PluginContribution, file: String, what: String, line: u32) {
    let unread = UnreadSettings {
        file,
        site: Some((what, Some(line))),
    };
    if !contrib.unread_settings.contains(&unread) {
        contrib.unread_settings.push(unread);
    }
}

/// First-party modules a `conf.py` imports at top level.
#[derive(Default)]
struct ConfImports {
    /// Local name → (module file, name in it) of `from m import name [as local]`.
    names: HashMap<String, (String, String)>,
    /// Root-relative files of the imported modules.
    files: Vec<String>,
    /// Top-level `from m import *`, in source order.
    stars: Vec<StarImport>,
}

/// A top-level `from m import *`.
struct StarImport {
    /// Root-relative file of `m` when it is a first-party module imported
    /// absolutely; `None` for one not followed.
    file: Option<String>,
    /// `m` as written, with the leading dots of a relative import.
    module: String,
    /// Offset of the statement.
    start: TextSize,
}

impl ConfImports {
    fn new(stmts: &[Stmt], module_files: &HashMap<String, String>) -> Self {
        let mut imports = Self::default();
        for stmt in stmts {
            match stmt {
                Stmt::ImportFrom(from) => {
                    let file = from
                        .module
                        .as_ref()
                        .filter(|_| from.level == 0)
                        .and_then(|module| module_files.get(module.as_str()));
                    if from.names.iter().any(|alias| alias.name.as_str() == "*") {
                        imports.stars.push(StarImport {
                            file: file.cloned(),
                            module: format!(
                                "{}{}",
                                ".".repeat(from.level as usize),
                                from.module.as_ref().map_or("", |module| module.as_str())
                            ),
                            start: from.start(),
                        });
                    }
                    let Some(file) = file else {
                        continue;
                    };
                    imports.add_file(file);
                    for alias in &from.names {
                        if alias.name.as_str() == "*" {
                            continue;
                        }
                        let local = alias.asname.as_ref().unwrap_or(&alias.name);
                        imports
                            .names
                            .insert(local.to_string(), (file.clone(), alias.name.to_string()));
                    }
                },
                Stmt::Import(import) => {
                    for alias in &import.names {
                        if let Some(file) = module_files.get(alias.name.as_str()) {
                            imports.add_file(file);
                        }
                    }
                },
                _ => {},
            }
        }
        imports
    }

    fn add_file(&mut self, file: &String) {
        if !self.files.contains(file) {
            self.files.push(file.clone());
        }
    }

    /// The strings a bare name in `stmts` binds, `None` when they cannot be
    /// read: the module's own top-level assignment, else the one in the
    /// first-party module it imports the name from
    /// (`BASIC_SPHINX_EXTENSIONS = [...]`, #779), else in a module it
    /// star-imports. One level only: a name that module imports in turn is
    /// not read.
    fn strings(&self, modules: &ConfModules, stmts: &[Stmt], local: &str) -> Option<Vec<String>> {
        if let Some(value) = assigned_value(stmts, local) {
            return constant_strings(value);
        }
        let read = |path: &str, name: &str| {
            let module = modules.parsed(path)?;
            assigned_value(&module.1, name).and_then(constant_strings)
        };
        if let Some((path, name)) = self.names.get(local) {
            return read(path, name);
        }
        self.stars
            .iter()
            .filter_map(|star| star.file.as_deref())
            .find_map(|path| read(path, local))
    }
}

/// The strings of a string or of a list of strings only.
fn constant_strings(value: &Expr) -> Option<Vec<String>> {
    string_value(value).map(|value| vec![value]).or_else(|| {
        string_list(value)
            .filter(|scan| scan.complete)
            .map(|scan| scan.values)
    })
}

/// Module name → root-relative file of the discovered sources, the first
/// file winning.
fn module_files(sources: &DiscoveredSources) -> HashMap<String, String> {
    let mut modules = HashMap::new();
    for file in &sources.files {
        if let Some(module) = path_to_module(&file.path, &sources.layout) {
            modules.entry(module).or_insert_with(|| file.path.clone());
        }
    }
    modules
}

/// `html_theme` when a declared distribution has its name (`furo`,
/// `sphinx_rtd_theme`). A theme is registered by name rather than imported,
/// and Sphinx ships its own (`alabaster`, `classic`), so an undeclared name
/// is no module to resolve (#809).
fn declared_theme(stmts: &[Stmt], manifest: &LoadedManifest) -> Option<(String, TextSize)> {
    let value = assigned_value(stmts, "html_theme")?;
    let theme = string_value(value)?;
    let distribution = normalize_distribution_name(&theme);
    manifest
        .dependencies
        .iter()
        .any(|dep| normalize_distribution_name(&dep.name) == distribution)
        .then_some((theme, value.start()))
}

/// What a module's top-level `extensions` statements name.
#[derive(Debug, Default)]
struct Extensions {
    /// Each name with the offset of the string or bare name that names it.
    names: Vec<(String, TextSize)>,
    /// Offsets of the values not read: neither a string, a list of strings,
    /// nor a bare name bound to one. A partial list keeps the strings it has.
    unread: Vec<TextSize>,
    /// Whether the module assigns `extensions`.
    assigned: bool,
}

/// The names in each top-level `extensions = [...]` (a reassignment
/// `extensions = extensions + [...]` too, which is unread) and
/// `extensions.append(...)`, `.insert(i, ...)`, `.extend([...])` and
/// `+= [...]`. A bare name instead of a literal goes through `constant`
/// (`extensions = BASIC_SPHINX_EXTENSIONS` imported from a constants module,
/// #779) and takes the name's offset, so the reference stays in `conf.py`
/// (#830). Statements under `if` or in a function are not read.
fn sphinx_extensions(stmts: &[Stmt], constant: &dyn Fn(&str) -> Option<Vec<String>>) -> Extensions {
    let mut read = Extensions::default();
    let is_extensions =
        |expr: &Expr| matches!(expr, Expr::Name(name) if name.id.as_str() == "extensions");
    for stmt in stmts {
        match stmt {
            Stmt::Assign(assign) if assign.targets.iter().any(is_extensions) => {
                read.assigned = true;
                literal_strings(&assign.value, constant, &mut read);
            },
            Stmt::AnnAssign(assign) if is_extensions(&assign.target) => {
                if let Some(value) = &assign.value {
                    read.assigned = true;
                    literal_strings(value, constant, &mut read);
                }
            },
            Stmt::Expr(stmt) => {
                let Expr::Call(call) = &*stmt.value else {
                    continue;
                };
                let Expr::Attribute(method) = &*call.func else {
                    continue;
                };
                if !is_extensions(&method.value) {
                    continue;
                }
                let (("append" | "extend", [argument]) | ("insert", [_, argument])) =
                    (method.attr.as_str(), &*call.arguments.args)
                else {
                    continue;
                };
                literal_strings(argument, constant, &mut read);
            },
            Stmt::AugAssign(assign) if is_extensions(&assign.target) => {
                literal_strings(&assign.value, constant, &mut read);
            },
            _ => {},
        }
    }
    read
}

/// The strings of a string or list literal, or of the constant a name binds,
/// with their offsets; a value of another form is unread.
fn literal_strings(
    expr: &Expr,
    constant: &dyn Fn(&str) -> Option<Vec<String>>,
    read: &mut Extensions,
) {
    if let Expr::Name(name) = expr {
        match constant(name.id.as_str()) {
            Some(values) => read
                .names
                .extend(values.into_iter().map(|value| (value, name.start()))),
            None => read.unread.push(name.start()),
        }
    } else if let Some(value) = string_value(expr) {
        read.names.push((value, expr.start()));
    } else if let Some(scan) = string_list(expr) {
        read.names.extend(scan.values.into_iter().zip(scan.starts));
        if !scan.complete {
            read.unread.push(expr.start());
        }
    } else {
        read.unread.push(expr.start());
    }
}

fn extract_mkdocs(root: &Path, contrib: &mut PluginContribution) {
    for name in ["mkdocs.yml", "mkdocs.yaml"] {
        let path = root.join(name);
        if path.is_file() {
            push_binary(contrib, "mkdocs", origin_for_file(root, &path, name));
            return;
        }
    }
}

fn push_entry(
    contrib: &mut PluginContribution,
    root: &Path,
    path: &Path,
    context: FileContext,
    label: &str,
) {
    let rel = rel_to_root(root, path);
    contrib.entries.push(PluginEntry {
        spec: EntrySpec {
            path: rel.clone(),
            symbol: None,
        },
        context,
        origin: ReferenceOrigin {
            file: rel,
            line: None,
            label: label.to_owned(),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> (Vec<(String, u32)>, Vec<u32>, bool) {
        let stmts = parse_module(source).expect("valid python");
        let lines = LineIndex::new(source);
        let read = sphinx_extensions(&stmts, &|name| match name {
            "BASE" => Some(vec!["ext_constant".to_owned()]),
            _ => None,
        });
        (
            read.names
                .into_iter()
                .map(|(module, start)| (module, lines.line(start)))
                .collect(),
            read.unread.iter().map(|start| lines.line(*start)).collect(),
            read.assigned,
        )
    }

    fn extensions(source: &str) -> Vec<String> {
        read(source)
            .0
            .into_iter()
            .map(|(module, _)| module)
            .collect()
    }

    #[test]
    fn literal_and_appended_extensions() {
        assert_eq!(
            extensions(
                "extensions = ['a']\nextensions.append('b')\n\
                 extensions.extend(['c'])\nextensions += ['d']\n\
                 extensions.insert(0, 'e')\n\
                 templates.append('x')\nextensions.remove('a')\n"
            ),
            ["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn a_bare_name_reads_the_constant_it_binds() {
        let (names, unread, assigned) =
            read("extensions = BASE\nextensions.extend(BASE)\nextensions += MISSING\n");
        assert_eq!(
            names,
            [
                ("ext_constant".to_owned(), 1),
                ("ext_constant".to_owned(), 2)
            ]
        );
        assert_eq!(unread, [3]);
        assert!(assigned);
    }

    /// An imported constant's extensions point at the name in `conf.py`, not
    /// at the constants module (#830).
    #[test]
    fn extensions_take_the_line_of_their_string_or_name() {
        assert_eq!(
            read("extensions = [\n    'a',\n    'b',\n]\nextensions += BASE\n").0,
            [
                ("a".to_owned(), 2),
                ("b".to_owned(), 3),
                ("ext_constant".to_owned(), 5),
            ]
        );
    }

    /// A value of a form not read is a gap at its line, and a partial list
    /// keeps the strings it has (ADR 0005, #1062).
    #[test]
    fn values_of_other_forms_are_unread() {
        let (names, unread, assigned) = read(
            "extensions = ['a', EXTRA]\nextensions += BASE + ['b']\n\
             extensions.append(f'ext_{x}')\nextensions.extend(load())\n\
             if x:\n    extensions.append(y)\n",
        );
        assert_eq!(names, [("a".to_owned(), 1)]);
        assert_eq!(unread, [1, 2, 3, 4]);
        assert!(assigned);
        assert!(!read("extensions.append('a')\n").2);
    }

    /// Every top-level assignment is read, not only the first: a
    /// reassignment in a form not read is a gap, not dropped (ADR 0005).
    #[test]
    fn each_assignment_of_extensions_is_read() {
        let (names, unread, assigned) = read(
            "extensions = ['a']\nextensions = extensions + ['b']\n\
             extensions: list[str] = ['c']\n",
        );
        assert_eq!(names, [("a".to_owned(), 1), ("c".to_owned(), 3)]);
        assert_eq!(unread, [2]);
        assert!(assigned);
        assert!(read("extensions: list[str] = ['a']\n").2);
        assert_eq!(
            read("html_theme: str = 'alabaster'\n"),
            (vec![], vec![], false)
        );
    }

    #[test]
    fn conf_imports_keep_first_party_modules_once() {
        let stmts = parse_module(
            "import os\nfrom common.constants import EXTS as BASE, OTHER\n\
             import common.constants\nfrom . import local\nfrom sphinx import addnodes\n\
             from common.constants import *\nfrom theme import *\nfrom .base import *\n",
        )
        .expect("valid python");
        let module_files = HashMap::from([(
            "common.constants".to_owned(),
            "src/common/constants.py".to_owned(),
        )]);
        let imports = ConfImports::new(&stmts, &module_files);
        assert_eq!(imports.files, ["src/common/constants.py"]);
        let file = "src/common/constants.py".to_owned();
        assert_eq!(imports.names["BASE"], (file.clone(), "EXTS".to_owned()));
        assert_eq!(imports.names["OTHER"], (file.clone(), "OTHER".to_owned()));
        assert_eq!(imports.names.len(), 2);
        let stars: Vec<_> = imports
            .stars
            .iter()
            .map(|star| (star.file.clone(), star.module.as_str()))
            .collect();
        assert_eq!(
            stars,
            [
                (Some(file), "common.constants"),
                (None, "theme"),
                (None, ".base"),
            ]
        );
    }

    #[test]
    fn a_local_constant_shadows_an_imported_one() {
        let stmts = parse_module("from c import BASE\nBASE = ['local']\nLIST = [f()]\n")
            .expect("valid python");
        let imports = ConfImports::new(&stmts, &HashMap::new());
        let modules = ConfModules::new(Path::new("/nonexistent"));
        assert_eq!(
            imports.strings(&modules, &stmts, "BASE"),
            Some(vec!["local".to_owned()])
        );
        assert_eq!(imports.strings(&modules, &stmts, "LIST"), None);
        assert_eq!(imports.strings(&modules, &stmts, "MISSING"), None);
    }
}
