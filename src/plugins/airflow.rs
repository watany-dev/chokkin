//! Airflow plugin: the DAG files the DAG processor loads by path from the
//! DAG folder and local DAG bundles (#401, #1058).

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::path_util::rel_to_root;
use crate::sources::{FileContext, assign_file_context};

use super::config_text::{ini_value, origin_at};
use super::context::PluginContext;
use super::types::{PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{join_lexical, origin_for_file, project_dirs, walk_py};

/// A DAG folder: root-relative directory and whether every `.py` file in it
/// loads (`dag_discovery_safe_mode = False`) or only those that look like
/// DAG files.
struct DagFolder {
    dir: String,
    load_all: bool,
    origin: ReferenceOrigin,
}

#[must_use]
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let root = ctx.root.path.as_path();
    let mut contrib = PluginContribution::empty(PluginId::Airflow);
    let folders = config_folders(ctx, &mut contrib);
    // The inventory, then the DAG folders on disk: a `dags/` with no
    // `__init__.py` is no package, so the inferred globs leave it out
    // (#932). A DAG folder at the root is discovery's own tree; walking it
    // would read the virtualenvs and build output discovery excludes.
    let mut files: Vec<(String, FileContext)> = ctx
        .sources
        .python_files()
        .map(|file| (file.path.clone(), file.context))
        .collect();
    let mut on_disk = BTreeSet::new();
    let default_dirs = project_dirs(ctx);
    for dir in default_dirs
        .filter_map(|dir| join_lexical(dir, "dags"))
        .chain(folders.iter().map(|folder| folder.dir.clone()))
        .filter(|dir| !dir.is_empty())
    {
        walk_py(root, &dir, &mut on_disk);
    }
    files.extend(
        on_disk
            .into_iter()
            .filter(|path| !ctx.sources.files.iter().any(|file| file.path == *path))
            .map(|path| {
                let context = assign_file_context(&path);
                (path, context)
            }),
    );
    let mut seen = BTreeSet::new();
    for (path, context) in &files {
        let configured = folders.iter().find(|folder| under(path, &folder.dir));
        let named = named_dag_dir(path);
        let Some(origin) = configured
            .map(|folder| folder.origin.clone())
            .or_else(|| named.map(|dir| origin_for_file(root, &root.join(dir), "dags folder")))
        else {
            continue;
        };
        // A folder named `dags` is the one tests, e2e suites and SDK examples
        // hand the DAG processor or copy by path (#1064): a file that fails
        // the safe-mode check there (`no_dags.py`, an `.airflowignore` case)
        // is still read, so every `.py` under it is a DAG-folder file.
        let load_all = configured.is_some_and(|folder| folder.load_all) || under_dags_dir(path);
        if (load_all || looks_like_dag_file(&root.join(path))) && seen.insert(path.clone()) {
            contrib.entries.push(PluginEntry {
                spec: EntrySpec {
                    path: path.clone(),
                    symbol: None,
                },
                context: *context,
                origin,
            });
        }
    }
    // The DAG processor loads them by path and collects the DAGs they bind.
    contrib.scanned_files = contrib
        .entries
        .iter()
        .map(|entry| entry.spec.path.clone())
        .collect();
    contrib
}

/// `[core] dags_folder` and the `LocalDagBundle` paths of `[dag_processor]
/// dag_bundle_config_list` from `airflow.cfg` in the root and each workspace
/// member. `{AIRFLOW_HOME}` and a relative path are read from the config's
/// directory, the `AIRFLOW_HOME` airflow finds it in; an absolute path or
/// any other substitution names a folder not known here, so it is a gap.
fn config_folders(ctx: &PluginContext<'_>, contrib: &mut PluginContribution) -> Vec<DagFolder> {
    let root = ctx.root.path.as_path();
    let dirs = project_dirs(ctx);
    let mut folders = Vec::new();
    for dir in dirs {
        let path = root.join(dir).join("airflow.cfg");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = rel_to_root(root, &path);
        // Safe mode is the DAG processor's, so it covers every bundle.
        let load_all = ini_value(&contents, "core", "dag_discovery_safe_mode")
            .is_some_and(|(_, value)| is_false(&value));
        if let Some((index, value)) = ini_value(&contents, "core", "dags_folder") {
            let origin = origin_at(&file, index, "dags_folder");
            match dags_folder(dir, &value) {
                Some(dir) => folders.push(DagFolder {
                    dir,
                    load_all,
                    origin,
                }),
                None => contrib.push_gap(&value, origin),
            }
        }
        if let Some((index, value)) =
            ini_value(&contents, "dag_processor", "dag_bundle_config_list")
        {
            let origin = origin_at(&file, index, "dag_bundle_config_list");
            for bundle in bundle_folders(dir, &value) {
                match bundle {
                    Ok(dir) => folders.push(DagFolder {
                        dir,
                        load_all,
                        origin: origin.clone(),
                    }),
                    Err(text) => contrib.push_gap(&text, origin.clone()),
                }
            }
        }
    }
    folders
}

const LOCAL_DAG_BUNDLE: &str = "airflow.dag_processing.bundles.local.LocalDagBundle";

