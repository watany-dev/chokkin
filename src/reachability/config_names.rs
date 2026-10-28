//! Config files a package ships that name an unreachable module (#1258).
//!
//! A logging handler or a backend users select in their own settings is
//! named only by the package's config template or schema
//! (airflow's `config_templates/config.yml` documents
//! `TaskHandlerWithCustomFormatter`); nothing in the tree imports it. Being
//! named there is not reachability, only a reason to lower CHK001.

use std::collections::HashMap;
use std::path::Path;

use rustc_hash::FxHashSet as HashSet;

use crate::parser::{ParseSummary, ParsedModule, SymbolKind};
use crate::path_util::parent_rel;
use crate::sources::DiscoveredFile;

/// Extensions of the config files read. Docs (`.rst`, `.md`) are left out:
/// release notes name classes long after they stop being used (airflow's
/// `CustomTTYColoredFormatter`), and generated JSON schemas repeat the YAML.
const CONFIG_EXTENSIONS: &[&str] = &["yml", "yaml", "cfg", "ini", "toml"];
const MAX_DEPTH: usize = 16;

/// Config text read per top-level package directory, read once for every
/// unreachable file under it.
pub(super) struct ConfigNames<'a> {
    root: &'a Path,
    packages: HashSet<&'a str>,
    modules: HashMap<&'a str, &'a ParsedModule>,
    texts: HashMap<String, Vec<(String, String)>>,
}

impl<'a> ConfigNames<'a> {
    pub(super) fn new(
        root: &'a Path,
        files: &'a [DiscoveredFile],
        parse: &'a ParseSummary,
    ) -> Self {
        let packages = files
            .iter()
            .filter_map(|file| file.path.strip_suffix("/__init__.py"))
            .collect();
        let modules = parse
            .modules
            .iter()
            .map(|module| (module.path.as_str(), module))
            .collect();
        Self {
            root,
            packages,
            modules,
            texts: HashMap::new(),
        }
    }

    /// Where a config file of the file's top-level package names one of its
    /// public classes or its dotted module path, as ``file:line` names `x``.
    ///
    /// A file outside a package (its directory has no `__init__.py`) or in a
    /// package at the root has no package data to read, and a config file
    /// that is not UTF-8 names nothing: the file stays a warning.
    pub(super) fn mention(&mut self, path: &str) -> Option<String> {
        let top = self.top_package(path)?;
        let dotted = dotted_module(top, path)?;
        let mut names: Vec<&str> = self
            .modules
            .get(path)
            .map(|module| {
                module
                    .symbols
                    .iter()
                    .filter(|def| {
                        def.kind == SymbolKind::Class && def.is_public && !def.in_type_checking
                    })
                    .map(|def| def.name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        names.push(&dotted);
        let root = self.root;
        let texts = self
            .texts
            .entry(top.to_owned())
            .or_insert_with(|| read_configs(root, top));
        texts.iter().find_map(|(file, text)| {
            names.iter().find_map(|name| {
                let at = find_word(text, name)?;
                let line = text[..at].matches('\n').count() + 1;
                Some(format!("`{file}:{line}` names `{name}`"))
            })
        })
    }

    /// The outermost directory of the `__init__.py` chain holding `path`.
    fn top_package(&self, path: &str) -> Option<&'a str> {
        let mut dir = parent_rel(path);
        let mut top = None;
        while !dir.is_empty() {
            let Some(package) = self.packages.get(dir) else {
                break;
            };
            top = Some(*package);
            dir = parent_rel(dir);
        }
        top
    }
}

/// `pkg/sub/mod.py` under top package `pkg` is `pkg.sub.mod`.
fn dotted_module(top: &str, path: &str) -> Option<String> {
    let name = top.rsplit('/').next()?;
    let rest = path.strip_prefix(top)?.strip_prefix('/')?;
    let rest = rest.strip_suffix(".py")?;
    Some(format!("{name}.{}", rest.replace('/', ".")))
}

/// First offset where `name` stands as a whole identifier or dotted path:
/// no identifier character on either side, and no `.` before it unless it
/// is a class name after its module (`pkg.mod.Name`).
fn find_word(text: &str, name: &str) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let dotted = name.contains('.');
    text.match_indices(name).map(|(at, _)| at).find(|&at| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(|c| ident(c) || (dotted && c == '.')) && !after.is_some_and(ident)
    })
}

