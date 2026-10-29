//! Property-based tests for CHK006 / CHK007 (§12) end to end, beyond the
//! star-import propagation `rules/symbols/graph.rs` already checks.
//!
//! The reference model is written over name sets: a public name a plain
//! module defines is CHK006 unless another module imports it (`from m import
//! name`, `import m; m.name`, or a package re-export), or the module itself
//! reads it while `__all__` does not list it; a public `__init__.py`
//! re-export is CHK007 unless some module imports it from the package. A
//! module tools read by convention (`*_pb2.py`, `modular_x.py` beside
//! `modeling_x.py`) is never reported, yet still refers to others.
//!
//! The uses #1238 / #1223 added are in the model too: another module's string
//! spelling the name's dotted path (`"pkg.m.name"` or `"pkg.m:name"`), a
//! registry decorator on the definition (`@atexit.register`, or a module
//! function that stores its argument and returns it), and an upper-case name
//! of the module Flask's `config.from_object` loads.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

const MODULES: usize = 3;
/// The importer index of `main.py`, the entry that reaches every module.
const MAIN: usize = MODULES;
const NAMES: [&str; 5] = ["alpha", "beta", "gamma", "_delta", "OMEGA"];

/// What a module's file name tells the tools that read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    /// `m<i>_pb2.py`, a protoc output.
    Protobuf,
    /// `modular_m<i>.py` beside `modeling_m<i>.py`, a transformers codegen source.
    Modular,
}

#[derive(Debug, Clone)]
struct Module {
    kind: Kind,
    /// Index into [`NAMES`] of each name defined at top level.
    defined: BTreeSet<usize>,
    /// A literal `__all__`, when there is one.
    all: Option<BTreeSet<usize>>,
    all_form: AllForm,
    /// Defined names the module reads itself.
    read: BTreeSet<usize>,
    /// Defined names written under `decorator`.
    decorated: BTreeSet<usize>,
    decorator: Decorator,
}

/// What decorates [`Module::decorated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decorator {
    /// `@atexit.register`, a registry by its name.
    Atexit,
    /// `@_keep`, a module function that appends its argument to a list and
    /// returns it: a first-party registry (#1223).
    Keep,
    /// `@_wrap`, a module function that only returns its argument: no
    /// registry, so the name stays reported.
    Wrap,
}

impl Decorator {
    const fn registers(self) -> bool {
        matches!(self, Self::Atexit | Self::Keep)
    }
}

impl Module {
    fn listed(&self, name: usize) -> bool {
        self.all.as_ref().is_some_and(|all| all.contains(&name))
    }
}

/// How `__all__` is written: each form `src/parser/exports.rs` reads in full.
#[derive(Debug, Clone, Copy)]
enum AllForm {
    Literal,
    /// The first name, then `+=` the rest.
    Augmented,
    /// An annotated empty list, then `.append` each name.
    Appended,
    /// The first name, then `.extend` a tuple of the rest.
    Extended,
    /// The first name, then `+=` the rest in an `if` without `else`: the
    /// paths join to the union.
    Branched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `from pkg.m import name as alias`.
    From,
    /// `import pkg.m`, then `pkg.m.name` in a function.
    Attribute,
    /// `from pkg import name as alias`.
    Package,
    /// `import pkg`, then `pkg.name` in a function.
    PackageAttribute,
    /// `"pkg.m.name"` in a function, a dotted path a loader may import.
    Dotted,
    /// `"pkg.m:name"` in a function.
    Colon,
}

#[derive(Debug, Clone, Copy)]
struct Use {
    /// A module index, or [`MAIN`].
    importer: usize,
    target: usize,
    name: usize,
    form: Form,
}

#[derive(Debug, Clone)]
struct Spec {
    modules: Vec<Module>,
    uses: Vec<Use>,
    /// `from .m import name` in `pkg/__init__.py`, one per name.
    reexports: Vec<(usize, usize)>,
    /// The module `main.py` hands Flask's `config.from_object`, with `flask`
    /// declared.
    settings: Option<usize>,
}

fn names() -> impl Strategy<Value = BTreeSet<usize>> {
    prop::collection::btree_set(0..NAMES.len(), 0..=NAMES.len())
}

