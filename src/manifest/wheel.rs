//! Wheel target configuration → distributed package paths (R-05).
//!
//! Only explicitly written build backend tables count. Backend defaults
//! (auto-discovery) are left to layout inference, so a project without any of
//! these tables keeps the pre-R-05 public surface.

use toml::Value;

use crate::path_util::join_rel;

use super::types::{PackageFind, WheelTargets};

/// Parse the wheel target tables of `pyproject.toml`.
///
/// When `build-backend` names a known backend only that backend's table is
/// read, so stale tables of another tool do not narrow the surface.
pub(super) fn parse_wheel_targets(
    table: &toml::Table,
    backend: Option<&str>,
    project_name: Option<&str>,
) -> Option<WheelTargets> {
    let tool = table.get("tool").and_then(Value::as_table)?;
    let prefix = backend
        .and_then(|name| name.split(['.', ':']).next())
        .filter(|prefix| BACKENDS.contains(prefix));
    match prefix {
        Some(name) => backend_targets(name, tool, project_name),
        None => BACKENDS
            .iter()
            .find_map(|name| backend_targets(name, tool, project_name)),
    }
}

/// First dotted component of each supported `build-backend`.
const BACKENDS: [&str; 5] = ["hatchling", "setuptools", "pdm", "flit_core", "maturin"];

fn backend_targets(
    backend: &str,
    tool: &toml::Table,
    project_name: Option<&str>,
) -> Option<WheelTargets> {
    match backend {
        "hatchling" => hatch_targets(tool),
        "setuptools" => setuptools_targets(tool),
        "pdm" => pdm_targets(tool),
        "flit_core" => flit_targets(tool),
        "maturin" => maturin_targets(tool, project_name),
        _ => None,
    }
}

fn table_at<'a>(table: &'a toml::Table, keys: &[&str]) -> Option<&'a toml::Table> {
    keys.iter().try_fold(table, |current, key| {
        current.get(*key).and_then(Value::as_table)
    })
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_path)
                .filter(|path| !path.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Root-relative, `/`-separated, no leading `./` or `/`, no trailing `/`;
/// the root itself is `""`.
fn normalize_path(raw: &str) -> String {
    let unified = raw.trim().replace('\\', "/");
    let mut path = unified.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let path = path.trim_matches('/');
    if path == "." {
        String::new()
    } else {
        path.to_owned()
    }
}

fn targets(source: &str, paths: Vec<String>, find: Vec<PackageFind>) -> Option<WheelTargets> {
    if paths.is_empty() && find.is_empty() {
        return None;
    }
    Some(WheelTargets {
        source: source.to_owned(),
        paths,
        find,
    })
}

/// `packages` / `only-include`; the wheel target overrides `[tool.hatch.build]`.
/// `sources` only rewrites install paths, so it never changes which files ship.
fn hatch_targets(tool: &toml::Table) -> Option<WheelTargets> {
    [
        (
            &["hatch", "build", "targets", "wheel"][..],
            "tool.hatch.build.targets.wheel",
        ),
        (&["hatch", "build"][..], "tool.hatch.build"),
    ]
    .into_iter()
    .find_map(|(keys, source)| {
        let section = table_at(tool, keys)?;
        let mut paths = string_array(section.get("packages"));
        paths.extend(string_array(section.get("only-include")));
        targets(source, paths, Vec::new())
    })
}

fn setuptools_targets(tool: &toml::Table) -> Option<WheelTargets> {
    let section = tool.get("setuptools").and_then(Value::as_table)?;
    let package_dir = setuptools_package_dir(section);
    let mut paths = Vec::new();
    let mut find = Vec::new();
    match section.get("packages") {
        Some(Value::Array(items)) => paths.extend(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|package| setuptools_package_path(package, &package_dir)),
        ),
        Some(Value::Table(packages)) => {
            find.extend(
                packages
                    .get("find")
                    .and_then(Value::as_table)
                    .map(package_find),
            );
        },
        _ => {},
    }
    let modules = section.get("py-modules").and_then(Value::as_array);
    paths.extend(
        modules
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|module| format!("{}.py", setuptools_package_path(module, &package_dir))),
    );
    targets("tool.setuptools", paths, find)
}

