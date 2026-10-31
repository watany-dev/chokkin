//! Requirements files that install a separate environment for one subtree of
//! a package (#1340).
//!
//! `KiroCrew` keeps `runtime/container/requirements.txt`, which its Dockerfile
//! installs into the container that runs `container/front/`, and
//! `local_servers/laya.requirements.txt`, which a launcher installs into the
//! model server's own venv. Those distributions are not the package's: adding
//! them to the root manifest would be wrong, and merging the files into the
//! root declarations would hide what the rest of the package imports without
//! declaring. So each file covers only the sources below its own directory,
//! like a build script's `[build-system].requires` (#735): those imports need
//! no project declaration, but still count as uses.
//!
//! A directory qualifies only strictly inside a package directory
//! (`src/pkg/sub/`); a file in the package directory itself or above it is
//! the project's own (`src/pkg/requirements.txt`, read by `setup.py`), and
//! the root and its `requirements/` / `docs/` files are the manifest's
//! (spec §4). A file whose name names a dev, test or docs context
//! (`requirements-dev.txt`) installs no runtime and covers nothing, and a
//! `.txt` beside a same-name `.in` is pip-compile output: only the `.in`
//! declares (spec §4, #786). Who
//! installs the file (a Dockerfile, a launcher) is not read: the file's
//! place is the only evidence. A file that cannot be read covers nothing, so
//! its subtree's imports stay checked against the root declarations.

use std::collections::{BTreeMap, BTreeSet};

use crate::manifest::{
    DependencyContext, LoadedManifest, referenced_requirement_names, requirements_files_in,
    requirements_path_context,
};
use crate::path_util::join_rel;
use crate::sources::DiscoveredSources;

/// Each source file below a subtree requirements file → the normalized names
/// those files list.
pub(super) fn environment_requirements(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> BTreeMap<String, BTreeSet<String>> {
    let packages: Vec<String> = sources
        .layout
        .packages
        .iter()
        .map(|package| sources.layout.package_dir(package))
        .chain(sources.layout.members.iter().flat_map(|member| {
            member
                .layout
                .packages
                .iter()
                .map(|package| join_rel(&member.path, &member.layout.package_dir(package)))
        }))
        .collect();
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for file in &sources.files {
        let mut dir = file.path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            dir = parent;
            if !packages
                .iter()
                .any(|package| is_strictly_below(dir, package))
            {
                break;
            }
            if !dirs.insert(dir) {
                break;
            }
        }
    }
    let root = manifest.root.path.as_path();
    let mut provided: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for dir in dirs {
        let files = requirements_files_in(&root.join(dir));
        for path in &files {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // pip-compile output pins the whole environment; its `.in` declares.
            let compiled = path.extension().is_some_and(|ext| ext == "txt")
                && files.contains(&path.with_extension("in"));
            if compiled || !installs_runtime(name) {
                continue;
            }
            if let Some(names) = referenced_requirement_names(manifest, &format!("{dir}/{name}")) {
                provided.entry(dir).or_default().extend(names);
            }
        }
    }
    provided.retain(|_, names| !names.is_empty());
    let mut by_file: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &sources.files {
        for (dir, names) in &provided {
            if is_strictly_below(&file.path, dir) {
                by_file
                    .entry(file.path.clone())
                    .or_default()
                    .extend(names.iter().cloned());
            }
        }
    }
    by_file
}

fn is_strictly_below(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn installs_runtime(name: &str) -> bool {
    matches!(
        requirements_path_context(name),
        None | Some(DependencyContext::Runtime | DependencyContext::OptionalExtra(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_test_and_docs_files_install_no_runtime() {
        for name in [
            "requirements.txt",
            "laya.requirements.txt",
            "strands-decider-2b.requirements.txt",
            "requirements.in",
            "requirements-optional.txt",
        ] {
            assert!(installs_runtime(name), "{name}");
        }
        for name in [
            "requirements-dev.txt",
            "requirements-test.txt",
            "test-requirements.txt",
            "requirements-docs.txt",
        ] {
            assert!(!installs_runtime(name), "{name}");
        }
    }

    #[test]
    fn strictly_below_needs_a_separator() {
        assert!(is_strictly_below("src/pkg/sub/a.py", "src/pkg/sub"));
        assert!(!is_strictly_below("src/pkg/subway/a.py", "src/pkg/sub"));
        assert!(!is_strictly_below("src/pkg/sub", "src/pkg/sub"));
    }
}