fn module_strategy() -> impl Strategy<Value = Module> {
    (
        prop_oneof![
            6 => Just(Kind::Plain),
            1 => Just(Kind::Protobuf),
            1 => Just(Kind::Modular),
        ],
        names(),
        prop::option::of(names()),
        prop_oneof![
            Just(AllForm::Literal),
            Just(AllForm::Augmented),
            Just(AllForm::Appended),
            Just(AllForm::Extended),
            Just(AllForm::Branched),
        ],
        names(),
        names(),
        prop_oneof![
            Just(Decorator::Atexit),
            Just(Decorator::Keep),
            Just(Decorator::Wrap),
        ],
    )
        .prop_map(
            |(kind, defined, all, all_form, read, decorated, decorator)| {
                let read = read.intersection(&defined).copied().collect();
                let decorated = decorated.intersection(&defined).copied().collect();
                Module {
                    kind,
                    defined,
                    all,
                    all_form,
                    read,
                    decorated,
                    decorator,
                }
            },
        )
}

fn use_strategy() -> impl Strategy<Value = Use> {
    (
        0..=MAIN,
        0..MODULES,
        0..NAMES.len(),
        prop_oneof![
            Just(Form::From),
            Just(Form::Attribute),
            Just(Form::Package),
            Just(Form::PackageAttribute),
            Just(Form::Dotted),
            Just(Form::Colon),
        ],
    )
        .prop_map(|(importer, target, name, form)| Use {
            importer,
            target,
            name,
            form,
        })
        // A module importing its own name is no outside caller, and would
        // rebind the name it defines. Its own string is kept: it counts for
        // nothing.
        .prop_filter("imports from another module", |item| {
            item.importer != item.target
                || matches!(
                    item.form,
                    Form::Package | Form::PackageAttribute | Form::Dotted | Form::Colon
                )
        })
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (
        prop::collection::vec(module_strategy(), MODULES),
        prop::collection::vec(use_strategy(), 0..8),
        prop::collection::vec((0..MODULES, 0..NAMES.len()), 0..4),
        prop::option::of(0..MODULES),
    )
        .prop_map(|(modules, uses, reexports, settings)| {
            let mut seen = BTreeSet::new();
            let reexports = reexports
                .into_iter()
                .filter(|&(_, name)| seen.insert(name))
                .collect();
            Spec {
                modules,
                uses,
                reexports,
                settings,
            }
        })
}