/// The DAG folders a `dag_bundle_config_list` value names (Airflow 3,
/// #1058): `Ok(dir)` for each `LocalDagBundle` whose `kwargs.path` reads as
/// [`dags_folder`] does, and `Err(text)` for what could not be read. A value
/// that is not a JSON list, an item without a string `classpath` and an
/// object `kwargs`, and a `path` that is not a string are gaps with their
/// JSON text. A bundle of any other class (`GitDagBundle`, S3) loads DAGs
/// from outside the checkout, so its classpath is a gap, and a `path` that
/// is absolute, a template or outside the config's tree is a gap with the
/// path. An empty value, `null` and `[]` configure no bundle, and a local
/// bundle without a `path` loads `dags_folder`, already read.
fn bundle_folders(config_dir: &str, value: &str) -> Vec<Result<String, String>> {
    use serde_json::Value;
    let value = value.trim();
    let items = match serde_json::from_str::<Value>(value) {
        Ok(Value::Array(items)) => items,
        Ok(Value::Null) => return Vec::new(),
        Err(_) if value.is_empty() => return Vec::new(),
        _ => return vec![Err(value.to_owned())],
    };
    items
        .iter()
        .filter_map(|item| {
            let classpath = item.get("classpath").and_then(Value::as_str);
            let kwargs = item.get("kwargs").and_then(Value::as_object);
            let (Some(classpath), Some(kwargs)) = (classpath, kwargs) else {
                return Some(Err(item.to_string()));
            };
            if classpath != LOCAL_DAG_BUNDLE {
                return Some(Err(classpath.to_owned()));
            }
            match kwargs.get("path") {
                None | Some(Value::Null) => None,
                Some(Value::String(path)) => {
                    Some(dags_folder(config_dir, path).ok_or_else(|| path.clone()))
                },
                Some(_) => Some(Err(item.to_string())),
            }
        })
        .collect()
}

fn dags_folder(config_dir: &str, value: &str) -> Option<String> {
    let mut value = value.trim().to_owned();
    for home in ["${AIRFLOW_HOME}", "$AIRFLOW_HOME", "{AIRFLOW_HOME}"] {
        value = value.replace(home, ".");
    }
    if value.is_empty() || value.contains(['$', '{', '%', '~']) {
        return None;
    }
    join_lexical(config_dir, &value)
}

