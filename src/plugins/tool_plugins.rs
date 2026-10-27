//! Tool plugins named in config: pytest `addopts` (`-p`, plugin options),
//! mypy `plugins`, type-checker configs (ty, pyright, basedpyright, pyrefly), and
//! datamodel-codegen `custom-formatters`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::entry::resolve_module_to_path;
use crate::path_util::{is_file_named, rel_to_root};

use super::commands::{SourceHits, is_dotted_identifier};
use super::config_text::{
    PyprojectDoc, ini_section_line, ini_value, origin_at, read_root_file, toml_key_line, toml_words,
};
use super::context::PluginContext;
use super::plugin_map::pytest_option_distribution;
use super::types::{BinaryUsage, CommandTarget, CommandTargetRef, ReferenceOrigin, ToolConfigRef};

/// (file, section) pairs pytest reads `addopts` from, besides pyproject.
const PYTEST_INI_SECTIONS: [(&str, &str); 3] = [
    ("pytest.ini", "pytest"),
    ("tox.ini", "pytest"),
    ("setup.cfg", "tool:pytest"),
];
const MYPY_INI_FILES: [&str; 3] = ["mypy.ini", ".mypy.ini", "setup.cfg"];
const PYTEST_PYPROJECT_TABLES: [&str; 2] = ["tool.pytest.ini_options", "tool.pytest"];
/// pyright and basedpyright read the same config, so either may be the one in use.
const TYPE_CHECKER_TABLES: [(&str, &[&str]); 4] = [
    ("tool.ty", &["ty"]),
    ("tool.pyrefly", &["pyrefly"]),
    ("tool.pyright", &["pyright", "basedpyright"]),
    ("tool.basedpyright", &["basedpyright"]),
];
const TYPE_CHECKER_FILES: [(&str, &[&str]); 3] = [
    ("ty.toml", &["ty"]),
    ("pyrefly.toml", &["pyrefly"]),
    ("pyrightconfig.json", &["pyright", "basedpyright"]),
];

/// Inputs not already listed by the config scan (`tox.ini` is).
pub(super) fn input_paths(root: &Path) -> Vec<PathBuf> {
    [
        "pytest.ini",
        "setup.cfg",
        "mypy.ini",
        ".mypy.ini",
        "ty.toml",
        "pyrefly.toml",
        "pyrightconfig.json",
    ]
    .into_iter()
    .filter(|name| is_file_named(root, name))
    .map(|name| root.join(name))
    .collect()
}

pub(super) fn scan(root: &Path, pyproject: Option<&PyprojectDoc>) -> SourceHits {
    let mut hits = SourceHits::default();
    if let Some(doc) = pyproject {
        scan_pyproject(doc, &mut hits);
    }
    for (name, section) in PYTEST_INI_SECTIONS {
        if let Some((rel, contents)) = read_root_file(root, name)
            && let Some((index, value)) = ini_value(&contents, section, "addopts")
        {
            let origin = origin_at(&rel, index, &format!("{section}.addopts"));
            push_addopts(&mut hits, &value, &origin);
        }
    }
    for name in MYPY_INI_FILES {
        if let Some((rel, contents)) = read_root_file(root, name) {
            scan_mypy_ini(&rel, &contents, &mut hits);
        }
    }
    for (name, distributions) in TYPE_CHECKER_FILES {
        if is_file_named(root, name) {
            hits.distributions
                .extend(distributions.iter().copied().map(str::to_owned));
        }
    }
    hits
}