fn module_name(index: usize, kind: Kind) -> String {
    match kind {
        Kind::Plain => format!("m{index}"),
        Kind::Protobuf => format!("m{index}_pb2"),
        Kind::Modular => format!("modular_m{index}"),
    }
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn module_names(spec: &Spec) -> Vec<String> {
    spec.modules
        .iter()
        .enumerate()
        .map(|(index, module)| module_name(index, module.kind))
        .collect()
}

/// The source of importer `importer`: its imports, then `body`. `reversed`
/// writes every list of statements in the opposite order.
fn render_importer(spec: &Spec, importer: usize, body: &[String], reversed: bool) -> String {
    let names = module_names(spec);
    let mut imports = Vec::new();
    let mut functions = Vec::new();
    if importer == MAIN {
        imports.extend(names.iter().map(|name| format!("import pkg.{name}")));
    }
    for (index, item) in spec.uses.iter().enumerate() {
        if item.importer != importer {
            continue;
        }
        let (module, name) = (&names[item.target], NAMES[item.name]);
        match item.form {
            Form::From => imports.push(format!("from pkg.{module} import {name} as _u{index}")),
            Form::Attribute => {
                imports.push(format!("import pkg.{module}"));
                functions.push(format!(
                    "def _use{index}():\n    return pkg.{module}.{name}"
                ));
            },
            Form::Package => imports.push(format!("from pkg import {name} as _u{index}")),
            Form::PackageAttribute => {
                imports.push("import pkg".to_owned());
                functions.push(format!("def _use{index}():\n    return pkg.{name}"));
            },
            Form::Dotted => functions.push(format!(
                "def _use{index}():\n    return \"pkg.{module}.{name}\""
            )),
            Form::Colon => functions.push(format!(
                "def _use{index}():\n    return \"pkg.{module}:{name}\""
            )),
        }
    }
    let mut parts: Vec<String> = imports;
    parts.extend(body.iter().cloned());
    parts.extend(functions);
    if reversed {
        parts.reverse();
    }
    let mut source = parts.join("\n\n\n");
    source.push('\n');
    source
}

fn render_all(all: &BTreeSet<usize>, form: AllForm) -> String {
    let quoted: Vec<String> = all
        .iter()
        .map(|&name| format!("\"{}\"", NAMES[name]))
        .collect();
    let (first, rest) = quoted.split_at(quoted.len().min(1));
    let (first, rest) = (first.join(""), rest.join(", "));
    match form {
        AllForm::Literal => format!("__all__ = [{}]", quoted.join(", ")),
        AllForm::Augmented => format!("__all__ = [{first}]\n__all__ += [{rest}]"),
        AllForm::Appended => {
            let mut text = "__all__: list[str] = []".to_owned();
            for name in &quoted {
                write!(text, "\n__all__.append({name})").expect("fmt");
            }
            text
        },
        AllForm::Extended if rest.is_empty() => format!("__all__ = [{first}]\n__all__.extend(())"),
        AllForm::Extended => format!("__all__ = [{first}]\n__all__.extend(({rest},))"),
        AllForm::Branched => format!(
            "import sys\n__all__ = [{first}]\nif sys.platform == \"win32\":\n    __all__ += [{rest}]"
        ),
    }
}

fn render_module(spec: &Spec, index: usize, reversed: bool) -> String {
    let module = &spec.modules[index];
    let mut body = Vec::new();
    if let Some(all) = &module.all {
        // One statement block, so `reversed` keeps its order.
        body.push(render_all(all, module.all_form));
    }
    if !module.decorated.is_empty() {
        body.push(
            match module.decorator {
                Decorator::Atexit => "import atexit",
                Decorator::Keep => {
                    "_REGISTRY = []\n\n\ndef _keep(target):\n    _REGISTRY.append(target)\n    return target"
                },
                Decorator::Wrap => "def _wrap(target):\n    return target",
            }
            .to_owned(),
        );
    }
    let decorator = match module.decorator {
        Decorator::Atexit => "@atexit.register\n",
        Decorator::Keep => "@_keep\n",
        Decorator::Wrap => "@_wrap\n",
    };
    let mut defined: Vec<usize> = module.defined.iter().copied().collect();
    if reversed {
        defined.reverse();
    }
    body.extend(defined.iter().map(|&name| {
        let decorator = if module.decorated.contains(&name) {
            decorator
        } else {
            ""
        };
        format!("{decorator}def {}():\n    pass", NAMES[name])
    }));
    if !module.read.is_empty() {
        let mut read = String::new();
        for &name in &module.read {
            write!(read, "{}, ", NAMES[name]).expect("fmt");
        }
        body.push(format!("def _reader():\n    return ({read})"));
    }
    render_importer(spec, index, &body, reversed)
}

fn project(spec: &Spec, reversed: bool) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let dependencies = if spec.settings.is_some() {
        "[\"flask\"]"
    } else {
        "[]"
    };
    write(
        root,
        "pyproject.toml",
        &format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = {dependencies}\n\n\
             [project.scripts]\npkg = \"pkg.main:run\"\n\n[tool.chokkin]\nmode = \"app\"\n"
        ),
    );
    let names = module_names(spec);
    let mut init: Vec<String> = spec
        .reexports
        .iter()
        .map(|&(target, name)| format!("from .{} import {}", names[target], NAMES[name]))
        .collect();
    if reversed {
        init.reverse();
    }
    write(root, "src/pkg/__init__.py", &(init.join("\n") + "\n"));
    for (index, module) in spec.modules.iter().enumerate() {
        write(
            root,
            &format!("src/pkg/{}.py", names[index]),
            &render_module(spec, index, reversed),
        );
        if module.kind == Kind::Modular {
            write(root, &format!("src/pkg/modeling_m{index}.py"), "");
        }
    }
    let run = spec.settings.map_or_else(
        || "def run():\n    pass".to_owned(),
        |index| {
            format!(
                "def run():\n    import flask\n\n    \
                 flask.Flask(__name__).config.from_object(\"pkg.{}\")",
                names[index]
            )
        },
    );
    let main = render_importer(spec, MAIN, &[run], reversed);
    write(root, "src/pkg/main.py", &main);
    dir
}

/// `(code, symbol, severity, confidence)`.
type Key = (String, String, String, String);

fn key(code: &str, symbol: String, severity: &str, confidence: &str) -> Key {
    (
        code.to_owned(),
        symbol,
        severity.to_owned(),
        confidence.to_owned(),
    )
}

