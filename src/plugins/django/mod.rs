//! Django plugin extractor.

mod apps;
mod installed;
mod settings_env;
mod settings_refs;
mod urls;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, ExprCall, Stmt};
use ruff_text_size::{Ranged, TextSize};

use crate::config::PluginId;
use crate::entry::resolve_module_to_path;
use crate::manifest::literals::{
    LiteralScan, assigned_value, parse_module, string_list, string_value,
};
use crate::manifest::util::read_to_string;
use crate::parser::{LineIndex, resolve_relative_import};
use crate::path_util::is_file_named;
use crate::rules::import_site;
use crate::sources::FileContext;

use super::commands::is_dotted_identifier;
use super::context::PluginContext;
use super::pytest::import_settings as pytest_import_settings;
use super::types::{
    FrameworkUsedGlob, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
    SymbolReference, UnreadSettings,
};
use super::util::{
    choose_settings_path, django_settings_module_set_line, extract_django_settings_module,
    find_settings_candidates, manifest_has_dependency, origin_for_file, parse_module_symbol,
    project_dirs,
};
use super::warnings::{PluginsWarning, SettingsUnread};

/// Settings whose string-list values become module references (spec §9
/// Django plugin). Each is also reported as a partial parse when present but
/// not a literal list. `INSTALLED_APPS` is read by `installed`, and nested
/// import settings by `settings_refs`.
const LIST_FIELDS: &[&str] = &["MIDDLEWARE"];

/// The settings field naming the packages Django's format loader reads
/// `<prefix>.<locale>.formats` from (#1239).
const FORMAT_MODULE_PATH: &str = "FORMAT_MODULE_PATH";

/// Fields whose absence from the settings read leaves project files or
/// distributions unexplained, so an unfollowed star import may hide them.
const EXPLAINING_FIELDS: &[&str] = &["ROOT_URLCONF", "INSTALLED_APPS", "MIDDLEWARE"];

/// Files that set `DJANGO_SETTINGS_MODULE` before Django starts: the
/// management and server entry points, and the test runners that set it for
/// a reusable app's suite (wagtail's `runtests.py`, a `conftest.py`, #979).
const SETTER_NAMES: &[&str] = &[
    "manage.py",
    "wsgi.py",
    "asgi.py",
    "runtests.py",
    "conftest.py",
];

/// Directories whose setters and `settings.py` configure a sample project,
/// not this one (celery's `examples/django/`, #979).
pub(super) const EXAMPLE_DIRS: &[&str] = &["examples", "example"];

/// Settings modules followed through star imports, the chosen one included.
const MAX_SETTINGS_FILES: usize = 16;

/// Origin label of the settings module's entry; symbol analysis uses it to
/// tell the settings entry from `manage.py` and `ROOT_URLCONF` (#729).
pub(crate) const SETTINGS_LABEL: &str = "settings.py";

/// Origin labels of the files the settings module star-imports or
/// `include()`s, read as part of the settings (#938, #1239).
pub(crate) const STAR_IMPORT_LABEL: &str = "settings star import";
pub(crate) const INCLUDE_LABEL: &str = "settings include()";

const MIGRATIONS_LABEL: &str = "Django migrations";

/// Origin label of a module unittest discovery imports (#1239).
const TEST_DISCOVERY_LABEL: &str = "manage.py test discovery";

/// Origin label of a settings module another deployment selects (#1239).
const SETTINGS_VARIANT_LABEL: &str = "settings variant";

/// How `DJANGO_SETTINGS_MODULE` was set across the files that set it.
#[derive(Default)]
struct SettingsModule {
    /// The literal values in reading order, each with its file and line
    /// (none for a pytest config option).
    literals: Vec<(String, String, Option<u32>)>,
    /// The first place it is set in a form that is not a literal, or a
    /// setter that could not be read.
    unread: Option<(String, Option<u32>)>,
    /// The same for the environment files, which only explain a missing
    /// settings module after `unread`.
    env_unread: Option<(String, Option<u32>)>,
    /// A setter or pytest config sets it: a Django project, not a reusable
    /// app. An environment file alone does not say so, since a reusable app
    /// may ship a `.env.example` for its demo.
    in_setters: bool,
}

/// Where module names resolve: the discovered sources, and the directory the
/// setter of `DJANGO_SETTINGS_MODULE` imports from (`setter_dir`).
struct Lookup {
    known: BTreeSet<String>,
    modules: BTreeSet<String>,
    dir: Option<String>,
}

