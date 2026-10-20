//! Entry-point declarations: `[project]`, Poetry, setup.cfg and setup.py.
//!
//! Every form reads into [`EntryPointDecl`] with the `[project]` group
//! names: setuptools' `console_scripts` / `gui_scripts` groups are the
//! `console` / `gui` scripts (#916). A value none of the forms describes is
//! reported as [`ManifestWarning::EntryPointUnreadable`] and roots nothing.

use toml::Value as Toml;

use super::setup_py_eval::Value;
use super::types::{DependencyOrigin, EntryPointDecl};
use super::warnings::ManifestWarning;

/// Entry points and the warnings for the ones that could not be read, all
/// from one manifest file.
pub(super) struct EntryPoints<'a> {
    pub rel: &'a str,
    pub entries: &'a mut Vec<EntryPointDecl>,
    pub warnings: &'a mut Vec<ManifestWarning>,
}

fn project_group(group: &str) -> &str {
    match group {
        "console_scripts" => "console",
        "gui_scripts" => "gui",
        other => other,
    }
}

impl EntryPoints<'_> {
    fn push(&mut self, name: &str, target: &str, group: &str, label: String) {
        self.entries.push(EntryPointDecl {
            name: name.to_owned(),
            target: object_reference(target),
            group: project_group(group).to_owned(),
            origin: DependencyOrigin {
                file: self.rel.to_owned(),
                line: None,
                label,
            },
        });
    }

    fn skip(&mut self, label: impl Into<String>, reason: &str) {
        self.warnings.push(ManifestWarning::EntryPointUnreadable {
            file: self.rel.to_owned(),
            label: label.into(),
            reason: reason.to_owned(),
        });
    }

    pub(super) fn read_project(&mut self, project: &toml::Table) {
        for (key, group) in [("scripts", "console"), ("gui-scripts", "gui")] {
            if let Some(scripts) = project.get(key) {
                self.read_toml_group(scripts, group, &format!("project.{key}"), false);
            }
        }
        if let Some(groups) = project.get("entry-points") {
            self.read_toml_groups(groups, "project.entry-points");
        }
    }

    pub(super) fn read_poetry(&mut self, poetry: &toml::Table) {
        if let Some(scripts) = poetry.get("scripts") {
            self.read_toml_group(scripts, "console", "tool.poetry.scripts", true);
        }
        if let Some(groups) = poetry.get("plugins") {
            self.read_toml_groups(groups, "tool.poetry.plugins");
        }
    }

    fn read_toml_groups(&mut self, groups: &Toml, label: &str) {
        let Some(groups) = groups.as_table() else {
            return self.skip(label, "not a table");
        };
        for (group, entries) in groups {
            self.read_toml_group(entries, group, &format!("{label}.{group}"), false);
        }
    }

    /// `name = "module:attr"` entries; with `poetry_scripts`, also Poetry's
    /// `{ reference = "...", type = "console" }` and legacy
    /// `{ callable = "..." }` tables.
    fn read_toml_group(&mut self, entries: &Toml, group: &str, label: &str, poetry_scripts: bool) {
        let Some(entries) = entries.as_table() else {
            return self.skip(label, "not a table");
        };
        for (name, target) in entries {
            let label = format!("{label}.{name}");
            match target {
                Toml::String(target) => self.push(name, target, group, label),
                Toml::Table(script) if poetry_scripts => match poetry_script(script) {
                    Ok(target) => self.push(name, target, group, label),
                    Err(reason) => self.skip(label, reason),
                },
                _ if poetry_scripts => self.skip(label, "not a string or script table"),
                _ => self.skip(label, "not a string"),
            }
        }
    }

    /// `[options.entry_points]`: each option is a group whose value lists
    /// `name = target` items as setuptools' `_parse_list` splits them, by
    /// line, or by comma when the value is one line. A comma inside
    /// `[extras]` does not split: the ini reader drops the newline before a
    /// lone continuation line, which setuptools splits by line, and on the
    /// key's own line such an item would not build.
    pub(super) fn read_setup_cfg_group(&mut self, group: &str, value: &str) {
        let label = format!("options.entry_points.{group}");
        if group.is_empty() {
            return self.skip(label, "no group name");
        }
        if value.contains('\n') {
            for item in value.lines() {
                self.read_item(group, &label, item);
            }
            return;
        }
        let mut depth = 0_usize;
        for item in value.split(|c| {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                _ => {},
            }
            c == ',' && depth == 0
        }) {
            self.read_item(group, &label, item);
        }
    }

    /// `[options] entry_points = ...`, a `file:` or an inline ini string,
    /// which chokkin does not read.
    pub(super) fn skip_setup_cfg_option(&mut self) {
        self.skip(
            "options.entry_points",
            "only the `[options.entry_points]` section is read",
        );
    }

    /// `setup(entry_points=...)`: a dict of groups, each a string of lines
    /// or a list of strings, or one string of `[group]` sections, as
    /// setuptools takes them.
    pub(super) fn read_setup_py(&mut self, value: &Value) {
        match value {
            Value::Dict { items, partial, .. } => {
                for (group, entries) in items {
                    let label = format!("entry_points.{group}");
                    match entries {
                        Value::Str(text) => self.read_lines(group, &label, text),
                        Value::List(list, _) => {
                            for (index, item) in list.iter().enumerate() {
                                match item {
                                    Value::Str(text) => self.read_lines(group, &label, text),
                                    _ => self.skip(format!("{label}[{index}]"), "not a string"),
                                }
                            }
                        },
                        _ => self.skip(label, "not a string or a list of strings"),
                    }
                }
                if *partial {
                    self.skip("entry_points", "some groups could not be read statically");
                }
            },
            Value::Str(text) => self.read_sections(text),
            _ => self.skip("entry_points", "not a dict or string read statically"),
        }
    }

    /// `[group]` headers, each followed by `name = target` lines.
    fn read_sections(&mut self, text: &str) {
        let mut group: Option<&str> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(name) = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                group = Some(name.trim());
                continue;
            }
            match group {
                Some(group) => self.read_item(group, &format!("entry_points.{group}"), trimmed),
                None => self.skip("entry_points", "an entry before any `[group]` header"),
            }
        }
    }

    fn read_lines(&mut self, group: &str, label: &str, text: &str) {
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') {
                self.read_item(group, label, trimmed);
            }
        }
    }

    /// One `name = target` item; a blank item is nothing.
    fn read_item(&mut self, group: &str, label: &str, item: &str) {
        let item = item.trim();
        if item.is_empty() {
            return;
        }
        match item.split_once('=') {
            Some((name, target)) if !name.trim().is_empty() && !target.trim().is_empty() => {
                let name = name.trim();
                self.push(name, target.trim(), group, format!("{label}.{name}"));
            },
            _ => self.skip(
                label,
                &format!("`{item}` is not a `name = module:attr` item"),
            ),
        }
    }
}