fn found(spec: &Spec, reversed: bool) -> BTreeSet<Key> {
    let dir = project(spec, reversed);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache"])
        .arg(dir.path())
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json reporter output");
    parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|issue| matches!(issue["code"].as_str(), Some("CHK006" | "CHK007")))
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (
                field("code"),
                field("symbol"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect()
}

fn reference(spec: &Spec) -> BTreeSet<Key> {
    let names = module_names(spec);
    let mut expected = BTreeSet::new();
    for (index, module) in spec.modules.iter().enumerate() {
        if module.kind != Kind::Plain {
            continue;
        }
        for &name in &module.defined {
            let listed = module.listed(name);
            if NAMES[name].starts_with('_') && !listed {
                continue;
            }
            let imported = spec.uses.iter().any(|item| {
                item.target == index
                    && item.name == name
                    && match item.form {
                        Form::From | Form::Attribute => true,
                        Form::Dotted | Form::Colon => item.importer != index,
                        Form::Package | Form::PackageAttribute => false,
                    }
            }) || spec.reexports.contains(&(index, name));
            let registered = module.decorated.contains(&name) && module.decorator.registers();
            let setting = spec.settings == Some(index) && NAMES[name] == "OMEGA";
            if imported || registered || setting || (module.read.contains(&name) && !listed) {
                continue;
            }
            let confidence = if listed { "certain" } else { "likely" };
            expected.insert(key(
                "CHK006",
                format!("pkg.{}:{}", names[index], NAMES[name]),
                "warning",
                confidence,
            ));
        }
    }
    for &(_, name) in &spec.reexports {
        let imported = spec.uses.iter().any(|item| {
            item.name == name && matches!(item.form, Form::Package | Form::PackageAttribute)
        });
        if NAMES[name].starts_with('_') || imported {
            continue;
        }
        expected.insert(key(
            "CHK007",
            format!("pkg:{}", NAMES[name]),
            "warning",
            "likely",
        ));
    }
    expected
}

/// `spec` with module `i` moved to index `(i + shift) % MODULES`.
fn rotated(spec: &Spec, shift: usize) -> Spec {
    let at = |index: usize| (index + shift) % MODULES;
    let mut modules = spec.modules.clone();
    for (index, module) in spec.modules.iter().enumerate() {
        modules[at(index)] = module.clone();
    }
    let uses = spec
        .uses
        .iter()
        .map(|item| Use {
            importer: if item.importer == MAIN {
                MAIN
            } else {
                at(item.importer)
            },
            target: at(item.target),
            ..*item
        })
        .collect();
    let reexports = spec
        .reexports
        .iter()
        .map(|&(target, name)| (at(target), name))
        .collect();
    Spec {
        modules,
        uses,
        reexports,
        settings: spec.settings.map(at),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// CHK006 / CHK007 match the name-set model, and a module read by
    /// convention is never reported.
    #[test]
    fn unused_symbols_match_reference_model(spec in spec_strategy()) {
        prop_assert_eq!(found(&spec, false), reference(&spec));
    }

    /// Importing a name more never adds a report.
    #[test]
    fn more_uses_never_add_reports(spec in spec_strategy(), extra in use_strategy()) {
        let mut wider = spec.clone();
        wider.uses.push(extra);
        let (before, after) = (found(&spec, false), found(&wider, false));
        prop_assert!(after.is_subset(&before), "{:?} not in {:?}", after, before);
    }

    /// Renumbering the modules and writing every statement list in reverse
    /// renames the reports and changes nothing else.
    #[test]
    fn reports_ignore_module_and_statement_order(
        spec in spec_strategy(),
        shift in 1..MODULES,
    ) {
        let moved = rotated(&spec, shift);
        let (old, new) = (module_names(&spec), module_names(&moved));
        let renamed: BTreeSet<Key> = found(&spec, false)
            .into_iter()
            .map(|(code, symbol, severity, confidence)| {
                let symbol = old
                    .iter()
                    .position(|name| symbol.starts_with(&format!("pkg.{name}:")))
                    .map_or_else(
                        || symbol.clone(),
                        |index| {
                            let target = new[(index + shift) % MODULES].as_str();
                            symbol.replacen(&old[index], target, 1)
                        },
                    );
                (code, symbol, severity, confidence)
            })
            .collect();
        prop_assert_eq!(found(&moved, true), renamed);
    }
}