/// Extract Django-related plugin hints.
#[expect(
    clippy::too_many_lines,
    reason = "reads every config surface the framework uses, in order; each block is short"
)]
pub(super) fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Django);
    let mut warnings = Vec::new();
    apps::receivers(ctx, &mut contrib);
    let root = ctx.root.path.as_path();
    let has_manage = is_file_named(root, "manage.py");
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
    let is_project = has_manage || settings_module.in_setters;
    if !is_project && !manifest_has_dependency(ctx.manifest, "django") {
        return (contrib, warnings);
    }

    // `manage.py test` runs unittest discovery, which imports every
    // `test*.py` by name, so nothing else reaches them (#1239): `tests.py`
    // of an app and babybuddy's `tests_views.py`, which pytest's
    // `test_*.py` does not match. A name that only starts with `test`
    // (`testing.py`, `testutils.py`) is left out: it is as likely to be a
    // helper module, and taking it would hide a real orphan. The entry is
    // a test root, so the classes in an app's `tests.py` the runner
    // collects are no unused exports, as a pytest suite's are not.
    let discovered_tests: Vec<String> = if is_project {
        ctx.sources
            .python_files()
            .filter(|file| is_discovered_test(&file.path))
            .filter(|file| {
                crate::sources::path_to_module(&file.path, &ctx.sources.layout).is_some()
            })
            .map(|file| file.path.clone())
            .collect()
    } else {
        Vec::new()
    };
    for path in discovered_tests {
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec {
                path: path.clone(),
                symbol: None,
            },
            context: FileContext::Test,
            origin: ReferenceOrigin {
                file: path,
                line: None,
                label: TEST_DISCOVERY_LABEL.to_owned(),
            },
        });
    }

    // The first value that names a project file wins: a template's
    // placeholder or another app's settings earlier on must not hide it.
    let first = settings_module.literals.first();
    let mut lookup = Lookup {
        modules: known
            .iter()
            .filter_map(|path| crate::sources::path_to_module(path, &ctx.sources.layout))
            .collect(),
        known,
        dir: first.and_then(|(_, setter, _)| setter_dir(setter)),
    };
    let mut resolved = None;
    for (module, setter, _) in &settings_module.literals {
        lookup.dir = setter_dir(setter);
        resolved = module_path(root, module, ctx, &lookup);
        if resolved.is_some() {
            break;
        }
    }
    if resolved.is_none() {
        lookup.dir = first.and_then(|(_, setter, _)| setter_dir(setter));
    }
    let settings_path = resolved.or_else(|| {
        let candidates = find_settings_candidates(root, EXAMPLE_DIRS);
        let (chosen, ambiguous) = choose_settings_path(
            &candidates,
            first.map(|(module, _, _)| module.as_str()),
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
        let origin = if is_project {
            let (file, line, reason) = missing_settings(&settings_module);
            let origin = ReferenceOrigin {
                file: file.clone(),
                line,
                label: MIGRATIONS_LABEL.to_owned(),
            };
            push_unread(&mut contrib, &mut warnings, file, line, reason);
            Some(origin)
        } else {
            django_declaration(ctx.manifest)
        };
        // Django runs the migrations of every installed app, so a reusable
        // app's own are used without settings of its own (DRF, #979).
        contrib
            .framework_used_globs
            .extend(origin.map(migrations_glob));
        return (contrib, warnings);
    };

    // The other settings modules beside the one that is read are the
    // deployment's variants, named by `--settings` or another environment
    // (babybuddy's `settings/ci.py`), so they are entries too (#1239).
    let variants = settings_variants(root, &settings_rel, ctx);
    contrib.settings_modules.extend(
        variants
            .iter()
            .filter_map(|path| crate::sources::path_to_module(path, &ctx.sources.layout)),
    );
    for path in variants {
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec {
                path: path.clone(),
                symbol: None,
            },
            context: FileContext::Runtime,
            origin: ReferenceOrigin {
                file: path,
                line: None,
                label: SETTINGS_VARIANT_LABEL.to_owned(),
            },
        });
    }

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

    // Each value of `DJANGO_SETTINGS_MODULE` is a settings module in some
    // deployment (`settings.dev`, `settings.prod`), though only one is read.
    contrib.settings_modules.extend(
        settings_module
            .literals
            .iter()
            .map(|(module, _, _)| module.clone())
            .chain(crate::sources::path_to_module(
                &settings_rel,
                &ctx.sources.layout,
            )),
    );

    let installed = read_settings(ctx, &settings_rel, &lookup, &mut contrib, &mut warnings);
    apps::extract(ctx, &lookup, &installed, &mut contrib);

    contrib
        .framework_used_globs
        .push(migrations_glob(origin_for_file(
            root,
            &settings_path_abs,
            MIGRATIONS_LABEL,
        )));

    (contrib, warnings)
}

/// The glob Django's migration loader reads in every installed app.
fn migrations_glob(origin: ReferenceOrigin) -> FrameworkUsedGlob {
    FrameworkUsedGlob {
        pattern: "**/migrations/**/*.py".to_owned(),
        origin,
    }
}

