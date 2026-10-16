//! Django plugin extractor.

#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::{Ranged, TextSize};

use crate::config::PluginId;
use crate::entry::resolve_module_to_path;
use crate::manifest::literals::{
    LiteralScan, assigned_value, parse_module, string_list, string_value,
};
use crate::manifest::util::read_to_string;
use crate::parser::{LineIndex, resolve_relative_import};
use crate::rules::import_site;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    FrameworkUsedGlob, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
    SymbolReference, UnreadSettings,
};
use super::util::{
    choose_settings_path, django_settings_module_set_line, extract_django_settings_module,
    find_settings_candidates, manifest_has_dependency, origin_for_file, parse_module_symbol,
};
use super::warnings::{PluginsWarning, SettingsUnread};

/// Settings whose string-list values become module references (spec §9
/// Django plugin). Each is also reported as a partial parse when present but
/// not a literal list, so `TEMPLATES` (a list of dicts) cannot go here; add a
/// field such as `AUTHENTICATION_BACKENDS` only with a fixture whose module is
/// otherwise reported unused.
const LIST_FIELDS: &[&str] = &["INSTALLED_APPS", "MIDDLEWARE"];

/// Fields whose absence from the settings read leaves project files or
/// distributions unexplained, so an unfollowed star import may hide them.
const EXPLAINING_FIELDS: &[&str] = &["ROOT_URLCONF", "INSTALLED_APPS", "MIDDLEWARE"];

/// Files that set `DJANGO_SETTINGS_MODULE` before Django starts.
const SETTER_NAMES: &[&str] = &["manage.py", "wsgi.py", "asgi.py"];

/// Settings modules followed through star imports, the chosen one included.
const MAX_SETTINGS_FILES: usize = 16;

/// Origin label of the settings module's entry; symbol analysis uses it to
/// tell the settings entry from `manage.py` and `ROOT_URLCONF` (#729).
pub(crate) const SETTINGS_LABEL: &str = "settings.py";

/// How `DJANGO_SETTINGS_MODULE` was set across the files that set it.
#[derive(Default)]
struct SettingsModule {
    /// The first literal value, its file and line.
    literal: Option<(String, String, u32)>,
    /// The first place it is set in a form that is not a literal, or a
    /// setter that could not be read.
    unread: Option<(String, Option<u32>)>,
}

/// Where module names resolve: the discovered sources, and the directory the
/// setter of `DJANGO_SETTINGS_MODULE` imports from (`setter_dir`).
struct Lookup {
    known: BTreeSet<String>,
    dir: Option<String>,
}

/// Extract Django-related plugin hints.
pub(super) fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Django);
    let mut warnings = Vec::new();
    let root = ctx.root.path.as_path();
    let has_manage = root.join("manage.py").is_file();
    let known: BTreeSet<String> = ctx.sources.files.iter().map(|f| f.path.clone()).collect();

    if has_manage {
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec {
                path: "manage.py".to_owned(),
                symbol: None,
            },
            context: FileContext::Runtime,
            origin: ReferenceOrigin {
                file: "manage.py".to_owned(),
                line: None,
                label: "manage.py".to_owned(),
            },
        });
    }

    let settings_module = read_settings_module(ctx, has_manage, &mut contrib);
    // A Django project, not a reusable app that only depends on Django: its
    // settings decide what runs, so failing to find them is a gap (#914).
    let is_project =
        has_manage || settings_module.literal.is_some() || settings_module.unread.is_some();
    if !is_project && !manifest_has_dependency(ctx.manifest, "django") {
        return (contrib, warnings);
    }

    let lookup = Lookup {
        known,
        dir: settings_module
            .literal
            .as_ref()
            .and_then(|(_, setter, _)| setter_dir(setter)),
    };
    let resolved = settings_module
        .literal
        .as_ref()
        .and_then(|(module, _, _)| module_path(root, module, ctx, &lookup));
    let settings_path = resolved.or_else(|| {
        let candidates = find_settings_candidates(root);
        let (chosen, ambiguous) = choose_settings_path(
            &candidates,
            settings_module
                .literal
                .as_ref()
                .map(|(module, _, _)| module.as_str()),
            ctx.manifest.metadata.name.as_deref(),
        );
        if ambiguous && let Some(chosen) = &chosen {
            let others: Vec<String> = candidates
                .iter()
                .filter(|path| *path != chosen)
                .cloned()
                .collect();
            warnings.push(PluginsWarning::AmbiguousSettings {
                chosen: chosen.clone(),
                candidates: others,
            });
        }
        chosen
    });

    let Some(settings_rel) = settings_path else {
        if is_project {
            let (file, line, reason) = missing_settings(&settings_module);
            push_unread(&mut contrib, &mut warnings, file, line, reason);
        }
        return (contrib, warnings);
    };

    let settings_path_abs = abs_path(root, &settings_rel);
    contrib.entries.push(PluginEntry {
        spec: crate::config::EntrySpec {
            path: settings_rel.clone(),
            symbol: None,
        },
        context: FileContext::Runtime,
        origin: ReferenceOrigin {
            file: settings_rel.clone(),
            line: None,
            label: SETTINGS_LABEL.to_owned(),
        },
    });

    read_settings(ctx, &settings_rel, &lookup, &mut contrib, &mut warnings);

    contrib.framework_used_globs.push(FrameworkUsedGlob {
        pattern: "**/migrations/**/*.py".to_owned(),
        origin: origin_for_file(root, &settings_path_abs, "Django migrations"),
    });

    (contrib, warnings)
}