/// The config files commands point mypy and pytest at (`mypy --config-file
/// mypy_self_check.ini`, `pytest -c ci.ini`, #1273), read as the tool reads
/// its own: mypy `plugins` and pytest `addopts`, from an INI file's `[mypy]`
/// / `[pytest]` / `[tool:pytest]` section or a TOML file's `[tool.mypy]` /
/// `[tool.pytest.ini_options]` table. The binary itself is a usage where the
/// command runs it. A file that is not a literal path in the project, or
/// cannot be read, is an opaque command target: the plugins it may load are
/// unknown, so the gap stage lowers what they may reach.
pub(super) fn scan_referenced(root: &Path, refs: &[ToolConfigRef]) -> SourceHits {
    let mut hits = SourceHits::default();
    let mut seen = BTreeSet::new();
    for reference in refs {
        if !seen.insert((reference.tool.as_str(), reference.path.as_str())) {
            continue;
        }
        let path = root.join(&reference.path);
        let contents = (!super::commands::is_computed(&reference.path)
            && !Path::new(&reference.path).is_absolute()
            && path.is_file())
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten();
        let Some(contents) = contents else {
            hits.command_targets.push(CommandTargetRef {
                target: CommandTarget::Opaque {
                    text: format!("{} config {}", reference.tool, reference.path),
                },
                origin: reference.origin.clone(),
                deploy: false,
            });
            continue;
        };
        let rel = rel_to_root(root, &path);
        let toml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        match (reference.tool.as_str(), toml) {
            ("mypy", false) => {
                if let Some((index, plugins)) = ini_value(&contents, "mypy", "plugins") {
                    push_mypy_plugins(&mut hits, &plugins, &origin_at(&rel, index, "mypy.plugins"));
                }
            },
            ("pytest", false) => {
                for section in ["pytest", "tool:pytest"] {
                    if let Some((index, value)) = ini_value(&contents, section, "addopts") {
                        let origin = origin_at(&rel, index, &format!("{section}.addopts"));
                        push_addopts(&mut hits, &value, &origin);
                    }
                }
            },
            (tool_name, true) => {
                let Ok(table) = toml::from_str::<toml::Table>(&contents) else {
                    continue;
                };
                let value = |table_path: &str, key: &str| {
                    table_path
                        .split('.')
                        .try_fold(&table, |table, name| table.get(name)?.as_table())
                        .and_then(|table| table.get(key))
                        .and_then(toml_words)
                };
                let origin = |table_path: &str, key: &str| ReferenceOrigin {
                    file: rel.clone(),
                    line: toml_key_line(&contents, table_path, key),
                    label: format!("{table_path}.{key}"),
                };
                if tool_name == "mypy"
                    && let Some(plugins) = value("tool.mypy", "plugins")
                {
                    push_mypy_plugins(&mut hits, &plugins, &origin("tool.mypy", "plugins"));
                }
                if tool_name == "pytest" {
                    for table_path in PYTEST_PYPROJECT_TABLES {
                        if let Some(addopts) = value(table_path, "addopts") {
                            push_addopts(&mut hits, &addopts, &origin(table_path, "addopts"));
                        }
                    }
                }
            },
            _ => {},
        }
    }
    hits
}

fn scan_pyproject(doc: &PyprojectDoc, hits: &mut SourceHits) {
    for table in PYTEST_PYPROJECT_TABLES {
        if let Some(addopts) = doc.value(table, "addopts").and_then(toml_words) {
            push_addopts(hits, &addopts, &doc.origin(table, "addopts"));
        }
    }
    if let Some(plugins) = doc.value("tool.mypy", "plugins").and_then(toml_words) {
        push_mypy_plugins(hits, &plugins, &doc.origin("tool.mypy", "plugins"));
    }
    for (table, distributions) in TYPE_CHECKER_TABLES {
        if doc.table(table).is_some() {
            hits.distributions
                .extend(distributions.iter().copied().map(str::to_owned));
        }
    }
}

/// A `[mypy]` section means mypy is run, like `[tool.mypy]` in pyproject.
fn scan_mypy_ini(rel: &str, contents: &str, hits: &mut SourceHits) {
    let Some(header) = ini_section_line(contents, "mypy") else {
        return;
    };
    hits.binaries.push(BinaryUsage {
        binary: "mypy".to_owned(),
        origin: origin_at(rel, header, "mypy"),
        config_section: true,
        inferred: false,
    });
    if let Some((index, plugins)) = ini_value(contents, "mypy", "plugins") {
        push_mypy_plugins(hits, &plugins, &origin_at(rel, index, "mypy.plugins"));
    }
}