/// Where the manifest declares Django, for a reusable app's migrations.
/// Whether `path` is a module unittest discovery imports as a test:
/// `tests.py`, `test.py`, or a name starting with `test_` / `tests_`.
fn is_discovered_test(path: &str) -> bool {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    let Some(stem) = name.strip_suffix(".py") else {
        return false;
    };
    matches!(stem, "test" | "tests") || stem.starts_with("test_") || stem.starts_with("tests_")
}

/// The discovered modules beside `settings_rel` that another deployment
/// selects: a module that star-imports one of its siblings (wger's
/// `settings/ci.py` takes `settings/settings_global.py`, babybuddy's
/// `settings/ci.py` takes `settings/base.py`), or one that sets
/// `INSTALLED_APPS` / `ROOT_URLCONF` as the settings module does. A star
/// import of something else (`from .models import *`) is ordinary code, so
/// the looser settings shape is not enough here. `settings_rel` itself and
/// `__init__.py` are not variants.
fn settings_variants(root: &Path, settings_rel: &str, ctx: &PluginContext<'_>) -> Vec<String> {
    let Some((dir, _)) = settings_rel.rsplit_once('/') else {
        return Vec::new();
    };
    let siblings: BTreeSet<&str> = ctx
        .sources
        .python_files()
        .filter_map(|file| {
            let (parent, name) = file.path.rsplit_once('/')?;
            (parent == dir).then_some(name.trim_end_matches(".py"))
        })
        .collect();
    let mut variants: Vec<String> = ctx
        .sources
        .python_files()
        .map(|file| file.path.clone())
        .filter(|path| {
            path.rsplit_once('/')
                .is_some_and(|(parent, name)| parent == dir && name != "__init__.py")
        })
        .filter(|path| path != settings_rel && is_settings_variant(root, path, &siblings))
        .collect();
    variants.sort();
    variants
}

/// Whether the file at `path` star-imports one of the `siblings` beside it,
/// or assigns the fields a settings module assigns.
fn is_settings_variant(root: &Path, path: &str, siblings: &BTreeSet<&str>) -> bool {
    let Ok(contents) = read_to_string(&abs_path(root, path)) else {
        // Unreadable here means the parser reports it; the plugin does not
        // guess what it holds.
        return false;
    };
    let Some(stmts) = parse_module(&contents) else {
        return false;
    };
    stmts.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(import) => {
            import.names.iter().any(|alias| &alias.name == "*")
                && import.module.as_ref().is_some_and(|module| {
                    module
                        .split('.')
                        .next_back()
                        .is_some_and(|name| siblings.contains(name))
                })
        },
        _ => false,
    }) || ["INSTALLED_APPS", "ROOT_URLCONF"]
        .into_iter()
        .any(|field| assigned_value(&stmts, field).is_some())
}

fn django_declaration(manifest: &crate::manifest::LoadedManifest) -> Option<ReferenceOrigin> {
    manifest
        .dependencies
        .iter()
        .find(|dep| dep.name == "django")
        .map(|dep| ReferenceOrigin {
            file: dep.origin.file.clone(),
            line: dep.origin.line,
            label: MIGRATIONS_LABEL.to_owned(),
        })
}

/// Read `DJANGO_SETTINGS_MODULE` from the root `manage.py`, then from the
/// discovered setters (runtime ones first), the root setters discovery did
/// not take (`runtests.py`), pytest-django's `--ds` / ini option, and last
/// the environment files of the root and each member (`.env`, compose files,
/// Dockerfiles: #999, #1239); each
/// literal value is a module reference. Setters under [`EXAMPLE_DIRS`] are
/// skipped, and a value that is not a dotted module name (a project
/// template's `{{ project_name }}.settings`) counts as set in a form not read.
fn read_settings_module(
    ctx: &PluginContext<'_>,
    has_manage: bool,
    contrib: &mut PluginContribution,
) -> SettingsModule {
    let root = ctx.root.path.as_path();
    let is_setter = |path: &str| {
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| SETTER_NAMES.contains(&name))
            && !path.split('/').any(|part| EXAMPLE_DIRS.contains(&part))
    };
    let setters = setter_paths(ctx, root, has_manage, &is_setter);

    let mut found = SettingsModule::default();
    let mut literal = |found: &mut SettingsModule, module: String, file: &str, line| {
        if !is_dotted_identifier(&module) {
            found.unread.get_or_insert_with(|| (file.to_owned(), line));
            return;
        }
        contrib.module_refs.push(ModuleReference {
            module: module.clone(),
            origin: ReferenceOrigin {
                file: file.to_owned(),
                line,
                label: "DJANGO_SETTINGS_MODULE".to_owned(),
            },
        });
        found.literals.push((module, file.to_owned(), line));
    };
    for setter in setters {
        let Ok(contents) = read_to_string(&abs_path(root, setter)) else {
            found
                .unread
                .get_or_insert_with(|| (setter.to_owned(), None));
            continue;
        };
        if let Some((module, line)) = extract_django_settings_module(&contents) {
            literal(&mut found, module, setter, Some(line));
        } else if let Some(line) = django_settings_module_set_line(&contents) {
            found
                .unread
                .get_or_insert_with(|| (setter.to_owned(), Some(line)));
        }
    }
    if let Some((file, module)) =
        pytest_import_settings(root).and_then(|settings| settings.django_settings_module)
    {
        literal(&mut found, module, &file, None);
    }
    found.in_setters = !found.literals.is_empty() || found.unread.is_some();
    // Nothing reads the environment files of a project Django is not in.
    if !has_manage && !found.in_setters && !manifest_has_dependency(ctx.manifest, "django") {
        return found;
    }
    let members: Vec<&str> = project_dirs(ctx).collect();
    let dirs = members
        .iter()
        .copied()
        .filter(|dir| !dir.split('/').any(|part| EXAMPLE_DIRS.contains(&part)));
    let nested = settings_env::read_subdir_compose(root, &|name| {
        members.contains(&name) || EXAMPLE_DIRS.contains(&name)
    });
    for setting in settings_env::read(root, dirs).into_iter().chain(nested) {
        if is_dotted_identifier(&setting.value) {
            literal(&mut found, setting.value, &setting.file, Some(setting.line));
        } else {
            found
                .env_unread
                .get_or_insert((setting.file, Some(setting.line)));
        }
    }
    found
}

