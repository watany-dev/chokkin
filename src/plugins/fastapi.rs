//! `FastAPI` / uvicorn plugin extractor.

#![allow(clippy::too_many_lines)]

use crate::config::PluginId;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    BinaryUsage, PluginContribution, PluginEntry, ReferenceOrigin, SymbolReference,
};
use super::util::{
    origin_for_file, parse_module_symbol, parse_uvicorn_script_target, read_pyproject_table,
    uvicorn_tool_from_pyproject,
};
use super::warnings::PluginsWarning;

/// App modules directly under `src/` (`src/main.py` with `src/` as the import
/// root). §8 auto-detection already covers root `main.py` / `app.py` /
/// `asgi.py` / `wsgi.py` and the same names inside a `src/<package>/`, but not
/// top-level modules of `src/` itself; this list adds only that gap.
const SRC_APP_ENTRY_CANDIDATES: &[&str] = &["src/asgi.py", "src/main.py"];

/// Extract `FastAPI` / uvicorn-related plugin hints.
pub(super) fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Fastapi);
    let mut warnings = Vec::new();
    let root = ctx.root.path.as_path();
    let pyproject_path = root.join("pyproject.toml");

    if pyproject_path.is_file() {
        match read_pyproject_table(&pyproject_path) {
            Ok(table) => {
                if let Some(uvicorn) = uvicorn_tool_from_pyproject(&table) {
                    let origin = origin_for_file(root, &pyproject_path, "tool.uvicorn");
                    if let Some(app) = uvicorn.get("app").and_then(|v| v.as_str())
                        && let Some((module, symbol)) = parse_module_symbol(app)
                    {
                        contrib.symbol_refs.push(SymbolReference {
                            module,
                            symbol,
                            origin: origin.clone(),
                        });
                    }
                    contrib.binary_usages.push(BinaryUsage {
                        binary: "uvicorn".to_owned(),
                        origin,
                    });
                }

                if let Some(scripts) = table
                    .get("project")
                    .and_then(|v| v.as_table())
                    .and_then(|project| project.get("scripts"))
                    .and_then(|v| v.as_table())
                {
                    for (name, target) in scripts {
                        if let Some(target_str) = target.as_str()
                            && let Some((module, symbol)) = parse_uvicorn_script_target(target_str)
                        {
                            contrib.symbol_refs.push(SymbolReference {
                                module,
                                symbol,
                                origin: ReferenceOrigin {
                                    file: "pyproject.toml".to_owned(),
                                    line: None,
                                    label: format!("project.scripts.{name}"),
                                },
                            });
                            contrib.binary_usages.push(BinaryUsage {
                                binary: "uvicorn".to_owned(),
                                origin: ReferenceOrigin {
                                    file: "pyproject.toml".to_owned(),
                                    line: None,
                                    label: format!("project.scripts.{name}"),
                                },
                            });
                        }
                    }
                }
            },
            Err(error) => {
                warnings.push(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Fastapi,
                    detail: error.to_string(),
                });
                return (contrib, warnings);
            },
        }
    }

    for &candidate in SRC_APP_ENTRY_CANDIDATES {
        if ctx.sources.files.iter().any(|file| file.path == candidate) {
            contrib.entries.push(PluginEntry {
                spec: crate::config::EntrySpec {
                    path: candidate.to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: ReferenceOrigin {
                    file: candidate.to_owned(),
                    line: None,
                    label: "auto-detected entry".to_owned(),
                },
            });
        }
    }

    (contrib, warnings)
}
