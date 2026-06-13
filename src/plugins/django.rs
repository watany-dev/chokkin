//! Django plugin extractor.

#![allow(clippy::too_many_lines)]

use crate::config::PluginId;
use crate::manifest::literals::{extract_python_list_literals, extract_python_string_assignment};
use crate::manifest::util::read_to_string;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    FrameworkUsedGlob, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
    SymbolReference,
};
use super::util::{
    choose_settings_path, extract_django_settings_module, find_settings_candidates,
    manifest_has_dependency, module_to_py_path, origin_for_file, parse_module_symbol,
    relative_path, root_join,
};
use super::warnings::PluginsWarning;

const LIST_FIELDS: &[&str] = &["INSTALLED_APPS", "MIDDLEWARE"];

/// Extract Django-related plugin hints.
pub fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Django);
    let mut warnings = Vec::new();
    let root = ctx.root.path.as_path();
    let manage_py = root.join("manage.py");

    let mut settings_module: Option<String> = None;

    if manage_py.is_file() {
        let rel = relative_path(root, &manage_py);
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec {
                path: rel.clone(),
                symbol: None,
            },
            context: FileContext::Runtime,
            origin: ReferenceOrigin {
                file: rel,
                line: None,
                label: "manage.py".to_owned(),
            },
        });

        if let Ok(contents) = read_to_string(&manage_py) {
            settings_module = extract_django_settings_module(&contents);
            if let Some(module) = &settings_module {
                contrib.module_refs.push(ModuleReference {
                    module: module.clone(),
                    origin: ReferenceOrigin {
                        file: relative_path(root, &manage_py),
                        line: None,
                        label: "DJANGO_SETTINGS_MODULE".to_owned(),
                    },
                });
            }
        }
    } else if !manifest_has_dependency(ctx.manifest, "django") {
        warnings.push(PluginsWarning::PluginNoOp {
            plugin: PluginId::Django,
        });
        return (contrib, warnings);
    }

    let candidates = find_settings_candidates(root);
    let (settings_path, ambiguous) = choose_settings_path(
        &candidates,
        settings_module.as_deref(),
        ctx.manifest.metadata.name.as_deref(),
    );

    if ambiguous && let (Some(chosen), true) = (&settings_path, candidates.len() > 1) {
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

    let Some(settings_rel) = settings_path else {
        if manage_py.is_file() {
            warnings.push(PluginsWarning::PluginNoOp {
                plugin: PluginId::Django,
            });
        }
        return (contrib, warnings);
    };

    let settings_path_abs = root_join(root, &settings_rel);
    contrib.entries.push(PluginEntry {
        spec: crate::config::EntrySpec {
            path: settings_rel.clone(),
            symbol: None,
        },
        context: FileContext::Runtime,
        origin: ReferenceOrigin {
            file: settings_rel.clone(),
            line: None,
            label: "settings.py".to_owned(),
        },
    });

    if let Ok(contents) = read_to_string(&settings_path_abs) {
        let lists = extract_python_list_literals(&contents, LIST_FIELDS);
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
                path: settings_rel.clone(),
                fields: partial,
            });
        }

        for (field, scan) in &lists {
            for module in &scan.values {
                contrib.module_refs.push(ModuleReference {
                    module: module.clone(),
                    origin: ReferenceOrigin {
                        file: settings_rel.clone(),
                        line: None,
                        label: field.clone(),
                    },
                });
            }
        }

        if let Some(urlconf) = extract_python_string_assignment(&contents, "ROOT_URLCONF") {
            contrib.module_refs.push(ModuleReference {
                module: urlconf.clone(),
                origin: ReferenceOrigin {
                    file: settings_rel.clone(),
                    line: None,
                    label: "ROOT_URLCONF".to_owned(),
                },
            });
            let urls_path = module_to_py_path(&urlconf);
            if ctx.sources.files.iter().any(|f| f.path == urls_path) {
                contrib.entries.push(PluginEntry {
                    spec: crate::config::EntrySpec {
                        path: urls_path,
                        symbol: None,
                    },
                    context: FileContext::Runtime,
                    origin: ReferenceOrigin {
                        file: settings_rel.clone(),
                        line: None,
                        label: "ROOT_URLCONF".to_owned(),
                    },
                });
            }
        }

        for field in ["WSGI_APPLICATION", "ASGI_APPLICATION"] {
            if let Some(target) = extract_python_string_assignment(&contents, field)
                && let Some((module, symbol)) = parse_module_symbol(&target)
            {
                contrib.symbol_refs.push(SymbolReference {
                    module,
                    symbol,
                    origin: ReferenceOrigin {
                        file: settings_rel.clone(),
                        line: None,
                        label: field.to_owned(),
                    },
                });
            }
        }
    }

    contrib.framework_used_globs.push(FrameworkUsedGlob {
        pattern: "**/migrations/**/*.py".to_owned(),
        origin: origin_for_file(root, &settings_path_abs, "Django migrations"),
    });

    (contrib, warnings)
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