fn push_addopts(hits: &mut SourceHits, addopts: &str, origin: &ReferenceOrigin) {
    let mut words = addopts
        .split_whitespace()
        .map(|word| word.trim_matches(['"', '\'']));
    while let Some(word) = words.next() {
        let plugin = match word.strip_prefix("-p") {
            Some("") => words.next(),
            plugin => plugin,
        };
        match plugin {
            // `-p no:name` disables a plugin rather than loading it.
            Some(module) if module.starts_with("no:") => {},
            Some(module) => hits.push_plugin_module(module, origin),
            None => hits
                .distributions
                .extend(pytest_option_distribution(word).map(str::to_owned)),
        }
    }
}

/// Plugin entries are `module`, `module:function` or a file path; paths name
/// project files rather than installed plugins and are skipped.
fn push_mypy_plugins(hits: &mut SourceHits, plugins: &str, origin: &ReferenceOrigin) {
    for plugin in plugins.split([',', ' ', '\t', '\n']).map(str::trim) {
        if plugin.is_empty()
            || plugin.contains(['/', '\\'])
            || std::path::Path::new(plugin)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
        {
            continue;
        }
        let module = plugin.split_once(':').map_or(plugin, |(module, _)| module);
        hits.push_plugin_module(module, origin);
    }
}

const DATAMODEL_CODEGEN_TABLE: &str = "tool.datamodel-codegen";
/// datamodel-codegen reads its pyproject keys with `-` turned into `_`.
const FORMATTER_KEYS: [&str; 2] = ["custom-formatters", "custom_formatters"];

/// The formatter modules `[tool.datamodel-codegen] custom-formatters` names
/// in the root and each workspace member's `pyproject.toml` (#1063).
/// datamodel-codegen imports each one while it generates code, so a name
/// that resolves through the import roots, or else to the one module of that
/// name under the pyproject's directory (where the tool is run), is a dev
/// command target: an entry, dev unless something else runs it. Anything
/// else is an opaque target the gap stage reports, lowering CHK001: a value
/// that is not an array of module names, or a name matching no file or
/// several. A name matching no project file may be an installed formatter,
/// so it is also a module reference, whose distribution counts as used.
/// Profiles (`[tool.datamodel-codegen.profiles.*]`) are not read.
///
/// Read outside the config scan cache, which keys on root inputs only: the
/// member pyprojects and the inventory decide the result. A pyproject that
/// does not parse fails manifest loading first, which reports it.
pub(super) fn datamodel_codegen_formatters(ctx: &PluginContext<'_>) -> SourceHits {
    let root = ctx.root.path.as_path();
    let known: BTreeSet<String> = ctx
        .sources
        .python_files()
        .map(|file| file.path.clone())
        .collect();
    let dirs: BTreeSet<&str> = std::iter::once("")
        .chain(
            ctx.sources
                .layout
                .members
                .iter()
                .map(|member| member.path.as_str()),
        )
        .collect();
    let mut hits = SourceHits::default();
    for dir in dirs {
        let path = root.join(dir).join("pyproject.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(table) = toml::from_str::<toml::Table>(&text) else {
            continue;
        };
        let Some(codegen) = table
            .get("tool")
            .and_then(|tool| tool.get("datamodel-codegen"))
        else {
            continue;
        };
        for key in FORMATTER_KEYS {
            let Some(value) = codegen.get(key) else {
                continue;
            };
            let origin = ReferenceOrigin {
                file: rel_to_root(root, &path),
                line: toml_key_line(&text, DATAMODEL_CODEGEN_TABLE, key),
                label: format!("{DATAMODEL_CODEGEN_TABLE}.{key}"),
            };
            let names: Vec<Result<&str, String>> = match value.as_array() {
                Some(items) => items
                    .iter()
                    .map(|item| item.as_str().ok_or_else(|| item.to_string()))
                    .collect(),
                None => vec![Err(value.to_string())],
            };
            for name in names {
                let target = match name {
                    Ok(module) if is_dotted_identifier(module) => {
                        let found = formatter_files(ctx, dir, module, &known);
                        if found.is_empty() {
                            hits.push_module(module, &origin);
                        }
                        match found.as_slice() {
                            [path] => CommandTarget::Script {
                                paths: vec![path.clone()],
                            },
                            _ => CommandTarget::Opaque {
                                text: module.to_owned(),
                            },
                        }
                    },
                    Ok(text) => CommandTarget::Opaque {
                        text: text.to_owned(),
                    },
                    Err(text) => CommandTarget::Opaque { text },
                };
                hits.command_targets.push(CommandTargetRef {
                    target,
                    origin: origin.clone(),
                    deploy: false,
                });
            }
        }
    }
    hits
}

