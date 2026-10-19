//! Airflow plugin: the DAG files the DAG processor loads by path from the
//! DAG folder (#401).

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
        let load_all = configured.is_some_and(|folder| folder.load_all);
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

/// `[core] dags_folder` from `airflow.cfg` in the root and each workspace
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
        let load_all = ini_value(&contents, "core", "dag_discovery_safe_mode")
            .is_some_and(|(_, value)| is_false(&value));
        let Some((index, value)) = ini_value(&contents, "core", "dags_folder") else {
            continue;
        };
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
    folders
}

fn dags_folder(config_dir: &str, value: &str) -> Option<String> {
    let mut value = value.trim().to_owned();
    for home in ["${AIRFLOW_HOME}", "$AIRFLOW_HOME", "{AIRFLOW_HOME}"] {
        value = value.replace(home, ".");
    }
    if value.is_empty()
        || value.contains(['$', '{', '%', '~'])
        || value.starts_with(['/', '\\'])
        || value.get(1..2) == Some(":")
    {
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

    #[test]
    fn is_false_reads_config_booleans() {
        assert!(is_false("False"));
        assert!(is_false(" 0 "));
        assert!(!is_false("True"));
    }
}