/// The files that may set `DJANGO_SETTINGS_MODULE`, in read order:
/// `manage.py`, then the discovered setters (runtime context first), then
/// the root files of the same names discovery left out (wagtail's
/// `runtests.py`).
fn setter_paths<'a>(
    ctx: &'a PluginContext<'a>,
    root: &Path,
    has_manage: bool,
    is_setter: &dyn Fn(&str) -> bool,
) -> Vec<&'a str> {
    let mut setters: Vec<(bool, &str)> = ctx
        .sources
        .files
        .iter()
        .filter(|file| file.path != "manage.py" && is_setter(&file.path))
        .map(|file| (file.context != FileContext::Runtime, file.path.as_str()))
        .collect();
    setters.sort_unstable();
    let discovered: BTreeSet<&str> = setters.iter().map(|(_, path)| *path).collect();
    let undiscovered_root = SETTER_NAMES
        .iter()
        .copied()
        .filter(|name| *name != "manage.py" && !discovered.contains(name))
        .filter(|name| root.join(name).is_file());
    has_manage
        .then_some("manage.py")
        .into_iter()
        .chain(setters.into_iter().map(|(_, path)| path))
        .chain(undiscovered_root)
        .collect()
}

/// Why no settings module was found, and the file and line it points to.
fn missing_settings(found: &SettingsModule) -> (String, Option<u32>, SettingsUnread) {
    if let Some((module, file, line)) = found.literals.first() {
        let site = import_site(file, *line);
        return (
            file.clone(),
            *line,
            SettingsUnread::MissingModule {
                module: module.clone(),
                site,
            },
        );
    }
    if let Some((file, line)) = found.unread.as_ref().or(found.env_unread.as_ref()) {
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
        SettingsUnread::UnreadField { field, .. } => Some(field.clone()),
        SettingsUnread::UnreadModule { .. } => Some("DJANGO_SETTINGS_MODULE".to_owned()),
        SettingsUnread::UnreadInclude { .. } => Some("include()".to_owned()),
        SettingsUnread::NoSettings | SettingsUnread::Unreadable { .. } => None,
    };
    let backend = match &reason {
        SettingsUnread::UnreadField { field, .. }
            if settings_refs::BACKEND_FIELDS.contains(&field.as_str()) =>
        {
            Some(field.clone())
        },
        _ => None,
    };
    contrib.unread_settings.push(UnreadSettings {
        file: file.clone(),
        site: what.map(|what| (what, line)),
        backend: backend.clone(),
    });
    warnings.push(match (backend, reason) {
        (Some(field), SettingsUnread::UnreadField { site, .. }) => {
            PluginsWarning::DjangoBackendUnread {
                path: file,
                line,
                field,
                site,
            }
        },
        (_, reason) => PluginsWarning::DjangoSettingsUnread {
            path: file,
            line,
            reason,
        },
    });
}