fn is_false(value: &str) -> bool {
    ["false", "f", "no", "off", "0"].contains(&value.trim().to_lowercase().as_str())
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The nearest directory of `path` named like a DAG folder: `dags`, or
/// `dags_*`, `*_dags` and their `-` forms, as test suites and example
/// projects name the folders they point `dags_folder` or a `DagBag` at.
fn named_dag_dir(path: &str) -> Option<&str> {
    let (dir, _) = path.rsplit_once('/')?;
    let mut end = dir.len();
    loop {
        let start = dir[..end].rfind('/').map_or(0, |slash| slash + 1);
        let name = &dir[start..end];
        if name == "dags"
            || name.starts_with("dags_")
            || name.starts_with("dags-")
            || name.ends_with("_dags")
            || name.ends_with("-dags")
        {
            return Some(&dir[..end]);
        }
        if start == 0 {
            return None;
        }
        end = start - 1;
    }
}

/// Whether a directory of `path` is named exactly `dags`, Airflow's default
/// DAG folder name. The partial names [`named_dag_dir`] also accepts
/// (`example_dags` subpackages, `dags_corrupted`) keep the safe-mode check.
fn under_dags_dir(path: &str) -> bool {
    path.rsplit_once('/')
        .is_some_and(|(dir, _)| dir.split('/').any(|name| name == "dags"))
}

/// The DAG processor's safe mode: it loads a file only when its text holds
/// both `airflow` and `dag`, case-insensitively.
fn looks_like_dag_file(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| {
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        text.contains("airflow") && text.contains("dag")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_dag_dir_finds_nearest_dag_folder() {
        assert_eq!(named_dag_dir("dags/etl.py"), Some("dags"));
        assert_eq!(named_dag_dir("dags/sub/etl.py"), Some("dags"));
        assert_eq!(
            named_dag_dir("tests/dags_with_system_exit/a.py"),
            Some("tests/dags_with_system_exit")
        );
        assert_eq!(
            named_dag_dir("airflow/example_dags/x.py"),
            Some("airflow/example_dags")
        );
        assert_eq!(named_dag_dir("my-dags/x.py"), Some("my-dags"));
        assert_eq!(named_dag_dir("dagster/x.py"), None);
        assert_eq!(named_dag_dir("etl.py"), None);
        assert_eq!(named_dag_dir("src/daggy/x.py"), None);
    }

    #[test]
    fn under_dags_dir_needs_a_directory_named_dags() {
        assert!(under_dags_dir("dags/etl.py"));
        assert!(under_dags_dir("tests/unit/dags/subdir2/subdir3/x.py"));
        assert!(under_dags_dir("dags/example_dags/x.py"));
        assert!(!under_dags_dir("dags.py"));
        assert!(!under_dags_dir("tests/dags_with_system_exit/a.py"));
        assert!(!under_dags_dir("airflow/example_dags/x.py"));
        assert!(!under_dags_dir("Dags/etl.py"));
        assert!(!under_dags_dir("src/dags"));
    }

    #[test]
    fn dags_folder_reads_home_and_relative_paths_only() {
        assert_eq!(
            dags_folder("", "{AIRFLOW_HOME}/pipelines").as_deref(),
            Some("pipelines")
        );
        assert_eq!(
            dags_folder("deploy", "$AIRFLOW_HOME/dags").as_deref(),
            Some("deploy/dags")
        );
        assert_eq!(
            dags_folder("", "${AIRFLOW_HOME}/pipelines").as_deref(),
            Some("pipelines")
        );
        assert_eq!(dags_folder("", "flows").as_deref(), Some("flows"));
        assert_eq!(dags_folder("", "/opt/airflow/dags"), None);
        assert_eq!(dags_folder("", "C:\\airflow\\dags"), None);
        assert_eq!(dags_folder("", "${DAGS_DIR}"), None);
        assert_eq!(dags_folder("", "~/dags"), None);
        assert_eq!(dags_folder("", "../outside"), None);
    }

    fn local_bundle(kwargs: &str) -> String {
        format!(r#"[{{"name": "b", "classpath": "{LOCAL_DAG_BUNDLE}", "kwargs": {kwargs}}}]"#)
    }

    #[test]
    fn bundle_folders_reads_local_bundle_paths() {
        assert_eq!(
            bundle_folders("", &local_bundle(r#"{"path": "pipelines"}"#)),
            [Ok("pipelines".to_owned())]
        );
        assert_eq!(
            bundle_folders(
                "deploy",
                &local_bundle(r#"{"path": "$AIRFLOW_HOME/flows"}"#)
            ),
            [Ok("deploy/flows".to_owned())]
        );
        // A local bundle without a `path` loads `dags_folder`.
        assert_eq!(bundle_folders("", &local_bundle("{}")), []);
        assert_eq!(bundle_folders("", &local_bundle(r#"{"path": null}"#)), []);
        assert_eq!(bundle_folders("", ""), []);
        assert_eq!(bundle_folders("", "[]"), []);
        assert_eq!(bundle_folders("", "null"), []);
        // Split over lines, as `ini_value` joins indented continuations.
        let multi = format!(
            "[\n{{\n\"name\": \"a\",\n\"classpath\": \"{LOCAL_DAG_BUNDLE}\",\n\"kwargs\": {{\"path\": \"a\"}}\n}},\n{{\"name\": \"b\", \"classpath\": \"{LOCAL_DAG_BUNDLE}\", \"kwargs\": {{\"path\": \"b/dags\", \"refresh_interval\": 0}}}}\n]"
        );
        assert_eq!(
            bundle_folders("", &multi),
            [Ok("a".to_owned()), Ok("b/dags".to_owned())]
        );
    }

    #[test]
    fn bundle_folders_reports_what_it_cannot_read() {
        let git = r#"[{"name": "g", "classpath": "airflow.providers.git.bundles.git.GitDagBundle", "kwargs": {"subdir": "dags"}}]"#;
        assert_eq!(
            bundle_folders("", git),
            [Err(
                "airflow.providers.git.bundles.git.GitDagBundle".to_owned()
            )]
        );
        for path in [
            "/opt/dags",
            "../outside",
            "${BUNDLE}",
            "~/dags",
            "{{ var }}",
        ] {
            let value = local_bundle(&format!(r#"{{"path": "{path}"}}"#));
            assert_eq!(bundle_folders("", &value), [Err(path.to_owned())], "{path}");
        }
        assert_eq!(bundle_folders("", "dags"), [Err("dags".to_owned())]);
        assert_eq!(
            bundle_folders("", r#"{"name": "b"}"#),
            [Err(r#"{"name": "b"}"#.to_owned())]
        );
        assert_eq!(bundle_folders("", "[1]"), [Err("1".to_owned())]);
        let no_kwargs = format!(r#"[{{"name": "b", "classpath": "{LOCAL_DAG_BUNDLE}"}}]"#);
        assert!(matches!(bundle_folders("", &no_kwargs)[..], [Err(_)]));
        let number = local_bundle(r#"{"path": 1}"#);
        assert!(matches!(bundle_folders("", &number)[..], [Err(_)]));
    }

    #[test]
    fn looks_like_dag_file_needs_both_words() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let check = |text: &str| {
            let path = temp.path().join("f.py");
            std::fs::write(&path, text).expect("write");
            looks_like_dag_file(&path)
        };
        assert!(check("from airflow import DAG\n"));
        assert!(!check("import airflow\n"));
        assert!(!check("dag = build()\n"));
        assert!(!looks_like_dag_file(&temp.path().join("missing.py")));
    }

    #[test]
    fn is_false_reads_config_booleans() {
        assert!(is_false("False"));
        assert!(is_false(" 0 "));
        assert!(!is_false("True"));
    }
}