/// The files `module` may name: the one it resolves to through the import
/// roots, else every module of that name under `dir`.
fn formatter_files(
    ctx: &PluginContext<'_>,
    dir: &str,
    module: &str,
    known: &BTreeSet<String>,
) -> Vec<String> {
    resolve_module_to_path(module, &ctx.sources.layout, known)
        .map_or_else(|| module_files_under(dir, module, known), |path| vec![path])
}

/// Discovered files under `dir` that are `module`, as a module or a package,
/// at any depth.
fn module_files_under(dir: &str, module: &str, known: &BTreeSet<String>) -> Vec<String> {
    let stem = module.replace('.', "/");
    let names = [format!("/{stem}.py"), format!("/{stem}/__init__.py")];
    known
        .iter()
        .filter(|path| {
            let rel = if dir.is_empty() {
                Some(path.as_str())
            } else {
                path.strip_prefix(dir)
                    .and_then(|rest| rest.strip_prefix('/'))
            };
            rel.is_some_and(|rel| {
                let rel = format!("/{rel}");
                names.iter().any(|name| rel.ends_with(name.as_str()))
            })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> ReferenceOrigin {
        origin_at("pytest.ini", 1, "pytest.addopts")
    }

    fn modules(hits: &SourceHits) -> Vec<&str> {
        hits.module_refs.iter().map(|m| m.module.as_str()).collect()
    }

    #[test]
    fn input_paths_lists_the_files_present_under_their_own_spelling() {
        let temp = tempfile::tempdir().expect("tempdir");
        for name in ["pytest.ini", "ty.toml", "MYPY.INI"] {
            std::fs::write(temp.path().join(name), "").expect("write");
        }
        let paths: Vec<_> = input_paths(temp.path())
            .iter()
            .map(|path| rel_to_root(temp.path(), path))
            .collect();
        assert_eq!(paths, ["pytest.ini", "ty.toml"]);
    }

    #[test]
    fn addopts_reads_plugins_and_plugin_options() {
        let mut hits = SourceHits::default();
        push_addopts(
            &mut hits,
            "--cov=src -n auto -p pytest_django -pxdist -p no:cacheprovider --ds=app.settings -q",
            &origin(),
        );
        assert_eq!(
            hits.distributions,
            [
                "pytest-cov",
                "pytest-xdist",
                "pytest-xdist",
                "pytest-django"
            ]
        );
        assert_eq!(modules(&hits), ["pytest_django"]);
    }

    #[test]
    fn mypy_plugins_skip_paths_and_function_suffixes() {
        let mut hits = SourceHits::default();
        push_mypy_plugins(
            &mut hits,
            "pydantic.mypy,\nmypy_django_plugin.main, custom:plugin ./tools/plugin.py",
            &origin(),
        );
        assert_eq!(hits.distributions, ["django-stubs"]);
        assert_eq!(modules(&hits), ["pydantic.mypy", "custom"]);
    }

    #[test]
    fn module_files_under_match_whole_path_parts_below_dir() {
        let known: BTreeSet<String> = [
            "task-sdk/dev/fmt.py",
            "task-sdk/dev/myfmt.py",
            "task-sdk/pkg/fmt/__init__.py",
            "task-sdk-extra/fmt.py",
            "other/fmt.py",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let under = |dir, module| module_files_under(dir, module, &known);
        assert_eq!(
            under("task-sdk", "fmt"),
            ["task-sdk/dev/fmt.py", "task-sdk/pkg/fmt/__init__.py"]
        );
        assert_eq!(under("task-sdk/dev", "fmt"), ["task-sdk/dev/fmt.py"]);
        assert_eq!(under("task-sdk", "dev.fmt"), ["task-sdk/dev/fmt.py"]);
        assert_eq!(under("", "fmt").len(), 4);
        assert!(under("task-sdk", "missing").is_empty());
    }
}