/// Read the settings module and the project files it star-imports or
/// `include()`s: each field is read from every file that sets it.
#[expect(
    clippy::too_many_lines,
    reason = "reads each settings field from every file that sets it, one block per field"
)]
fn read_settings(
    ctx: &PluginContext<'_>,
    settings_rel: &str,
    lookup: &Lookup,
    contrib: &mut PluginContribution,
    warnings: &mut Vec<PluginsWarning>,
) -> apps::Installed {
    let root = ctx.root.path.as_path();
    let mut queue = VecDeque::from([settings_rel.to_owned()]);
    let mut visited = BTreeSet::from([settings_rel.to_owned()]);
    let mut assigned: BTreeSet<&str> = BTreeSet::new();
    let mut unfollowed: Option<(String, String, u32)> = None;
    let mut called: Option<(String, String, u32)> = None;
    let mut urlconf_read = false;
    let mut installed = apps::Installed::default();
    let mut parts: Vec<(String, ReferenceOrigin)> = Vec::new();

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
            installed.incomplete = true;
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

        if let Some(apps) = installed::read(&stmts) {
            for (module, start) in apps.values {
                let origin = origin(start, "INSTALLED_APPS");
                contrib.module_refs.push(ModuleReference {
                    module: module.clone(),
                    origin: origin.clone(),
                });
                installed.apps.push((module, origin));
            }
            if let Some(start) = apps.unread {
                installed.incomplete = true;
                let line = Some(lines.line(start));
                let reason = SettingsUnread::UnreadField {
                    field: "INSTALLED_APPS".to_owned(),
                    site: import_site(&rel, line),
                };
                push_unread(contrib, warnings, rel.clone(), line, reason);
            }
        }

        for (field, scan) in &lists {
            for (module, start) in scan.values.iter().zip(&scan.starts) {
                contrib.module_refs.push(ModuleReference {
                    module: module.clone(),
                    origin: origin(*start, field),
                });
            }
        }

        // Django's format loader imports `<prefix>.<locale>.formats` for
        // the active locale, so every `formats.py` under the prefix is used
        // (babybuddy's `babybuddy/formats/en/formats.py`, #1239). A prefix
        // that is no string literal names no package to mark.
        if let Some(value) = assigned_value(&stmts, FORMAT_MODULE_PATH) {
            let prefixes = string_value(value)
                .map(|prefix| vec![prefix])
                .or_else(|| string_list(value).map(|scan| scan.values))
                .unwrap_or_default();
            for prefix in prefixes {
                let Some(dir) = module_path(root, &prefix, ctx, lookup)
                    .as_deref()
                    .and_then(|path| path.strip_suffix("/__init__.py"))
                    .map(str::to_owned)
                else {
                    continue;
                };
                contrib.framework_used_globs.push(FrameworkUsedGlob {
                    pattern: format!("{dir}/**/formats.py"),
                    origin: origin(value.start(), FORMAT_MODULE_PATH),
                });
                // The loader reads the format names from each module's
                // namespace, as Django reads the settings', so no importer
                // names them.
                let format_modules: Vec<String> = ctx
                    .sources
                    .python_files()
                    .filter(|file| {
                        file.path.starts_with(&format!("{dir}/"))
                            && file.path.ends_with("/formats.py")
                    })
                    .filter_map(|file| {
                        crate::sources::path_to_module(&file.path, &ctx.sources.layout)
                    })
                    .collect();
                contrib.settings_modules.extend(format_modules);
            }
        }

        // A value that is not a string literal is not read, and is no gap:
        // its package is treated as it was before this setting was read.
        if let Some(Expr::Dict(dict)) = assigned_value(&stmts, "MIGRATION_MODULES") {
            for item in &dict.items {
                if let Some(package) = string_value(&item.value) {
                    let origin = origin(item.value.start(), "MIGRATION_MODULES");
                    installed.migration_modules.push((package, origin));
                }
            }
        }

        let (references, unread) = settings_refs::extract(lookup, &stmts, &rel, &lines);
        contrib.module_refs.extend(references);
        for origin in unread {
            let reason = SettingsUnread::UnreadField {
                field: origin.label,
                site: import_site(&origin.file, origin.line),
            };
            push_unread(contrib, warnings, origin.file, origin.line, reason);
        }

        if !urlconf_read
            && let Some(value) = assigned_value(&stmts, "ROOT_URLCONF")
            && let Some(urlconf) = string_value(value)
        {
            urlconf_read = true;
            let urlconf_origin = origin(value.start(), "ROOT_URLCONF");
            contrib.module_refs.push(ModuleReference {
                module: urlconf.clone(),
                origin: urlconf_origin.clone(),
            });
            // Only a discovered file can be an entry.
            if let Some(urls_path) =
                module_path(root, &urlconf, ctx, lookup).filter(|path| lookup.known.contains(path))
            {
                contrib.entries.push(PluginEntry {
                    spec: crate::config::EntrySpec {
                        path: urls_path.clone(),
                        symbol: None,
                    },
                    context: FileContext::Runtime,
                    origin: ReferenceOrigin {
                        file: rel.clone(),
                        line: None,
                        label: "ROOT_URLCONF".to_owned(),
                    },
                });
                let urlconf_module =
                    crate::sources::path_to_module(&urls_path, &ctx.sources.layout)
                        .unwrap_or_else(|| urlconf.clone());
                urls::root_names_used(contrib, &urlconf_module, &urlconf_origin);
                urls::follow(ctx, lookup, &urls_path, contrib, warnings);
            }
        }

        for field in ["WSGI_APPLICATION", "ASGI_APPLICATION"] {
            let Some(value) = assigned_value(&stmts, field) else {
                continue;
            };
            let target = string_value(value);
            // Unconfigured handlers name no project module to hide.
            if matches!(value, Expr::NoneLiteral(_)) || target.as_deref() == Some("") {
                continue;
            }
            if let Some((module, symbol)) = target.as_deref().and_then(application_target) {
                contrib.symbol_refs.push(SymbolReference {
                    module,
                    symbol,
                    origin: origin(value.start(), field),
                });
            } else {
                let line = Some(lines.line(value.start()));
                push_unread(
                    contrib,
                    warnings,
                    rel.clone(),
                    line,
                    SettingsUnread::UnreadField {
                        field: field.to_owned(),
                        site: import_site(&rel, line),
                    },
                );
            }
        }

        let calls = settings_calls(&stmts);
        let mut unread_call = calls.unread;
        for (patterns, start) in calls.includes {
            for (pattern, optional) in patterns {
                // A missing file that is not `optional()` fails the import
                // under split-settings; here it is a file not seen.
                let files = include_files(root, &rel, &pattern)
                    .filter(|files| optional || !files.is_empty());
                let Some(files) = files else {
                    unread_call.get_or_insert_with(|| ("include()".to_owned(), start));
                    continue;
                };
                for path in files {
                    if visited.contains(&path) {
                        continue;
                    }
                    if visited.len() >= MAX_SETTINGS_FILES {
                        unread_call.get_or_insert_with(|| ("include()".to_owned(), start));
                        break;
                    }
                    visited.insert(path.clone());
                    parts.push((path.clone(), origin(start, INCLUDE_LABEL)));
                    queue.push_back(path);
                }
            }
        }
        if let Some((call, start)) = unread_call {
            installed.incomplete = true;
            called.get_or_insert_with(|| (call, rel.clone(), lines.line(start)));
        }

        for (module, beside, start) in star_imports(&stmts, &rel, root, ctx) {
            let followed = module
                .as_deref()
                .and_then(|module| module_path(root, module, ctx, lookup))
                .or(beside);
            match followed {
                Some(path) if visited.contains(&path) => {},
                Some(path) if visited.len() < MAX_SETTINGS_FILES => {
                    visited.insert(path.clone());
                    parts.push((path.clone(), origin(start, STAR_IMPORT_LABEL)));
                    queue.push_back(path);
                },
                // Not a project file, or past the limit: either may set a field.
                _ => {
                    installed.incomplete = true;
                    let shown = module.unwrap_or_else(|| "*".to_owned());
                    unfollowed.get_or_insert_with(|| (shown, rel.clone(), lines.line(start)));
                },
            }
        }
    }

    // A file the settings star-import or include is read as settings too:
    // an entry, though the import graph may not resolve the import (wger's
    // relative import outside a package, #1239) and never sees an include.
    contrib
        .entries
        .extend(parts.into_iter().map(|(path, origin)| PluginEntry {
            spec: crate::config::EntrySpec { path, symbol: None },
            context: FileContext::Runtime,
            origin,
        }));

    // What the settings did not set may come from a module they star-import
    // or a file they run.
    if EXPLAINING_FIELDS
        .iter()
        .all(|field| assigned.contains(field))
    {
        return installed;
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
    installed
}

