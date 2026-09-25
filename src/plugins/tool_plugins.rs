//! Tool plugins named in config: pytest `addopts` (`-p`, plugin options),
//! mypy `plugins`, and type-checker configs (ty, pyright, basedpyright).

use std::path::{Path, PathBuf};

use super::commands::SourceHits;
use super::config_text::{
    PyprojectDoc, ini_section_line, ini_value, origin_at, read_root_file, toml_words,
};
use super::plugin_map::pytest_option_distribution;
use super::types::{BinaryUsage, ReferenceOrigin};

/// (file, section) pairs pytest reads `addopts` from, besides pyproject.
const PYTEST_INI_SECTIONS: [(&str, &str); 3] = [
    ("pytest.ini", "pytest"),
    ("tox.ini", "pytest"),
    ("setup.cfg", "tool:pytest"),
];
const MYPY_INI_FILES: [&str; 3] = ["mypy.ini", ".mypy.ini", "setup.cfg"];
const PYTEST_PYPROJECT_TABLES: [&str; 2] = ["tool.pytest.ini_options", "tool.pytest"];
/// pyright and basedpyright read the same config, so either may be the one in use.
const TYPE_CHECKER_TABLES: [(&str, &[&str]); 3] = [
    ("tool.ty", &["ty"]),
    ("tool.pyright", &["pyright", "basedpyright"]),
    ("tool.basedpyright", &["basedpyright"]),
];
const TYPE_CHECKER_FILES: [(&str, &[&str]); 2] = [
    ("ty.toml", &["ty"]),
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
        "pyrightconfig.json",
    ]
    .into_iter()
    .map(|name| root.join(name))
    .filter(|path| path.is_file())
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
        if root.join(name).is_file() {
            hits.distributions
                .extend(distributions.iter().copied().map(str::to_owned));
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
        if plugin.is_empty() || plugin.contains(['/', '\\']) || plugin.ends_with(".py") {
            continue;
        }
        let module = plugin.split_once(':').map_or(plugin, |(module, _)| module);
        hits.push_plugin_module(module, origin);
    }
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
}