/// Config files under `top`, sorted by path; hidden directories,
/// `node_modules` and `__pycache__` are skipped.
fn read_configs(root: &Path, top: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk(root, top, 0, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &str, depth: usize, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let rel = format!("{dir}/{name}");
        if kind.is_dir() {
            if depth < MAX_DEPTH
                && !name.starts_with('.')
                && !matches!(name.as_str(), "node_modules" | "__pycache__")
            {
                walk(root, &rel, depth + 1, out);
            }
        } else if kind.is_file()
            && name
                .rsplit_once('.')
                .is_some_and(|(_, ext)| CONFIG_EXTENSIONS.contains(&ext))
            && let Ok(text) = std::fs::read_to_string(entry.path())
        {
            out.push((rel, text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SymbolDef;
    use crate::sources::{FileContext, FileKind};

    fn inventory(paths: &[&str]) -> Vec<DiscoveredFile> {
        paths
            .iter()
            .map(|path| DiscoveredFile {
                path: (*path).to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            })
            .collect()
    }

    fn module(path: &str, classes: &[(&str, bool)]) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            symbols: classes
                .iter()
                .map(|(name, is_public)| SymbolDef {
                    name: (*name).to_owned(),
                    kind: SymbolKind::Class,
                    line: 1,
                    is_public: *is_public,
                    decorators: Vec::new(),
                    in_type_checking: false,
                    used_in_module: false,
                })
                .collect(),
            ..ParsedModule::default()
        }
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn a_config_file_of_the_package_names_a_class_or_the_module() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "src/acme/config_templates/config.yml",
            "logging:\n  description: |\n    use TaskHandlerWithCustomFormatter here\n  other: _Hidden\n",
        );
        write(
            root,
            "src/acme/defaults.cfg",
            "[auth]\nbackend = acme.auth.backend_ldap\n",
        );
        write(root, "docs/notes.rst", "LegacyFormatter acme.log.legacy\n");
        write(root, "src/acme/notes.md", "acme.log.legacy\n");
        let files = inventory(&[
            "src/acme/__init__.py",
            "src/acme/log/__init__.py",
            "src/acme/auth/__init__.py",
            "src/acme/log/handler.py",
            "src/acme/log/legacy.py",
            "src/acme/log/private.py",
            "src/acme/auth/backend_ldap.py",
            "src/acme/auth/backend_ldap_v2.py",
            "scripts/tool.py",
        ]);
        let parse = ParseSummary {
            modules: vec![
                module(
                    "src/acme/log/handler.py",
                    &[("TaskHandlerWithCustomFormatter", true)],
                ),
                module("src/acme/log/legacy.py", &[("LegacyFormatter", true)]),
                module("src/acme/log/private.py", &[("_Hidden", false)]),
            ],
        };
        let mut names = ConfigNames::new(root, &files, &parse);
        assert_eq!(
            names.mention("src/acme/log/handler.py"),
            Some(
                "`src/acme/config_templates/config.yml:3` names `TaskHandlerWithCustomFormatter`"
                    .to_owned()
            )
        );
        assert_eq!(
            names.mention("src/acme/auth/backend_ldap.py"),
            Some("`src/acme/defaults.cfg:2` names `acme.auth.backend_ldap`".to_owned())
        );
        // A longer module name is not this one; docs and Markdown are not
        // config; a private class is nobody's to name; a file outside a
        // package has no package data.
        assert_eq!(names.mention("src/acme/auth/backend_ldap_v2.py"), None);
        assert_eq!(names.mention("src/acme/log/legacy.py"), None);
        assert_eq!(names.mention("src/acme/log/private.py"), None);
        assert_eq!(names.mention("scripts/tool.py"), None);
    }

    #[test]
    fn a_name_counts_only_as_a_whole_identifier() {
        assert_eq!(find_word("x: FooBar", "FooBar"), Some(3));
        assert_eq!(find_word("x: acme.log.FooBar", "FooBar"), Some(12));
        assert_eq!(find_word("x: FooBarBaz FooBar_x", "FooBar"), None);
        assert_eq!(find_word("x: other.acme.log", "acme.log"), None);
        assert_eq!(find_word("x: acme.log.Handler", "acme.log"), Some(3));
        assert_eq!(find_word("x: acme.logging", "acme.log"), None);
    }
}