// Django's import_string uses a final dot; retain the colon form already read
// by the plugin for existing configurations.
fn application_target(target: &str) -> Option<(String, String)> {
    parse_module_symbol(target).or_else(|| {
        if !is_dotted_identifier(target) {
            return None;
        }
        let (module, symbol) = target.rsplit_once('.')?;
        Some((module.to_owned(), symbol.to_owned()))
    })
}

/// Calls that run other files into the settings namespace: django-split-settings'
/// `include()`, `exec()` and `runpy.run_path()`, by name or as an attribute.
/// Only an `include()` of string literals is read ([`include_patterns`]).
const SETTINGS_CALLS: &[&str] = &["include", "exec", "run_path"];

/// Builtins that return the settings namespace when called with no arguments;
/// `.update()` on one adds names the plugin cannot see.
const NAMESPACE_CALLS: &[&str] = &["globals", "vars", "locals"];

/// The calls that run when the settings are imported and add to their
/// namespace.
#[derive(Default)]
struct SettingsCalls {
    /// The first [`SETTINGS_CALLS`] call not read or namespace `.update()`,
    /// shown as `name()` or `globals().update()`, and where it starts.
    unread: Option<(String, TextSize)>,
    /// Each `include()` read: its patterns, whether each is `optional()`,
    /// and where it starts (#938).
    includes: Vec<(Vec<(String, bool)>, TextSize)>,
}