/// Read `DJANGO_SETTINGS_MODULE` from the root `manage.py`, then from the
/// discovered `manage.py`, `wsgi.py` and `asgi.py`, runtime ones first; each
/// literal value is a module reference.
fn read_settings_module(
    ctx: &PluginContext<'_>,
    has_manage: bool,
    contrib: &mut PluginContribution,
) -> SettingsModule {
    let root = ctx.root.path.as_path();
    let mut setters: Vec<(bool, &str)> = ctx
        .sources
        .files
        .iter()
        .filter(|file| file.path != "manage.py")
        .filter(|file| {
            Path::new(&file.path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SETTER_NAMES.contains(&name))
        })
        .map(|file| (file.context != FileContext::Runtime, file.path.as_str()))
        .collect();
    setters.sort_unstable();
    let setters = has_manage
        .then_some("manage.py")
        .into_iter()
        .chain(setters.into_iter().map(|(_, path)| path));

    let mut found = SettingsModule::default();
    for setter in setters {
        let Ok(contents) = read_to_string(&abs_path(root, setter)) else {
            found
                .unread
                .get_or_insert_with(|| (setter.to_owned(), None));
            continue;
        };
        if let Some((module, line)) = extract_django_settings_module(&contents) {
            contrib.module_refs.push(ModuleReference {
                module: module.clone(),
                origin: ReferenceOrigin {
                    file: setter.to_owned(),
                    line: Some(line),
                    label: "DJANGO_SETTINGS_MODULE".to_owned(),
                },
            });
            found
                .literal
                .get_or_insert_with(|| (module, setter.to_owned(), line));
        } else if let Some(line) = django_settings_module_set_line(&contents) {
            found
                .unread
                .get_or_insert_with(|| (setter.to_owned(), Some(line)));
        }
    }
    found
}

/// Why no settings module was found, and the file and line it points to.
fn missing_settings(found: &SettingsModule) -> (String, Option<u32>, SettingsUnread) {
    if let Some((module, file, line)) = &found.literal {
        let site = import_site(file, Some(*line));
        return (
            file.clone(),
            Some(*line),
            SettingsUnread::MissingModule {
                module: module.clone(),
                site,
            },
        );
    }
    if let Some((file, line)) = &found.unread {
        return (
            file.clone(),
            *line,
            SettingsUnread::UnreadModule {
                site: import_site(file, *line),
            },
        );
    }
    ("manage.py".to_owned(), None, SettingsUnread::NoSettings)
}

/// Record settings that could not be found or read for the gap stage, and
/// the warning that says why.
fn push_unread(
    contrib: &mut PluginContribution,
    warnings: &mut Vec<PluginsWarning>,
    file: String,
    line: Option<u32>,
    reason: SettingsUnread,
) {
    let what = match &reason {
        SettingsUnread::MissingModule { module, .. }
        | SettingsUnread::UnfollowedImport { module, .. } => Some(module.clone()),
        SettingsUnread::UnreadCall { call, .. } => Some(call.clone()),
        SettingsUnread::UnreadModule { .. } => Some("DJANGO_SETTINGS_MODULE".to_owned()),
        SettingsUnread::NoSettings | SettingsUnread::Unreadable { .. } => None,
    };
    contrib.unread_settings.push(UnreadSettings {
        file: file.clone(),
        site: what.map(|what| (what, line)),
    });
    warnings.push(PluginsWarning::DjangoSettingsUnread {
        path: file,
        line,
        reason,
    });
}