/// `module:attr` as the entry-points specification reads it: spaces
/// around the colon and a trailing `[extras]` are not part of it.
fn object_reference(target: &str) -> String {
    let target = target.trim();
    let target = match target.rsplit_once('[') {
        Some((reference, extras)) if extras.trim_end().ends_with(']') => reference.trim_end(),
        _ => target,
    };
    match target.split_once(':') {
        Some((module, attr)) => format!("{}:{}", module.trim_end(), attr.trim_start()),
        None => target.to_owned(),
    }
}

fn poetry_script(script: &toml::Table) -> Result<&str, &'static str> {
    match script.get("type").map(Toml::as_str) {
        None | Some(Some("console")) => {},
        Some(Some("file")) => return Err("a file script is installed as is, not imported"),
        Some(_) => return Err("not a `console` script"),
    }
    script
        .get("reference")
        .or_else(|| script.get("callable"))
        .and_then(Toml::as_str)
        .ok_or("no `reference` string")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(group, name, target, label)`.
    type Entry = (String, String, String, String);

    fn read(fill: impl FnOnce(&mut EntryPoints<'_>)) -> (Vec<Entry>, Vec<String>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        fill(&mut EntryPoints {
            rel: "f",
            entries: &mut entries,
            warnings: &mut warnings,
        });
        let entries = entries
            .into_iter()
            .map(|entry| (entry.group, entry.name, entry.target, entry.origin.label))
            .collect();
        let warnings = warnings
            .into_iter()
            .map(|warning| match warning {
                ManifestWarning::EntryPointUnreadable { label, .. } => label,
                other => format!("{other:?}"),
            })
            .collect();
        (entries, warnings)
    }

    fn entry(group: &str, name: &str, target: &str, label: &str) -> Entry {
        (
            group.to_owned(),
            name.to_owned(),
            target.to_owned(),
            label.to_owned(),
        )
    }

    fn toml(text: &str) -> toml::Table {
        text.parse().expect("valid toml")
    }

    #[test]
    fn poetry_scripts_and_plugins_read_like_project_entry_points() {
        let poetry = toml(concat!(
            "[scripts]\n",
            "acme = \"acme.main:main\"\n",
            "tool = { reference = \"acme.tool:run\", type = \"console\", extras = [\"x\"] }\n",
            "old = { callable = \"acme.old:main\" }\n",
            "sh = { reference = \"bin/run.sh\", type = \"file\" }\n",
            "bad = 1\n",
            "[plugins.\"flake8.extension\"]\n",
            "ACM = \"acme.lint:Checker\"\n",
            "[plugins.console_scripts]\n",
            "other = \"acme.other:main\"\n",
        ));
        let (entries, warnings) = read(|reader| reader.read_poetry(&poetry));
        assert_eq!(
            entries,
            [
                entry(
                    "console",
                    "acme",
                    "acme.main:main",
                    "tool.poetry.scripts.acme"
                ),
                entry("console", "old", "acme.old:main", "tool.poetry.scripts.old"),
                entry(
                    "console",
                    "tool",
                    "acme.tool:run",
                    "tool.poetry.scripts.tool"
                ),
                entry(
                    "console",
                    "other",
                    "acme.other:main",
                    "tool.poetry.plugins.console_scripts.other"
                ),
                entry(
                    "flake8.extension",
                    "ACM",
                    "acme.lint:Checker",
                    "tool.poetry.plugins.flake8.extension.ACM"
                ),
            ]
        );
        assert_eq!(
            warnings,
            ["tool.poetry.scripts.bad", "tool.poetry.scripts.sh"]
        );
    }

    #[test]
    fn project_entries_that_are_not_strings_are_reported() {
        let project = toml(concat!(
            "scripts = { a = \"acme:main\", b = { reference = \"acme:b\" } }\n",
            "gui-scripts = \"acme:gui\"\n",
            "[entry-points]\n",
            "pytest11 = { acme = \"acme.plugin\" }\n",
            "broken = [\"acme:x\"]\n",
        ));
        let (entries, warnings) = read(|reader| reader.read_project(&project));
        assert_eq!(
            entries,
            [
                entry("console", "a", "acme:main", "project.scripts.a"),
                entry(
                    "pytest11",
                    "acme",
                    "acme.plugin",
                    "project.entry-points.pytest11.acme"
                ),
            ]
        );
        assert_eq!(
            warnings,
            [
                "project.scripts.b",
                "project.gui-scripts",
                "project.entry-points.broken"
            ]
        );
    }

    #[test]
    fn only_poetry_scripts_take_script_tables() {
        let mut warnings = Vec::new();
        EntryPoints {
            rel: "f",
            entries: &mut Vec::new(),
            warnings: &mut warnings,
        }
        .read_project(&toml("scripts = { b = { reference = \"acme:b\" } }"));
        assert!(
            matches!(
                &warnings[..],
                [ManifestWarning::EntryPointUnreadable { reason, .. }] if reason == "not a string"
            ),
            "{warnings:?}"
        );
    }

    #[test]
    fn setup_cfg_groups_split_like_setuptools() {
        let (entries, warnings) = read(|reader| {
            reader.read_setup_cfg_group(
                "console_scripts",
                "acme = acme.cli:main\nacme-gui=acme.gui:run",
            );
            reader.read_setup_cfg_group("gui_scripts", "a = acme.a:main [x], b = acme.b:main");
            reader.read_setup_cfg_group("pytest11", "acme.plugin");
            reader.skip_setup_cfg_option();
        });
        assert_eq!(
            entries,
            [
                entry(
                    "console",
                    "acme",
                    "acme.cli:main",
                    "options.entry_points.console_scripts.acme"
                ),
                entry(
                    "console",
                    "acme-gui",
                    "acme.gui:run",
                    "options.entry_points.console_scripts.acme-gui"
                ),
                entry(
                    "gui",
                    "a",
                    "acme.a:main",
                    "options.entry_points.gui_scripts.a"
                ),
                entry(
                    "gui",
                    "b",
                    "acme.b:main",
                    "options.entry_points.gui_scripts.b"
                ),
            ]
        );
        assert_eq!(
            warnings,
            ["options.entry_points.pytest11", "options.entry_points"]
        );
    }

    fn string(text: &str) -> Value {
        Value::Str(text.to_owned())
    }

    fn dict(items: Vec<(&str, Value)>, partial: bool) -> Value {
        Value::Dict {
            items: items
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
            partial,
            id: super::super::setup_py_eval::Identity(None),
        }
    }

    #[test]
    fn setup_py_entry_points_take_dicts_lists_and_sections() {
        let list = Value::List(
            vec![string("acme = acme.cli:main"), Value::Unknown],
            super::super::setup_py_eval::Identity(None),
        );
        let value = dict(
            vec![
                ("console_scripts", list),
                ("pytest11", string("acme = acme.plugin\n# off = acme.off\n")),
                ("gui_scripts", Value::Unknown),
            ],
            true,
        );
        let (entries, warnings) = read(|reader| {
            reader.read_setup_py(&value);
            reader.read_setup_py(&string(
                "\n# note\n; note\n[console_scripts]\nrun = acme.run:main\n\n[x]\n",
            ));
            reader.read_setup_py(&string("stray = acme:main\n"));
            reader.read_setup_py(&Value::Unknown);
        });
        assert_eq!(
            entries,
            [
                entry(
                    "console",
                    "acme",
                    "acme.cli:main",
                    "entry_points.console_scripts.acme"
                ),
                entry(
                    "pytest11",
                    "acme",
                    "acme.plugin",
                    "entry_points.pytest11.acme"
                ),
                entry(
                    "console",
                    "run",
                    "acme.run:main",
                    "entry_points.console_scripts.run"
                ),
            ]
        );
        assert_eq!(
            warnings,
            [
                "entry_points.console_scripts[1]",
                "entry_points.gui_scripts",
                "entry_points",
                "entry_points",
                "entry_points",
            ]
        );
    }

    #[test]
    fn targets_drop_extras_and_spaces_around_the_colon() {
        let (entries, warnings) = read(|reader| {
            reader.read_setup_cfg_group("console_scripts", "a = acme.cli : main [cli]");
            reader.read_setup_cfg_group("pytest11", "b = acme.plugin [test]");
            reader.read_setup_cfg_group("x", "c = acme:main[a,b]\nd = acme:d");
            // `key =` then one indented line, which reaches here without its
            // leading newline: setuptools splits that by line.
            reader.read_setup_cfg_group("y", "e = acme:e [a,b]");
            // An unclosed `[` is not extras, so the target stays whole.
            reader.read_setup_cfg_group("z", "f = acme:f [x");
        });
        let targets: Vec<&str> = entries.iter().map(|entry| entry.2.as_str()).collect();
        assert_eq!(
            targets,
            [
                "acme.cli:main",
                "acme.plugin",
                "acme:main",
                "acme:d",
                "acme:e",
                "acme:f [x"
            ]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn items_missing_a_name_or_a_target_are_reported() {
        let (entries, warnings) =
            read(|reader| reader.read_setup_cfg_group("console_scripts", "= acme:main\nacme ="));
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(
            warnings,
            [
                "options.entry_points.console_scripts",
                "options.entry_points.console_scripts"
            ]
        );
    }
}