/// The [`SettingsCalls`] of `stmts`. Module-level blocks (`if`, `try`,
/// `with`) count; function, class and lambda bodies do not run on import
/// (#925, #937). `update()` on anything else, such as a dict or `vars(obj)`,
/// is not the namespace and stays unread.
fn settings_calls(stmts: &[Stmt]) -> SettingsCalls {
    struct Calls(SettingsCalls);

    impl Visitor<'_> for Calls {
        fn visit_stmt(&mut self, stmt: &Stmt) {
            if !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                walk_stmt(self, stmt);
            }
        }

        fn visit_expr(&mut self, expr: &Expr) {
            if matches!(expr, Expr::Lambda(_)) {
                return;
            }
            if let Expr::Call(call) = expr {
                let name = call_name(call);
                if let Some(name) = name.filter(|name| SETTINGS_CALLS.contains(name)) {
                    match include_patterns(call).filter(|_| name == "include") {
                        Some(patterns) => self.0.includes.push((patterns, call.start())),
                        None => {
                            self.0
                                .unread
                                .get_or_insert_with(|| (format!("{name}()"), call.start()));
                        },
                    }
                    return;
                }
                if let Some(namespace) = namespace_update(call) {
                    self.0
                        .unread
                        .get_or_insert_with(|| (format!("{namespace}().update()"), call.start()));
                    return;
                }
            }
            walk_expr(self, expr);
        }
    }

    let mut calls = Calls(SettingsCalls::default());
    calls.visit_body(stmts);
    calls.0
}

/// The name a call is made by: `include` for `include(...)` and
/// `split_settings.tools.include(...)`.
fn call_name(call: &ExprCall) -> Option<&str> {
    match &*call.func {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        _ => None,
    }
}

/// The patterns an `include()` runs, each a string literal or one wrapped in
/// `optional()`; `None` when any argument is another form or a keyword
/// (`scope=` runs the files in another namespace), so the call stays a gap.
fn include_patterns(call: &ExprCall) -> Option<Vec<(String, bool)>> {
    if !call.arguments.keywords.is_empty() || call.arguments.args.is_empty() {
        return None;
    }
    call.arguments
        .args
        .iter()
        .map(|arg| {
            if let Some(pattern) = string_value(arg) {
                return Some((pattern, false));
            }
            let Expr::Call(inner) = arg else {
                return None;
            };
            let [pattern] = &*inner.arguments.args else {
                return None;
            };
            (call_name(inner) == Some("optional") && inner.arguments.keywords.is_empty())
                .then(|| string_value(pattern))
                .flatten()
                .map(|pattern| (pattern, true))
        })
        .collect()
}

/// The files an `include()` pattern in `rel` runs, as django-split-settings
/// finds them: `glob.glob` of the pattern joined to `rel`'s directory, in
/// sorted order. `None` when the pattern climbs out of the root or is
/// absolute.
fn include_files(root: &Path, rel: &str, pattern: &str) -> Option<Vec<String>> {
    let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let joined = super::util::join_lexical(dir, pattern)?;
    let mut found = vec![String::new()];
    let parts: Vec<&str> = joined.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let is_kind = |path: &Path| if last { path.is_file() } else { path.is_dir() };
        let join = |base: &str, name: &str| {
            if base.is_empty() {
                name.to_owned()
            } else {
                format!("{base}/{name}")
            }
        };
        if !part.contains(['*', '?', '[']) {
            found = found
                .iter()
                .map(|base| join(base, part))
                .filter(|path| is_kind(&abs_path(root, path)))
                .collect();
            continue;
        }
        let matcher = globset::GlobBuilder::new(part)
            .literal_separator(true)
            .build()
            .ok()?
            .compile_matcher();
        let mut next = Vec::new();
        for base in &found {
            let Ok(entries) = std::fs::read_dir(abs_path(root, base)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                // `glob` leaves hidden names to a pattern that starts with a dot.
                let hidden = name.starts_with('.') && !part.starts_with('.');
                if !hidden && matcher.is_match(&name) && is_kind(&entry.path()) {
                    next.push(join(base, &name));
                }
            }
        }
        found = next;
    }
    found.sort();
    Some(found)
}

/// The [`NAMESPACE_CALLS`] builtin `call` updates, as in `globals().update(...)`.
fn namespace_update(call: &ExprCall) -> Option<&str> {
    let Expr::Attribute(attribute) = &*call.func else {
        return None;
    };
    let Expr::Call(receiver) = &*attribute.value else {
        return None;
    };
    let Expr::Name(name) = &*receiver.func else {
        return None;
    };
    (attribute.attr.as_str() == "update"
        && receiver.arguments.is_empty()
        && NAMESPACE_CALLS.contains(&name.id.as_str()))
    .then_some(name.id.as_str())
}