/// Read the settings module and the project modules it star-imports: each
/// field is read from every file that sets it.
fn read_settings(
    ctx: &PluginContext<'_>,
    settings_rel: &str,
    lookup: &Lookup,
    contrib: &mut PluginContribution,
    warnings: &mut Vec<PluginsWarning>,
) {
    let root = ctx.root.path.as_path();
    let mut queue = VecDeque::from([settings_rel.to_owned()]);
    let mut visited = BTreeSet::from([settings_rel.to_owned()]);
    let mut assigned: BTreeSet<&str> = BTreeSet::new();
    let mut unfollowed: Option<(String, String, u32)> = None;
    let mut called: Option<(String, String, u32)> = None;
    let mut urlconf_read = false;

    while let Some(rel) = queue.pop_front() {
        let contents = read_to_string(&abs_path(root, &rel)).ok();
        let stmts = contents.as_deref().and_then(parse_module);
        let (Some(contents), Some(stmts)) = (contents, stmts) else {
            // A discovered module that does not parse is the parse stage's
            // to report; one it never reads is this plugin's.
            let parse_reports = lookup.known.contains(&rel) && root.join(&rel).is_file();
            if !parse_reports {
                push_unread(
                    contrib,
                    warnings,
                    rel.clone(),
                    None,
                    SettingsUnread::Unreadable { path: rel },
                );
            }
            continue;
        };
        let lines = LineIndex::new(&contents);
        let origin = |start: TextSize, label: &str| ReferenceOrigin {
            file: rel.clone(),
            line: Some(lines.line(start)),
            label: label.to_owned(),
        };

        for field in EXPLAINING_FIELDS {
            if assigned_value(&stmts, field).is_some() {
                assigned.insert(field);
            }
        }

        let lists: BTreeMap<String, LiteralScan> = LIST_FIELDS
            .iter()
            .filter_map(|field| {
                let scan = assigned_value(&stmts, field).and_then(string_list)?;
                Some(((*field).to_owned(), scan))
            })
            .collect();
        let mut partial = super::util::partial_fields(&lists);
        for field in LIST_FIELDS {
            if contents.contains(field) && !lists.contains_key(*field) {
                partial.push((*field).to_owned());
            }
        }
        partial.sort();
        partial.dedup();
        if !partial.is_empty() {
            warnings.push(PluginsWarning::PartialSettingsParse {
                path: rel.clone(),
                fields: partial,
            });
        }

        for (field, scan) in &lists {
            for (module, start) in scan.values.iter().zip(&scan.starts) {
                contrib.module_refs.push(ModuleReference {
                    module: module.clone(),
                    origin: origin(*start, field),
                });
            }
        }

        if !urlconf_read
            && let Some(value) = assigned_value(&stmts, "ROOT_URLCONF")
            && let Some(urlconf) = string_value(value)
        {
            urlconf_read = true;
            contrib.module_refs.push(ModuleReference {
                module: urlconf.clone(),
                origin: origin(value.start(), "ROOT_URLCONF"),
            });
            // Only a discovered file can be an entry.
            if let Some(urls_path) =
                module_path(root, &urlconf, ctx, lookup).filter(|path| lookup.known.contains(path))
            {
                contrib.entries.push(PluginEntry {
                    spec: crate::config::EntrySpec {
                        path: urls_path,
                        symbol: None,
                    },
                    context: FileContext::Runtime,
                    origin: ReferenceOrigin {
                        file: rel.clone(),
                        line: None,
                        label: "ROOT_URLCONF".to_owned(),
                    },
                });
            }
        }

        for field in ["WSGI_APPLICATION", "ASGI_APPLICATION"] {
            if let Some(value) = assigned_value(&stmts, field)
                && let Some(target) = string_value(value)
                && let Some((module, symbol)) = parse_module_symbol(&target)
            {
                contrib.symbol_refs.push(SymbolReference {
                    module,
                    symbol,
                    origin: origin(value.start(), field),
                });
            }
        }

        if called.is_none()
            && let Some((call, start)) = settings_call(&stmts)
        {
            called = Some((call, rel.clone(), lines.line(start)));
        }

        for (module, start) in star_imports(&stmts, &rel, ctx) {
            let followed = module
                .as_deref()
                .and_then(|module| module_path(root, module, ctx, lookup));
            match followed {
                Some(path) if visited.contains(&path) => {},
                Some(path) if visited.len() < MAX_SETTINGS_FILES => {
                    visited.insert(path.clone());
                    queue.push_back(path);
                },
                // Not a project file, or past the limit: either may set a field.
                _ => {
                    let shown = module.unwrap_or_else(|| "*".to_owned());
                    unfollowed.get_or_insert_with(|| (shown, rel.clone(), lines.line(start)));
                },
            }
        }
    }

    // What the settings did not set may come from a module they star-import
    // or a file they run.
    if EXPLAINING_FIELDS
        .iter()
        .all(|field| assigned.contains(field))
    {
        return;
    }
    if let Some((module, file, line)) = unfollowed {
        let site = import_site(&file, Some(line));
        push_unread(
            contrib,
            warnings,
            file,
            Some(line),
            SettingsUnread::UnfollowedImport { module, site },
        );
    } else if let Some((call, file, line)) = called {
        let site = import_site(&file, Some(line));
        push_unread(
            contrib,
            warnings,
            file,
            Some(line),
            SettingsUnread::UnreadCall { call, site },
        );
    }
}