fn setuptools_package_dir(section: &toml::Table) -> Vec<(String, String)> {
    section
        .get("package-dir")
        .and_then(Value::as_table)
        .map(|dirs| {
            dirs.iter()
                .filter_map(|(package, dir)| {
                    dir.as_str()
                        .map(|dir| (package.clone(), normalize_path(dir)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Map a dotted package to its directory through `package-dir`: an exact key,
/// then the longest parent key, then the `""` root.
fn setuptools_package_path(package: &str, package_dir: &[(String, String)]) -> String {
    let mut prefix = package;
    loop {
        if let Some((_, dir)) = package_dir.iter().find(|(key, _)| key == prefix) {
            let rest = package
                .strip_prefix(prefix)
                .unwrap_or_default()
                .trim_start_matches('.');
            if rest.is_empty() {
                return dir.clone();
            }
            return join_rel(dir, &rest.replace('.', "/"));
        }
        match prefix.rfind('.') {
            Some(index) => prefix = &prefix[..index],
            None => break,
        }
    }
    let base = package_dir
        .iter()
        .find(|(key, _)| key.is_empty())
        .map_or("", |(_, dir)| dir.as_str());
    join_rel(base, &package.replace('.', "/"))
}

fn package_find(spec: &toml::Table) -> PackageFind {
    let where_dirs = string_array(spec.get("where"));
    let include = string_array(spec.get("include"));
    PackageFind {
        where_dirs: if where_dirs.is_empty() {
            vec![String::new()]
        } else {
            where_dirs
        },
        include: if include.is_empty() {
            vec!["*".to_owned()]
        } else {
            include
        },
        exclude: string_array(spec.get("exclude")),
        namespaces: spec
            .get("namespaces")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    }
}

fn pdm_targets(tool: &toml::Table) -> Option<WheelTargets> {
    let section = table_at(tool, &["pdm", "build"])?;
    targets(
        "tool.pdm.build",
        string_array(section.get("includes")),
        Vec::new(),
    )
}

fn flit_targets(tool: &toml::Table) -> Option<WheelTargets> {
    let name = table_at(tool, &["flit", "module"])?
        .get("name")
        .and_then(Value::as_str)?;
    let module = name.replace('.', "/");
    let paths = ["", "src"]
        .iter()
        .flat_map(|base| {
            [
                join_rel(base, &module),
                join_rel(base, &format!("{module}.py")),
            ]
        })
        .collect();
    targets("tool.flit.module", paths, Vec::new())
}

/// `module-name = "pkg._core"` ships the Python package `pkg`.
fn maturin_targets(tool: &toml::Table, project_name: Option<&str>) -> Option<WheelTargets> {
    let section = tool.get("maturin").and_then(Value::as_table)?;
    let python_source = section.get("python-source").and_then(Value::as_str);
    let module_name = section.get("module-name").and_then(Value::as_str);
    if python_source.is_none() && module_name.is_none() {
        return None;
    }
    let package = module_name
        .and_then(|name| name.split('.').next())
        .map(str::to_owned)
        .or_else(|| project_name.map(|name| name.replace(['-', '.'], "_")))?;
    let base = python_source.map(normalize_path).unwrap_or_default();
    targets(
        "tool.maturin",
        vec![
            join_rel(&base, &package),
            join_rel(&base, &format!("{package}.py")),
        ],
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str, backend: Option<&str>) -> Option<WheelTargets> {
        let table: toml::Table = toml::from_str(contents).expect("valid toml");
        parse_wheel_targets(&table, backend, Some("acme-lib"))
    }

    #[test]
    fn hatch_wheel_packages_and_only_include() {
        let targets = parse(
            "[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\"]\nonly-include = [\"./extra.py\"]\n",
            Some("hatchling.build"),
        )
        .expect("targets");
        assert_eq!(targets.source, "tool.hatch.build.targets.wheel");
        assert_eq!(targets.paths, vec!["src/acme", "extra.py"]);
    }

    #[test]
    fn hatch_without_packages_falls_back_to_layout() {
        assert!(
            parse(
                "[tool.hatch.build.targets.wheel]\nsources = [\"src\"]\n",
                Some("hatchling.build")
            )
            .is_none()
        );
    }

    #[test]
    fn hatch_wheel_without_packages_inherits_global_build() {
        let targets = parse(
            "[tool.hatch.build]\npackages = [\"src/acme\"]\n[tool.hatch.build.targets.wheel]\nsources = [\"src\"]\n",
            Some("hatchling.build"),
        )
        .expect("targets");
        assert_eq!(targets.source, "tool.hatch.build");
        assert_eq!(targets.paths, vec!["src/acme"]);
    }

    #[test]
    fn setuptools_explicit_packages_use_package_dir() {
        let targets = parse(
            "[tool.setuptools]\npackages = [\"acme\", \"acme.sub\", \"plugins.x\"]\npy-modules = [\"single\"]\n[tool.setuptools.package-dir]\n\"\" = \"src\"\nplugins = \"third/plugins\"\n",
            Some("setuptools.build_meta"),
        )
        .expect("targets");
        assert_eq!(
            targets.paths,
            vec![
                "src/acme",
                "src/acme/sub",
                "third/plugins/x",
                "src/single.py"
            ]
        );
    }

    #[test]
    fn setuptools_find_defaults() {
        let targets = parse(
            "[tool.setuptools.packages.find]\nwhere = [\"src\"]\nexclude = [\"acme.tests*\"]\n",
            None,
        )
        .expect("targets");
        assert_eq!(
            targets.find,
            vec![PackageFind {
                where_dirs: vec!["src".to_owned()],
                include: vec!["*".to_owned()],
                exclude: vec!["acme.tests*".to_owned()],
                namespaces: true,
            }]
        );
    }

    #[test]
    fn pdm_includes_are_paths_or_globs() {
        let targets = parse(
            "[tool.pdm.build]\nincludes = [\"src/acme\", \"tools/*.py\"]\n",
            Some("pdm.backend"),
        )
        .expect("targets");
        assert_eq!(targets.paths, vec!["src/acme", "tools/*.py"]);
    }

    #[test]
    fn flit_module_candidates() {
        let targets = parse(
            "[tool.flit.module]\nname = \"acme\"\n",
            Some("flit_core.buildapi"),
        )
        .expect("targets");
        assert_eq!(
            targets.paths,
            vec!["acme", "acme.py", "src/acme", "src/acme.py"]
        );
    }

    #[test]
    fn maturin_python_source_and_module_name() {
        let targets = parse(
            "[tool.maturin]\npython-source = \"python\"\nmodule-name = \"acme._core\"\n",
            Some("maturin"),
        )
        .expect("targets");
        assert_eq!(targets.paths, vec!["python/acme", "python/acme.py"]);
    }

    #[test]
    fn maturin_defaults_module_to_project_name() {
        let targets = parse(
            "[tool.maturin]\npython-source = \"python\"\n",
            Some("maturin"),
        )
        .expect("targets");
        assert_eq!(targets.paths, vec!["python/acme_lib", "python/acme_lib.py"]);
    }

    #[test]
    fn known_backend_ignores_other_tool_tables() {
        assert!(
            parse(
                "[tool.setuptools]\npackages = [\"acme\"]\n",
                Some("hatchling.build")
            )
            .is_none()
        );
        assert!(parse("[tool.setuptools]\npackages = [\"acme\"]\n", Some("custom")).is_some());
    }
}