/// Top-level `from m import *` statements: the absolute module, `None` when a
/// relative one does not resolve, the file a relative one names beside `rel`
/// on disk, and where the statement starts. The file covers a settings
/// directory discovery did not take as a package (wger's `settings/`, #1065).
fn star_imports(
    stmts: &[Stmt],
    rel: &str,
    root: &Path,
    ctx: &PluginContext<'_>,
) -> Vec<(Option<String>, Option<String>, TextSize)> {
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
            let beside = (level > 0)
                .then(|| relative_path(root, rel, level, suffix.as_deref()))
                .flatten();
            Some((module, beside, import.start()))
        })
        .collect()
}

/// The project file a relative import in `rel` names, found from `rel`'s own
/// directory: `level` 1 is that directory, each further level its parent.
/// `None` when it climbs past the root or names no file.
fn relative_path(root: &Path, rel: &str, level: u8, suffix: Option<&str>) -> Option<String> {
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop();
    for _ in 1..level {
        parts.pop()?;
    }
    let suffix = suffix.filter(|suffix| !suffix.is_empty());
    parts.extend(suffix.into_iter().flat_map(|suffix| suffix.split('.')));
    if parts.is_empty() {
        return None;
    }
    let target = parts.join("/");
    let module = suffix.map(|_| format!("{target}.py"));
    module
        .into_iter()
        .chain([format!("{target}/__init__.py")])
        .find(|path| abs_path(root, path).is_file())
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
/// puts the directory of `manage.py` on `sys.path`, as pytest does for a
/// `conftest.py` and Python for a `runtests.py` script, and `wsgi.py` /
/// `asgi.py` sit in the project package beside it. An environment file
/// (`.env`, a compose file, a Dockerfile) sits where the project starts, as
/// `manage.py` does. A root setter needs none.
fn setter_dir(setter: &str) -> Option<String> {
    let path = Path::new(setter);
    let dir = path.parent();
    let own_dir = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(name, "manage.py" | "conftest.py" | "runtests.py")
                || Path::new(name).extension().is_none_or(|ext| ext != "py")
        });
    let dir = if own_dir {
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

    #[test]
    fn include_files_expand_globs_beside_the_including_file() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        for path in [
            "conf/settings/components/b.py",
            "conf/settings/components/a.py",
            "conf/settings/components/.hidden.py",
            "conf/settings/components/sub/c.py",
            "conf/local.py",
        ] {
            let path = abs_path(root, path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
            std::fs::write(path, "").expect("write file");
        }
        let files = |pattern: &str| include_files(root, "conf/settings/__init__.py", pattern);
        assert_eq!(
            files("components/*.py").expect("pattern"),
            [
                "conf/settings/components/a.py",
                "conf/settings/components/b.py"
            ]
        );
        assert_eq!(
            files("components/.*.py").expect("pattern"),
            ["conf/settings/components/.hidden.py"]
        );
        assert_eq!(
            files("components/[s]*/c.py").expect("pattern"),
            ["conf/settings/components/sub/c.py"]
        );
        assert_eq!(files("../local.py").expect("pattern"), ["conf/local.py"]);
        assert_eq!(files("missing.py").expect("pattern"), Vec::<String>::new());
        assert_eq!(files("components"), Some(Vec::new()));
        assert_eq!(files("../../../outside.py"), None);
        assert_eq!(files("/etc/settings.py"), None);
    }

    #[test]
    fn include_patterns_read_literals_and_optional_only() {
        let patterns = |code: &str| {
            let stmts = parse_module(code).expect("parse");
            let Some(Stmt::Expr(expr)) = stmts.first() else {
                panic!("{code}");
            };
            let Expr::Call(call) = &*expr.value else {
                panic!("{code}");
            };
            include_patterns(call)
        };
        assert_eq!(
            patterns("include('a.py', optional('b.py'), tools.optional('c.py'))"),
            Some(vec![
                ("a.py".to_owned(), false),
                ("b.py".to_owned(), true),
                ("c.py".to_owned(), true),
            ])
        );
        for code in [
            "include()",
            "include(name)",
            "include('a.py', scope=globals())",
            "include(*files)",
            "include(optional(name))",
            "include(wrap('a.py'))",
            "include(optional('a.py', strict=True))",
            "include(f'{ENV}.py')",
        ] {
            assert_eq!(patterns(code), None, "{code}");
        }
    }

    #[test]
    fn setter_dir_is_the_script_dir_or_the_package_parent() {
        assert_eq!(setter_dir("tests/conftest.py").as_deref(), Some("tests"));
        assert_eq!(setter_dir("tests/runtests.py").as_deref(), Some("tests"));
        assert_eq!(setter_dir("site/manage.py").as_deref(), Some("site"));
        assert_eq!(setter_dir("site/proj/wsgi.py").as_deref(), Some("site"));
        assert_eq!(setter_dir("proj/wsgi.py"), None);
        assert_eq!(setter_dir("web/.env").as_deref(), Some("web"));
        assert_eq!(setter_dir("web/Dockerfile").as_deref(), Some("web"));
    }
}