/// Calls that run other files into the settings namespace, which the plugin
/// does not read: django-split-settings' `include()` and `exec()`.
const SETTINGS_CALLS: &[&str] = &["include", "exec"];

/// The first [`SETTINGS_CALLS`] call that runs when the settings are
/// imported, shown as `name()`, and where it starts. Module-level blocks
/// (`if`, `try`, `with`) count; function, class and lambda bodies do not
/// run on import (#925).
fn settings_call(stmts: &[Stmt]) -> Option<(String, TextSize)> {
    #[derive(Default)]
    struct Calls(Option<(String, TextSize)>);

    impl Visitor<'_> for Calls {
        fn visit_stmt(&mut self, stmt: &Stmt) {
            if self.0.is_none() && !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                walk_stmt(self, stmt);
            }
        }

        fn visit_expr(&mut self, expr: &Expr) {
            if self.0.is_some() || matches!(expr, Expr::Lambda(_)) {
                return;
            }
            if let Expr::Call(call) = expr {
                let name = match &*call.func {
                    Expr::Name(name) => Some(name.id.as_str()),
                    Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
                    _ => None,
                };
                if let Some(name) = name.filter(|name| SETTINGS_CALLS.contains(name)) {
                    self.0 = Some((format!("{name}()"), call.start()));
                    return;
                }
            }
            walk_expr(self, expr);
        }
    }

    let mut calls = Calls::default();
    calls.visit_body(stmts);
    calls.0
}

/// Top-level `from m import *` statements: the absolute module, `None` when a
/// relative one does not resolve, and where the statement starts.
fn star_imports(
    stmts: &[Stmt],
    rel: &str,
    ctx: &PluginContext<'_>,
) -> Vec<(Option<String>, TextSize)> {
    stmts
        .iter()
        .filter_map(|stmt| {
            let Stmt::ImportFrom(import) = stmt else {
                return None;
            };
            if !import.names.iter().any(|alias| alias.name.as_str() == "*") {
                return None;
            }
            let level = u8::try_from(import.level).unwrap_or(u8::MAX);
            let suffix = import.module.as_ref().map(ToString::to_string);
            let module = resolve_relative_import(
                rel,
                &ctx.sources.layout,
                level,
                suffix.as_deref(),
                Some("*"),
            );
            Some((module, import.start()))
        })
        .collect()
}

/// The project file a module name loads: a discovered source, else a file on
/// disk the discovery globs did not take, as a root `settings.py` in a flat
/// layout, else one under a setter's import directory.
fn module_path(
    root: &Path,
    module: &str,
    ctx: &PluginContext<'_>,
    lookup: &Lookup,
) -> Option<String> {
    resolve_module_to_path(module, &ctx.sources.layout, &lookup.known).or_else(|| {
        let suffix = module.replace('.', "/");
        let in_dir = lookup.dir.iter().flat_map(|dir| {
            [
                format!("{dir}/{suffix}.py"),
                format!("{dir}/{suffix}/__init__.py"),
            ]
        });
        [
            format!("{suffix}.py"),
            format!("{suffix}/__init__.py"),
            format!("src/{suffix}.py"),
            format!("src/{suffix}/__init__.py"),
        ]
        .into_iter()
        .chain(in_dir)
        .find(|path| abs_path(root, path).is_file())
    })
}

/// The directory a nested setter imports the settings module from: Django
/// puts the directory of `manage.py` on `sys.path`, and `wsgi.py` / `asgi.py`
/// sit in the project package beside it. A root setter needs none.
fn setter_dir(setter: &str) -> Option<String> {
    let path = Path::new(setter);
    let dir = path.parent();
    let dir = if path.file_name().is_some_and(|name| name == "manage.py") {
        dir
    } else {
        dir.and_then(Path::parent)
    };
    dir.and_then(Path::to_str)
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
}

fn abs_path(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('/', MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_settings_prefers_manage_module() {
        let candidates = vec![
            "other/settings.py".to_owned(),
            "mysite/settings.py".to_owned(),
        ];
        let (chosen, ambiguous) = choose_settings_path(&candidates, Some("mysite.settings"), None);
        assert_eq!(chosen.as_deref(), Some("mysite/settings.py"));
        assert!(ambiguous);
    }
}
