//! `pyproject.toml` manifest extraction (`[project]` and entry points).

use std::path::Path;

use toml::Value;

use crate::path_util::rel_to_root;

use super::dependency_groups::extract_dependency_groups;
use super::error::ManifestError;
use super::pep508::canonical_version_specifiers;
use super::pep508_util::{
    normalize_distribution_name, parse_pep508_requirement, pep508_distribution_name,
};
use super::types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, ExtrasState,
    ProjectMetadata, UvToolSettings,
};
use super::util::{DependencyPush, push_dependency, read_to_string};
use super::uv_tool::extract_uv_tool;
use super::warnings::ManifestWarning;
use super::wheel::{
    parse_hatch_build_scripts, parse_in_tree_backend, parse_version_files, parse_wheel_targets,
};

/// Partial extraction result from `pyproject.toml`.
#[derive(Debug, Default)]
pub(super) struct PyprojectExtraction {
    /// Project metadata.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Entry points.
    pub entry_points: Vec<EntryPointDecl>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// `[tool.poetry]` section was detected.
    pub poetry_detected: bool,
    /// `[tool.uv]` constraint / override entries.
    pub constraints: Vec<DeclaredDependency>,
    /// `[tool.uv]` sources.
    pub uv: UvToolSettings,
    /// A `[project]` table is present.
    pub has_project_table: bool,
    /// `dynamic` lists `optional-dependencies`: how much of them this file
    /// lets chokkin read. `Unknown` when nothing here fills them, so
    /// `setup.cfg` or `setup.py` may (#905).
    pub dynamic_extras: Option<ExtrasState>,
}

/// Extract manifest data from `pyproject.toml`.
#[allow(clippy::too_many_lines)]
pub(super) fn extract_pyproject(
    root: &Path,
    path: &Path,
) -> Result<PyprojectExtraction, ManifestError> {
    let contents = read_to_string(path)?;
    let table: toml::Table =
        toml::from_str(&contents).map_err(|source| ManifestError::InvalidToml {
            path: path.to_path_buf(),
            source,
        })?;

    let rel = rel_to_root(root, path);
    let mut result = PyprojectExtraction::default();

    result.poetry_detected = detect_tool_sections(&table, &mut result.warnings);
    extract_tool_dependencies(&table, &rel, &mut result.dependencies, &mut result.warnings);
    let uv = extract_uv_tool(&table, &rel, &mut result.warnings);
    result.dependencies.extend(uv.dependencies);
    result.constraints = uv.constraints;
    result.uv = uv.settings;

    if let Some(project) = table.get("project").and_then(Value::as_table) {
        result.has_project_table = true;
        result.metadata = parse_project_metadata(project);
        let skip_project_dependencies = result
            .metadata
            .dynamic
            .iter()
            .any(|item| item == "dependencies");

        if !skip_project_dependencies
            && let Some(deps) = project.get("dependencies").and_then(Value::as_array)
        {
            push_dependency_array(
                deps,
                &rel,
                &DependencyContext::Runtime,
                "project.dependencies",
                &mut result.dependencies,
                &mut result.warnings,
            );
        }

        if let Some(optional) = project
            .get("optional-dependencies")
            .and_then(Value::as_table)
        {
            push_dependency_table(
                optional,
                &rel,
                DependencyContext::OptionalExtra,
                "project.optional-dependencies",
                &mut result.dependencies,
                &mut result.warnings,
            );
        }

        extract_uv_dynamic_versioning_hook(&table, &rel, &mut result);
        if result
            .metadata
            .dynamic
            .iter()
            .any(|field| field == "optional-dependencies")
        {
            result.dynamic_extras = Some(dynamic_extras_state(&table));
        }

        if let Some(scripts) = project.get("scripts").and_then(Value::as_table) {
            push_entry_point_table(
                scripts,
                &rel,
                "console",
                "project.scripts",
                &mut result.entry_points,
            );
        }

        if let Some(scripts) = project.get("gui-scripts").and_then(Value::as_table) {
            push_entry_point_table(
                scripts,
                &rel,
                "gui",
                "project.gui-scripts",
                &mut result.entry_points,
            );
        }

        if let Some(entry_points) = project.get("entry-points").and_then(Value::as_table) {
            for (group, entries) in entry_points {
                if let Some(entries_table) = entries.as_table() {
                    push_entry_point_table(
                        entries_table,
                        &rel,
                        group,
                        &format!("project.entry-points.{group}"),
                        &mut result.entry_points,
                    );
                }
            }
        }
    }

    poetry_metadata_fallback(&table, &mut result.metadata);
    extract_build_system(&table, &rel, &mut result);

    if let Some(groups) = table.get("dependency-groups").and_then(Value::as_table) {
        extract_dependency_groups(groups, &rel, &mut result.dependencies, &mut result.warnings);
    }

    Ok(result)
}

/// Push every `<name> = ["req", ...]` array of `table`, with `context(name)`.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_dependency_table(
    table: &toml::Table,
    rel: &str,
    context: fn(String) -> DependencyContext,
    label_prefix: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    for (name, deps_value) in table {
        if let Some(deps) = deps_value.as_array() {
            push_dependency_array(
                deps,
                rel,
                &context(name.clone()),
                &format!("{label_prefix}.{name}"),
                dependencies,
                warnings,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_dependency_array(
    deps: &[Value],
    rel: &str,
    context: &DependencyContext,
    label_prefix: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    for (index, dep) in deps.iter().enumerate() {
        if let Some(raw) = dep.as_str() {
            push_dependency(DependencyPush {
                dependencies,
                warnings,
                raw,
                context: context.clone(),
                file: rel,
                label: format!("{label_prefix}[{index}]"),
                line: None,
            });
        }
    }
}

fn push_entry_point_table(
    table: &toml::map::Map<String, Value>,
    rel: &str,
    group: &str,
    label_prefix: &str,
    entry_points: &mut Vec<EntryPointDecl>,
) {
    for (name, target) in table {
        if let Some(target_str) = target.as_str() {
            entry_points.push(EntryPointDecl {
                name: name.clone(),
                target: target_str.to_owned(),
                group: group.to_owned(),
                origin: DependencyOrigin {
                    file: rel.to_owned(),
                    line: None,
                    label: format!("{label_prefix}.{name}"),
                },
            });
        }
    }
}

fn parse_project_metadata(project: &toml::Table) -> ProjectMetadata {
    let name = project
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let version = project
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let requires_python = project
        .get("requires-python")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let string_array = |key: &str| -> Vec<String> {
        project
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    ProjectMetadata {
        name,
        version,
        requires_python,
        dynamic: string_array("dynamic"),
        classifiers: string_array("classifiers"),
        ..ProjectMetadata::default()
    }
}

/// Poetry 1.x declares the distribution under `[tool.poetry]` instead of
/// `[project]` (#974); `[project]` wins when it names one. Other
/// `[tool.poetry]` keys stay unread: `python` is a dependency constraint, not
/// `requires-python`, and nothing downstream reads the rest.
fn poetry_metadata_fallback(table: &toml::Table, metadata: &mut ProjectMetadata) {
    let Some(poetry) = table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("poetry"))
        .and_then(Value::as_table)
    else {
        return;
    };
    let string = |key: &str| poetry.get(key).and_then(Value::as_str).map(str::to_owned);
    if metadata.name.is_none() {
        metadata.name = string("name");
    }
    if metadata.version.is_none() {
        metadata.version = string("version");
    }
}

/// `[build-system]` plus the wheel target tables it selects (R-05).
fn extract_build_system(table: &toml::Table, rel: &str, result: &mut PyprojectExtraction) {
    if let Some(build_system) = table.get("build-system").and_then(Value::as_table) {
        result.metadata.build_backend = build_system
            .get("build-backend")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(requires) = build_system.get("requires").and_then(Value::as_array) {
            push_dependency_array(
                requires,
                rel,
                &DependencyContext::Build,
                "build-system.requires",
                &mut result.metadata.build_requires,
                &mut result.warnings,
            );
        }
    }
    result.metadata.wheel_targets = parse_wheel_targets(
        table,
        result.metadata.build_backend.as_deref(),
        result.metadata.name.as_deref(),
    );
    result.metadata.build_scripts = parse_hatch_build_scripts(table);
    result
        .metadata
        .build_scripts
        .extend(parse_in_tree_backend(table));
    result.metadata.version_files = parse_version_files(table);
}

fn detect_tool_sections(table: &toml::Table, warnings: &mut Vec<ManifestWarning>) -> bool {
    let Some(tool) = table.get("tool").and_then(Value::as_table) else {
        return false;
    };

    let poetry_detected = tool.contains_key("poetry");
    if poetry_detected {
        warnings.push(ManifestWarning::PoetryDetected);
    }
    if tool.contains_key("pdm") {
        warnings.push(ManifestWarning::PdmDetected);
    }
    if tool.contains_key("hatch") {
        warnings.push(ManifestWarning::HatchDetected);
    }
    poetry_detected
}

fn extract_tool_dependencies(
    table: &toml::Table,
    rel: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let Some(tool) = table.get("tool").and_then(Value::as_table) else {
        return;
    };
    extract_poetry_dependencies(tool, rel, dependencies, warnings);
    extract_pdm_dependencies(tool, rel, dependencies, warnings);
    extract_hatch_dependencies(tool, rel, dependencies, warnings);
}

fn extract_poetry_dependencies(
    tool: &toml::Table,
    rel: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let Some(poetry) = tool.get("poetry").and_then(Value::as_table) else {
        return;
    };

    if let Some(runtime) = poetry.get("dependencies").and_then(Value::as_table) {
        let extras = poetry.get("extras").and_then(Value::as_table);
        for (name, dep_value) in runtime {
            // Poetry installs an `optional = true` dependency only through an
            // extra; it is the PEP 621 optional-dependencies equivalent.
            let context = if poetry_is_optional(dep_value) {
                DependencyContext::OptionalExtra(poetry_extra_listing(extras, name))
            } else {
                DependencyContext::Runtime
            };
            push_poetry_dependency(
                dependencies,
                warnings,
                name,
                dep_value,
                context,
                rel,
                format!("tool.poetry.dependencies.{name}"),
            );
        }
    }

    if let Some(dev) = poetry.get("dev-dependencies").and_then(Value::as_table) {
        for (name, dep_value) in dev {
            push_poetry_dependency(
                dependencies,
                warnings,
                name,
                dep_value,
                DependencyContext::Group("dev".to_owned()),
                rel,
                format!("tool.poetry.dev-dependencies.{name}"),
            );
        }
    }

    if let Some(groups) = poetry.get("group").and_then(Value::as_table) {
        for (group, group_value) in groups {
            let Some(group_table) = group_value.as_table() else {
                continue;
            };
            let Some(group_deps) = group_table.get("dependencies").and_then(Value::as_table) else {
                continue;
            };
            for (name, dep_value) in group_deps {
                push_poetry_dependency(
                    dependencies,
                    warnings,
                    name,
                    dep_value,
                    DependencyContext::Group(group.clone()),
                    rel,
                    format!("tool.poetry.group.{group}.dependencies.{name}"),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_poetry_dependency(
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
    name: &str,
    value: &Value,
    context: DependencyContext,
    file: &str,
    label: String,
) {
    if name == "python" {
        return;
    }
    let raw = poetry_requirement_name(name, value);
    let pushed = dependencies.len();
    push_dependency(DependencyPush {
        dependencies,
        warnings,
        raw: &raw,
        context,
        file,
        label,
        line: None,
    });
    if let Some(dep) = dependencies.get_mut(pushed) {
        dep.specifier = poetry_version_constraint(value);
        dep.marker = poetry_marker(value);
    }
}

/// Whether every form of the dependency is `optional = true`.
fn poetry_is_optional(value: &Value) -> bool {
    let optional = |value: &Value| {
        value
            .as_table()
            .and_then(|table| table.get("optional"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    match value {
        Value::Array(forms) => !forms.is_empty() && forms.iter().all(optional),
        _ => optional(value),
    }
}

/// The first `[tool.poetry.extras]` entry that lists `name`, else the name
/// itself: an optional dependency no extra lists is still not installed by
/// default.
fn poetry_extra_listing(extras: Option<&toml::Table>, name: &str) -> String {
    let normalized = normalize_distribution_name(name);
    extras
        .into_iter()
        .flatten()
        .find(|(_, members)| {
            members.as_array().is_some_and(|members| {
                members.iter().filter_map(Value::as_str).any(|member| {
                    pep508_distribution_name(member).is_some_and(|member| member == normalized)
                })
            })
        })
        .map_or_else(|| normalized.clone(), |(extra, _)| extra.clone())
}

/// When Poetry installs the dependency, as a PEP 508 marker: `markers` as
/// written, `platform` as `sys_platform`, `python` converted from a Poetry
/// constraint. Other keys (`source`, `develop`, `allow-prereleases`, `git`,
/// `path`, ...) pick where it comes from, not whether it is installed. A
/// multiple-constraints array is conditional only when every form is.
fn poetry_marker(value: &Value) -> Option<String> {
    let single = |value: &Value| -> Option<String> {
        let table = value.as_table()?;
        let mut parts = Vec::new();
        if let Some(markers) = table.get("markers").and_then(Value::as_str) {
            parts.push(markers.trim().to_owned());
        }
        if let Some(platform) = table.get("platform").and_then(Value::as_str) {
            parts.extend(poetry_platform_marker(platform));
        }
        if let Some(python) = table
            .get("python")
            .and_then(Value::as_str)
            .and_then(poetry_python_marker)
        {
            parts.push(python);
        }
        parts.retain(|part| !part.is_empty());
        match parts.as_slice() {
            [] => None,
            [part] => Some(part.clone()),
            // An `or` inside a part would otherwise bind looser than `and`.
            parts => Some(
                parts
                    .iter()
                    .map(|part| format!("({part})"))
                    .collect::<Vec<_>>()
                    .join(" and "),
            ),
        }
    };
    match value {
        Value::Array(forms) => any_of(&forms.iter().map(single).collect::<Option<Vec<_>>>()?),
        _ => single(value),
    }
}

/// The markers joined with `or`; `None` for none.
fn any_of(markers: &[String]) -> Option<String> {
    match markers {
        [] => None,
        [marker] => Some(marker.clone()),
        markers => Some(
            markers
                .iter()
                .map(|marker| format!("({marker})"))
                .collect::<Vec<_>>()
                .join(" or "),
        ),
    }
}

/// A Poetry `platform` as a PEP 508 marker; `None` for `*`. A constraint
/// (`linux || darwin`, `!=win32`) is kept as `platform <constraint>` like an
/// unrecognized `python` one: conditional, but not written by `--fix`.
fn poetry_platform_marker(platform: &str) -> Option<String> {
    let platform = platform.trim();
    if platform.is_empty() || platform == "*" {
        None
    } else if platform
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some(format!("sys_platform == \"{platform}\""))
    } else {
        Some(format!("platform {platform}"))
    }
}

/// A Poetry `python` constraint as a PEP 508 marker; `None` when it allows
/// every version. A constraint outside the recognized forms is kept as
/// `python <constraint>`: still conditional, so CHK002 does not report the
/// dependency, but not PEP 508, so `--fix` will not write it.
fn poetry_python_marker(constraint: &str) -> Option<String> {
    let constraint = constraint.trim();
    if constraint.is_empty() {
        return None;
    }
    let mut alternatives = Vec::new();
    // Poetry accepts `|` as well as `||` between alternatives.
    for alternative in constraint.split('|').filter(|part| !part.is_empty()) {
        let Some(terms) = poetry_python_terms(alternative) else {
            return Some(format!("python {constraint}"));
        };
        if terms.is_empty() {
            return None;
        }
        alternatives.push(terms.join(" and "));
    }
    any_of(&alternatives)
}

/// The marker terms one `||` alternative of a Poetry constraint ANDs
/// together (empty for `*`), or `None` for a form not recognized.
fn poetry_python_terms(alternative: &str) -> Option<Vec<String>> {
    // Poetry separates AND terms with commas or spaces; an operator may be
    // spaced from its version (`>= 3.8`).
    let mut constraints: Vec<String> = Vec::new();
    for token in alternative
        .split([',', ' '])
        .filter(|token| !token.is_empty())
    {
        match constraints.last_mut() {
            Some(last) if last.chars().all(|c| "<>=!~^".contains(c)) => last.push_str(token),
            _ => constraints.push(token.to_owned()),
        }
    }
    if constraints.is_empty() {
        return None;
    }
    let mut terms = Vec::new();
    for constraint in constraints {
        if constraint == "*" {
            continue;
        }
        let (op, version) = constraint.split_at(constraint.find(|c: char| c.is_ascii_digit())?);
        let numeric = version.strip_suffix(".*").unwrap_or(version);
        let components: Vec<u64> = numeric
            .split('.')
            .map(|part| part.parse().ok())
            .collect::<Option<_>>()?;
        if numeric != version && !matches!(op, "" | "==" | "!=") {
            return None;
        }
        let wildcard = if numeric == version { "" } else { ".*" };
        let term = |op: &str, components: &[u64]| {
            let version = components
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".");
            let variable = if components.len() >= 3 {
                "python_full_version"
            } else {
                "python_version"
            };
            format!("{variable} {op} \"{version}{wildcard}\"")
        };
        match op {
            "" => terms.push(term("==", &components)),
            "==" | "!=" | ">=" | "<=" | ">" | "<" | "~=" => terms.push(term(op, &components)),
            "^" | "~" => {
                // `^1.2.3` allows up to the next change of the first nonzero
                // component; `~1.2.3` allows patch changes, `~1` minor ones.
                let bump = if op == "~" {
                    usize::from(components.len() > 1)
                } else {
                    components
                        .iter()
                        .position(|&part| part != 0)
                        .unwrap_or(components.len() - 1)
                };
                let mut upper = components[..=bump].to_vec();
                upper[bump] = upper[bump].checked_add(1)?;
                terms.push(term(">=", &components));
                terms.push(term("<", &upper));
            },
            _ => return None,
        }
    }
    Some(terms)
}

/// The Poetry constraint (#682): PEP 440 ones in canonical form so CHK009
/// matches them against PEP 508 declarations, others (`^0.1.7`) as written.
/// `*` and path/git sources have none; a multiple-constraints array joins
/// its versions with `||`.
fn poetry_version_constraint(value: &Value) -> Option<String> {
    let single = |value: &Value| -> Option<String> {
        let version = match value {
            Value::String(version) => version.as_str(),
            Value::Table(table) => table.get("version")?.as_str()?,
            _ => return None,
        }
        .trim();
        if version.is_empty() || version == "*" {
            return None;
        }
        Some(canonical_version_specifiers(version).unwrap_or_else(|| version.to_owned()))
    };
    match value {
        Value::Array(constraints) => {
            let versions: Vec<String> = constraints.iter().filter_map(single).collect();
            (!versions.is_empty()).then(|| versions.join(" || "))
        },
        _ => single(value),
    }
}

fn poetry_requirement_name(name: &str, value: &Value) -> String {
    let extras = value
        .as_table()
        .and_then(|table| table.get("extras"))
        .and_then(Value::as_array)
        .map(|extras| extras.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();

    if extras.is_empty() {
        name.to_owned()
    } else {
        format!("{name}[{}]", extras.join(","))
    }
}

fn extract_pdm_dependencies(
    tool: &toml::Table,
    rel: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let Some(pdm) = tool.get("pdm").and_then(Value::as_table) else {
        return;
    };

    if let Some(dev_groups) = pdm.get("dev-dependencies").and_then(Value::as_table) {
        push_dependency_table(
            dev_groups,
            rel,
            DependencyContext::Group,
            "tool.pdm.dev-dependencies",
            dependencies,
            warnings,
        );
    }

    if let Some(optional) = pdm.get("optional-dependencies").and_then(Value::as_table) {
        push_dependency_table(
            optional,
            rel,
            DependencyContext::OptionalExtra,
            "tool.pdm.optional-dependencies",
            dependencies,
            warnings,
        );
    }
}

fn extract_hatch_dependencies(
    tool: &toml::Table,
    rel: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let Some(hatch) = tool.get("hatch").and_then(Value::as_table) else {
        return;
    };
    let Some(envs) = hatch.get("envs").and_then(Value::as_table) else {
        return;
    };

    for (env_name, env_value) in envs {
        let Some(env_table) = env_value.as_table() else {
            continue;
        };
        let Some(deps) = env_table.get("dependencies").and_then(Value::as_array) else {
            continue;
        };
        let group = if env_name == "default" {
            "dev".to_owned()
        } else {
            env_name.clone()
        };
        push_dependency_array(
            deps,
            rel,
            &DependencyContext::Group(group),
            &format!("tool.hatch.envs.{env_name}.dependencies"),
            dependencies,
            warnings,
        );
    }
}

/// The uv-dynamic-versioning metadata hook fills the `dynamic` fields from
/// static arrays, only rendering templates such as `{{ version }}` in them.
fn extract_uv_dynamic_versioning_hook(
    table: &toml::Table,
    rel: &str,
    result: &mut PyprojectExtraction,
) {
    const HOOK: &str = "tool.hatch.metadata.hooks.uv-dynamic-versioning";
    let Some(hook) = table
        .get("tool")
        .and_then(|tool| tool.get("hatch"))
        .and_then(|hatch| hatch.get("metadata"))
        .and_then(|metadata| metadata.get("hooks"))
        .and_then(|hooks| hooks.get("uv-dynamic-versioning"))
        .and_then(Value::as_table)
    else {
        return;
    };
    let dynamic = &result.metadata.dynamic;
    let dynamic_deps = dynamic.iter().any(|field| field == "dependencies");
    let dynamic_optional = dynamic.iter().any(|field| field == "optional-dependencies");

    if dynamic_deps && let Some(deps) = hook.get("dependencies").and_then(Value::as_array) {
        push_templated_array(
            deps,
            rel,
            &DependencyContext::Runtime,
            &format!("{HOOK}.dependencies"),
            result,
        );
    }
    if dynamic_optional
        && let Some(optional) = hook.get("optional-dependencies").and_then(Value::as_table)
    {
        for (extra, deps) in optional {
            if let Some(deps) = deps.as_array() {
                push_templated_array(
                    deps,
                    rel,
                    &DependencyContext::OptionalExtra(extra.clone()),
                    &format!("{HOOK}.optional-dependencies.{extra}"),
                    result,
                );
            }
        }
    }
}

/// What fills `dynamic = ["optional-dependencies"]` in this file. The
/// uv-dynamic-versioning hook is read, except an extra whose value is not an
/// array of strings; `[tool.setuptools.dynamic]` names its extras but keeps
/// them in `file =` lists chokkin does not read. A table of another shape, or
/// any other backend, leaves them unknown (#905).
fn dynamic_extras_state(table: &toml::Table) -> ExtrasState {
    let tool = table.get("tool");
    let hook = tool
        .and_then(|tool| tool.get("hatch"))
        .and_then(|hatch| hatch.get("metadata"))
        .and_then(|metadata| metadata.get("hooks"))
        .and_then(|hooks| hooks.get("uv-dynamic-versioning"))
        .and_then(|hook| hook.get("optional-dependencies"));
    let setuptools = tool
        .and_then(|tool| tool.get("setuptools"))
        .and_then(|setuptools| setuptools.get("dynamic"))
        .and_then(|dynamic| dynamic.get("optional-dependencies"));
    let (extras, unread): (_, fn(&Value) -> bool) = match (hook, setuptools) {
        (Some(hook), _) => (hook, |deps| {
            deps.as_array()
                .is_none_or(|deps| !deps.iter().all(Value::is_str))
        }),
        (None, Some(setuptools)) => (setuptools, |_| true),
        (None, None) => return ExtrasState::Unknown,
    };
    let Some(extras) = extras.as_table() else {
        return ExtrasState::Unknown;
    };
    let unread: std::collections::BTreeSet<String> = extras
        .iter()
        .filter(|(_, deps)| unread(deps))
        .map(|(extra, _)| extra.clone())
        .collect();
    if unread.is_empty() {
        ExtrasState::Known
    } else {
        ExtrasState::Partial(unread)
    }
}

fn push_templated_array(
    deps: &[Value],
    rel: &str,
    context: &DependencyContext,
    label_prefix: &str,
    result: &mut PyprojectExtraction,
) {
    for (index, dep) in deps.iter().enumerate() {
        let Some(raw) = dep.as_str() else {
            continue;
        };
        let label = format!("{label_prefix}[{index}]");
        let origin = DependencyOrigin {
            file: rel.to_owned(),
            line: None,
            label: label.clone(),
        };
        let parsed = render_templates(raw)
            .and_then(|rendered| parse_pep508_requirement(&rendered, context.clone(), origin).ok());
        match parsed {
            Some(mut dep) => {
                // The rendered version is a placeholder, not the real one.
                if raw.contains("{{") {
                    dep.specifier = None;
                }
                result.dependencies.push(dep);
            },
            None => result
                .warnings
                .push(ManifestWarning::InvalidRequirementLine {
                    file: rel.to_owned(),
                    line: None,
                    label,
                    raw: raw.to_owned(),
                }),
        }
    }
}

/// Replace every `{{ ... }}` with a version that parses as PEP 440. `None`
/// when a template sits outside the version (name, extras, URL or marker),
/// where no placeholder can stand in for it.
fn render_templates(raw: &str) -> Option<String> {
    let Some(first) = raw.find("{{") else {
        return Some(raw.to_owned());
    };
    let head = raw[..first].trim_start();
    let name_len = head
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(head.len());
    let in_version = name_len < head.len()
        && !head.contains(['@', ':', '/'])
        && head.matches('[').count() == head.matches(']').count()
        && !raw
            .split_once(';')
            .is_some_and(|(_, marker)| marker.contains("{{"));
    if !in_version {
        return None;
    }
    let mut rendered = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        rendered.push_str(&rest[..start]);
        rendered.push('0');
        rest = &rest[start + len + 2..];
    }
    rendered.push_str(rest);
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(contents: &str) -> Result<PyprojectExtraction, ManifestError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pyproject.toml");
        std::fs::write(&path, contents).expect("write pyproject.toml");
        extract_pyproject(temp.path(), &path)
    }

    #[test]
    fn dynamic_dependencies_skip_project_list() {
        let result = extract(
            "[project]\nname = \"x\"\ndynamic = [\"dependencies\"]\ndependencies = [\"requests\"]\n",
        )
        .expect("valid pyproject");

        assert_eq!(result.dependencies, []);
    }

    #[test]
    fn records_whether_project_table_exists() {
        let tool_only = extract("[tool.isort]\nprofile = \"black\"\n").expect("valid pyproject");
        assert!(!tool_only.has_project_table);
        let project = extract("[project]\nname = \"x\"\n").expect("valid pyproject");
        assert!(project.has_project_table);
    }

    #[test]
    fn build_system_requires_stay_out_of_dependencies() {
        let result = extract(
            "[build-system]\nrequires = [\"hatchling\", \"hatch-vcs>=0.4\"]\nbuild-backend = \"hatchling.build\"\n[project]\nname = \"x\"\n[tool.hatch.build.targets.wheel]\npackages = [\"src/x\"]\n",
        )
        .expect("valid pyproject");

        assert_eq!(result.dependencies, []);
        assert_eq!(
            result.metadata.build_backend.as_deref(),
            Some("hatchling.build")
        );
        let names: Vec<_> = result
            .metadata
            .build_requires
            .iter()
            .map(|dep| (dep.name.as_str(), dep.origin.label.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("hatchling", "build-system.requires[0]"),
                ("hatch-vcs", "build-system.requires[1]"),
            ]
        );
        assert!(
            result
                .metadata
                .build_requires
                .iter()
                .all(|dep| dep.context == DependencyContext::Build)
        );
        let targets = result.metadata.wheel_targets.expect("wheel targets");
        assert_eq!(targets.paths, vec!["src/x"]);
    }

    #[test]
    fn extracts_pdm_dev_dependencies() {
        let result = extract(
            "[project]\nname = \"x\"\n[tool.pdm.dev-dependencies]\ndev = [\"pytest>=8\"]\n",
        )
        .expect("valid pyproject");
        assert!(result.warnings.contains(&ManifestWarning::PdmDetected));
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].name, "pytest");
        assert!(matches!(
            &result.dependencies[0].context,
            DependencyContext::Group(group) if group == "dev"
        ));
    }

    #[test]
    fn extracts_pdm_optional_dependencies() {
        let result = extract(
            "[project]\nname = \"x\"\n[tool.pdm.optional-dependencies]\nhttp = [\"httpx\"]\n",
        )
        .expect("valid pyproject");
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].name, "httpx");
        assert_eq!(
            result.dependencies[0].origin.label,
            "tool.pdm.optional-dependencies.http[0]"
        );
        assert!(matches!(
            &result.dependencies[0].context,
            DependencyContext::OptionalExtra(extra) if extra == "http"
        ));
    }

    #[test]
    fn extracts_hatch_env_dependencies_as_groups() {
        let result = extract(
            "[project]\nname = \"x\"\n[tool.hatch.envs.default]\ndependencies = [\"pytest\"]\n[tool.hatch.envs.test]\ndependencies = [\"coverage\"]\n",
        )
        .expect("valid pyproject");
        assert!(result.warnings.contains(&ManifestWarning::HatchDetected));
        assert_eq!(result.dependencies.len(), 2);
        assert!(result.dependencies.iter().any(|dep| {
            dep.name == "pytest"
                && matches!(&dep.context, DependencyContext::Group(group) if group == "dev")
        }));
        assert!(result.dependencies.iter().any(|dep| {
            dep.name == "coverage"
                && matches!(&dep.context, DependencyContext::Group(group) if group == "test")
        }));
    }

    #[test]
    fn poetry_name_fills_metadata_without_a_project_table() {
        let result = extract("[tool.poetry]\nname = \"textual\"\nversion = \"8.2.8\"\n")
            .expect("valid pyproject");
        assert_eq!(result.metadata.name.as_deref(), Some("textual"));
        assert_eq!(result.metadata.version.as_deref(), Some("8.2.8"));
        assert!(!result.has_project_table);
    }

    #[test]
    fn project_name_wins_over_poetry_name() {
        let result = extract(
            "[project]\nname = \"real\"\n[tool.poetry]\nname = \"legacy\"\nversion = \"1\"\n",
        )
        .expect("valid pyproject");
        assert_eq!(result.metadata.name.as_deref(), Some("real"));
        // Poetry 2 reads `[tool.poetry].version` when `[project]` has none.
        assert_eq!(result.metadata.version.as_deref(), Some("1"));
    }

    #[test]
    fn extracts_poetry_dependencies() {
        let result = extract(
            "[tool.poetry]\nname = \"x\"\n[tool.poetry.dependencies]\npython = \"^3.11\"\nrequests = \"^2.32\"\nhttpx = { version = \"^0.27\", extras = [\"http2\"] }\n[tool.poetry.group.dev.dependencies]\npytest = \"^8\"\n[tool.poetry.dev-dependencies]\nruff = \"^0.6\"\n",
        )
        .expect("valid pyproject");
        assert!(result.warnings.contains(&ManifestWarning::PoetryDetected));
        assert!(result.poetry_detected);
        assert_eq!(result.dependencies.len(), 4);
        assert!(
            result
                .dependencies
                .iter()
                .any(|dep| { dep.name == "requests" && dep.context == DependencyContext::Runtime })
        );
        assert!(result.dependencies.iter().any(|dep| {
            dep.name == "httpx"
                && dep.extras == vec!["http2".to_owned()]
                && dep.context == DependencyContext::Runtime
        }));
        assert!(result.dependencies.iter().any(|dep| {
            dep.name == "pytest"
                && matches!(&dep.context, DependencyContext::Group(group) if group == "dev")
        }));
        assert!(result.dependencies.iter().any(|dep| {
            dep.name == "ruff"
                && matches!(&dep.context, DependencyContext::Group(group) if group == "dev")
        }));
    }

    #[test]
    fn keeps_poetry_version_constraints_as_written() {
        let result = extract(
            "[tool.poetry]\nname = \"x\"\n[tool.poetry.dependencies]\npython = \"^3.11\"\nrequests = \"^2.32\"\nhttpx = { version = \">=0.27\", extras = [\"http2\"] }\nany = \"*\"\nlocal = { path = \"../local\" }\nspaced = \">= 1.2,<2\"\nsplit = [{ version = \"^1\", python = \"<3.9\" }, { version = \"^2\", python = \">=3.9\" }]\n[tool.poetry.group.test.dependencies]\ndulwich = \">=1.2.1\"\nrich = \"~13.0\"\n",
        )
        .expect("valid pyproject");
        let mut specifiers: Vec<_> = result
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), dep.specifier.as_deref()))
            .collect();
        specifiers.sort_unstable();
        assert_eq!(
            specifiers,
            vec![
                ("any", None),
                ("dulwich", Some(">=1.2.1")),
                ("httpx", Some(">=0.27")),
                ("local", None),
                ("requests", Some("^2.32")),
                ("rich", Some("~13.0")),
                ("spaced", Some(">=1.2, <2")),
                ("split", Some("^1 || ^2")),
            ]
        );
    }

    /// A multiple-constraint dependency is optional only when every form is.
    #[test]
    fn poetry_multiple_constraints_are_optional_when_every_form_is() {
        let result = extract(
            r#"[tool.poetry]
name = "x"
[tool.poetry.dependencies]
both = [{ version = "^1", python = "<3.9", optional = true }, { version = "^2", python = ">=3.9", optional = true }]
half = [{ version = "^1", python = "<3.9", optional = true }, { version = "^2", python = ">=3.9" }]
"#,
        )
        .expect("valid pyproject");
        let mut contexts: Vec<_> = result
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), &dep.context))
            .collect();
        contexts.sort_unstable_by_key(|dep| dep.0);
        assert_eq!(
            contexts,
            vec![
                ("both", &DependencyContext::OptionalExtra("both".to_owned())),
                ("half", &DependencyContext::Runtime),
            ]
        );
    }

    #[test]
    fn reads_when_poetry_installs_a_dependency() {
        let result = extract(
            r#"[tool.poetry]
name = "x"
[tool.poetry.dependencies]
python = "^3.9"
uvloop = { version = "*", optional = true }
orjson = { version = "*", optional = true }
tomli = { version = "*", python = "<3.11" }
colorama = { version = "*", markers = "sys_platform == 'win32'" }
pywin32 = { version = "*", platform = "win32", python = "^3.8" }
legacy = { version = "*", python = ">=2.7,<2.8 || ~3.4.1" }
odd = { version = "*", python = "3.9-dev" }
split = [{ version = "^1", python = "<3.9" }, { version = "^2", python = ">=3.9" }]
mixed = [{ version = "^1", python = "<3.9" }, { version = "^2" }]
plain = { version = "*", python = "*", source = "internal", develop = true }
[tool.poetry.extras]
fast = ["UVLoop"]
[tool.poetry.group.dev.dependencies]
tomli-w = { version = "*", optional = true }
"#,
        )
        .expect("valid pyproject");
        let mut deps: Vec<_> = result
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), &dep.context, dep.marker.as_deref()))
            .collect();
        deps.sort_unstable_by_key(|dep| dep.0);
        let runtime = DependencyContext::Runtime;
        let dev = DependencyContext::Group("dev".to_owned());
        assert_eq!(
            deps,
            vec![
                ("colorama", &runtime, Some("sys_platform == 'win32'")),
                (
                    "legacy",
                    &runtime,
                    Some(
                        "(python_version >= \"2.7\" and python_version < \"2.8\") or (python_full_version >= \"3.4.1\" and python_version < \"3.5\")"
                    )
                ),
                ("mixed", &runtime, None),
                ("odd", &runtime, Some("python 3.9-dev")),
                (
                    "orjson",
                    &DependencyContext::OptionalExtra("orjson".to_owned()),
                    None
                ),
                ("plain", &runtime, None),
                (
                    "pywin32",
                    &runtime,
                    Some(
                        "(sys_platform == \"win32\") and (python_version >= \"3.8\" and python_version < \"4\")"
                    )
                ),
                (
                    "split",
                    &runtime,
                    Some("(python_version < \"3.9\") or (python_version >= \"3.9\")")
                ),
                ("tomli", &runtime, Some("python_version < \"3.11\"")),
                ("tomli-w", &dev, None),
                (
                    "uvloop",
                    &DependencyContext::OptionalExtra("fast".to_owned()),
                    None
                ),
            ]
        );
    }

    /// A `platform` constraint stays conditional without becoming PEP 508,
    /// and an `or` in `markers` keeps its grouping next to `python`.
    #[test]
    fn poetry_platform_and_markers_keep_their_meaning() {
        let result = extract(
            r#"[tool.poetry.dependencies]
unix = { version = "*", platform = "linux || darwin" }
anywhere = { version = "*", platform = "*" }
wrapped = { version = "*", markers = "os_name == 'nt' or\nos_name == 'java'", python = "<3.9" }
"#,
        )
        .expect("valid pyproject");
        let markers: Vec<_> = result
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), dep.marker.as_deref()))
            .collect();
        assert_eq!(
            markers,
            vec![
                ("anywhere", None),
                ("unix", Some("platform linux || darwin")),
                (
                    "wrapped",
                    Some("(os_name == 'nt' or\nos_name == 'java') and (python_version < \"3.9\")")
                ),
            ]
        );
    }

    #[test]
    fn converts_poetry_python_constraints_to_markers() {
        let cases = [
            ("3.8", Some("python_version == \"3.8\"")),
            ("==3.8.*", Some("python_version == \"3.8.*\"")),
            (
                ">= 3.8, != 3.9.1",
                Some("python_version >= \"3.8\" and python_full_version != \"3.9.1\""),
            ),
            (
                ">=3.8 <4",
                Some("python_version >= \"3.8\" and python_version < \"4\""),
            ),
            (
                "^0.2.1",
                Some("python_full_version >= \"0.2.1\" and python_version < \"0.3\""),
            ),
            (
                "^0.0",
                Some("python_version >= \"0.0\" and python_version < \"0.1\""),
            ),
            (
                "~3",
                Some("python_version >= \"3\" and python_version < \"4\""),
            ),
            ("~=3.8", Some("python_version ~= \"3.8\"")),
            ("*", None),
            ("<3.9 || *", None),
            (
                "3.8 | 3.9",
                Some("(python_version == \"3.8\") or (python_version == \"3.9\")"),
            ),
            ("^3.*", Some("python ^3.*")),
            (">=3.8, >=", Some("python >=3.8, >=")),
            (
                "^18446744073709551615",
                Some("python ^18446744073709551615"),
            ),
            ("^3.x", Some("python ^3.x")),
            (" ", None),
        ];
        for (constraint, expected) in cases {
            assert_eq!(
                poetry_python_marker(constraint).as_deref(),
                expected,
                "{constraint}"
            );
        }
    }

    #[test]
    fn extracts_uv_dynamic_versioning_hook_fields_listed_in_dynamic() {
        let result = extract(
            "[project]\nname = \"x\"\ndynamic = [\"version\", \"dependencies\", \"optional-dependencies\"]\n[tool.hatch.metadata.hooks.uv-dynamic-versioning]\ndependencies = [\"x-core[cli]=={{ version }}\", \"rich>=13\"]\n[tool.hatch.metadata.hooks.uv-dynamic-versioning.optional-dependencies]\nclip = [\"pyperclip>=1.9\"]\n",
        )
        .expect("valid pyproject");
        let deps: Vec<_> = result
            .dependencies
            .iter()
            .map(|dep| {
                (
                    dep.name.as_str(),
                    dep.specifier.as_deref(),
                    &dep.context,
                    dep.origin.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            deps,
            vec![
                (
                    "x-core",
                    None,
                    &DependencyContext::Runtime,
                    "tool.hatch.metadata.hooks.uv-dynamic-versioning.dependencies[0]",
                ),
                (
                    "rich",
                    Some(">=13"),
                    &DependencyContext::Runtime,
                    "tool.hatch.metadata.hooks.uv-dynamic-versioning.dependencies[1]",
                ),
                (
                    "pyperclip",
                    Some(">=1.9"),
                    &DependencyContext::OptionalExtra("clip".to_owned()),
                    "tool.hatch.metadata.hooks.uv-dynamic-versioning.optional-dependencies.clip[0]",
                ),
            ]
        );
        assert_eq!(result.dependencies[0].extras, vec!["cli".to_owned()]);
    }

    #[test]
    fn uv_dynamic_versioning_hook_needs_the_field_in_dynamic() {
        let result = extract(
            "[project]\nname = \"x\"\ndynamic = [\"version\"]\n[tool.hatch.metadata.hooks.uv-dynamic-versioning]\ndependencies = [\"rich\"]\n[tool.hatch.metadata.hooks.uv-dynamic-versioning.optional-dependencies]\nclip = [\"pyperclip\"]\n",
        )
        .expect("valid pyproject");
        assert_eq!(result.dependencies, []);
    }

    #[test]
    fn templates_outside_the_version_warn_with_the_raw_text() {
        let entries = [
            "{{ name }}-core",
            ">={{ version }}",
            "core-{{ name }}",
            "core[{{ extra }}]",
            "core @ https://example.com/core-{{ version }}.whl",
            "https://example.com/core-{{ version }}.whl",
            "core>=1; python_version >= '{{ python }}'",
        ];
        let array = entries
            .iter()
            .map(|entry| format!("{entry:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let result = extract(&format!(
            "[project]\nname = \"x\"\ndynamic = [\"dependencies\"]\n[tool.hatch.metadata.hooks.uv-dynamic-versioning]\ndependencies = [{array}]\n"
        ))
        .expect("valid pyproject");
        assert_eq!(result.dependencies, []);
        for (index, raw) in entries.into_iter().enumerate() {
            assert!(
                result
                    .warnings
                    .contains(&ManifestWarning::InvalidRequirementLine {
                        file: "pyproject.toml".to_owned(),
                        line: None,
                        label: format!(
                            "tool.hatch.metadata.hooks.uv-dynamic-versioning.dependencies[{index}]"
                        ),
                        raw: raw.to_owned(),
                    })
            );
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn bare_key() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9-]{0,8}"
        }

        fn dep_name() -> impl Strategy<Value = String> {
            "[A-Za-z0-9]([A-Za-z0-9._-]{0,12}[A-Za-z0-9])?"
        }

        /// One Poetry python constraint term: an operator and 1..=3 version
        /// components. `>`, `<=`, `==` and bare versions are left out:
        /// `python_version` keeps only two components, so they differ from
        /// the full-version reading by convention, not by a bug.
        type PoetryTerm = (&'static str, Vec<u64>);

        fn poetry_term() -> impl Strategy<Value = PoetryTerm> {
            (
                prop::sample::select(vec!["^", "~", ">=", "<", "*"]),
                prop::collection::vec(0u64..3, 1..=3),
            )
        }

        /// Alternatives (`||`) of AND terms.
        fn poetry_constraint() -> impl Strategy<Value = Vec<Vec<PoetryTerm>>> {
            prop::collection::vec(prop::collection::vec(poetry_term(), 1..=2), 1..=3)
        }

        fn poetry_render(constraint: &[Vec<PoetryTerm>], spelling: u8) -> String {
            let or = ["||", " || ", "|"][usize::from(spelling % 3)];
            let and = [",", ", ", " "][usize::from(spelling / 3 % 3)];
            let gap = if spelling & 0x10 == 0 { "" } else { " " };
            constraint
                .iter()
                .map(|terms| {
                    terms
                        .iter()
                        .map(|(op, parts)| {
                            if *op == "*" {
                                return "*".to_owned();
                            }
                            let version = parts
                                .iter()
                                .map(u64::to_string)
                                .collect::<Vec<_>>()
                                .join(".");
                            format!("{op}{gap}{version}")
                        })
                        .collect::<Vec<_>>()
                        .join(and)
                })
                .collect::<Vec<_>>()
                .join(or)
        }

        fn padded(parts: &[u64]) -> [u64; 3] {
            let mut version = [0; 3];
            version[..parts.len()].copy_from_slice(parts);
            version
        }

        /// Poetry's documented meaning of a term on a full Python version.
        fn poetry_allows((op, parts): &PoetryTerm, python: [u64; 3]) -> bool {
            let lower = padded(parts);
            let upper = |index: usize| {
                let mut upper = [0; 3];
                upper[..index].copy_from_slice(&parts[..index]);
                upper[index] = parts[index] + 1;
                upper
            };
            match *op {
                "*" => true,
                ">=" => python >= lower,
                "<" => python < lower,
                // `^1.2.3` < 2.0.0, `^0.2.3` < 0.3.0, `^0.0.3` < 0.0.4,
                // `^0.0` < 0.1.0, `^0` < 1.0.0.
                "^" => {
                    let index = parts.iter().position(|&part| part != 0);
                    python >= lower && python < upper(index.unwrap_or(parts.len() - 1))
                },
                // `~1.2.3` < 1.3.0, `~1.2` < 1.3.0, `~1` < 2.0.0.
                "~" => python >= lower && python < upper(usize::from(parts.len() != 1)),
                op => panic!("unexpected operator {op}"),
            }
        }

        /// Evaluates the `python_version` / `python_full_version` markers
        /// `poetry_python_marker` writes; `python_version` is `X.Y`.
        fn marker_allows(marker: &str, python: [u64; 3]) -> bool {
            marker.split(" or ").any(|alternative| {
                let alternative = alternative.trim_start_matches('(').trim_end_matches(')');
                alternative.split(" and ").all(|term| {
                    let mut words = term.splitn(3, ' ');
                    let variable = words.next().expect("variable");
                    let op = words.next().expect("operator");
                    let version = words.next().expect("version").trim_matches('"');
                    let parts: Vec<u64> = version
                        .split('.')
                        .map(|part| part.parse().expect("number"))
                        .collect();
                    let value = match variable {
                        "python_version" => [python[0], python[1], 0],
                        "python_full_version" => python,
                        variable => panic!("unexpected variable {variable} in {marker}"),
                    };
                    let bound = padded(&parts);
                    match op {
                        ">=" => value >= bound,
                        "<" => value < bound,
                        op => panic!("unexpected operator {op} in {marker}"),
                    }
                })
            })
        }

        proptest! {
            #[test]
            fn poetry_python_marker_matches_poetry_semantics(
                constraint in poetry_constraint(),
                spelling in any::<u8>(),
            ) {
                let rendered = poetry_render(&constraint, spelling);
                let marker = poetry_python_marker(&rendered);
                if let Some(marker) = &marker {
                    prop_assert!(!marker.starts_with("python "), "{rendered} -> {marker}");
                }
                for python in (0..4).flat_map(|a| (0..4).flat_map(move |b| (0..4).map(move |c| [a, b, c]))) {
                    let expected = constraint
                        .iter()
                        .any(|terms| terms.iter().all(|term| poetry_allows(term, python)));
                    let actual = marker.as_deref().is_none_or(|marker| marker_allows(marker, python));
                    prop_assert_eq!(actual, expected, "{} -> {:?} at {:?}", rendered, marker, python);
                }
            }
        }

        proptest! {
            #[test]
            fn extract_pyproject_never_panics(contents in "\\PC{0,300}") {
                let _ = extract(&contents);
            }

            #[test]
            fn metadata_fields_roundtrip(
                name in proptest::option::of(dep_name()),
                version in proptest::option::of("[0-9]{1,3}\\.[0-9]{1,3}"),
                requires_python in proptest::option::of(">=3\\.[0-9]{1,2}"),
                dynamic in prop::collection::vec(bare_key(), 0..4),
            ) {
                let mut project = toml::Table::new();
                if let Some(name) = &name {
                    project.insert("name".into(), Value::String(name.clone()));
                }
                if let Some(version) = &version {
                    project.insert("version".into(), Value::String(version.clone()));
                }
                if let Some(requires) = &requires_python {
                    project.insert("requires-python".into(), Value::String(requires.clone()));
                }
                project.insert(
                    "dynamic".into(),
                    Value::Array(dynamic.iter().cloned().map(Value::String).collect()),
                );
                project.insert(
                    "dependencies".into(),
                    Value::Array(vec![Value::String("requests".into())]),
                );
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");
                prop_assert_eq!(result.metadata.name, name);
                prop_assert_eq!(result.metadata.version, version);
                prop_assert_eq!(result.metadata.requires_python, requires_python);
                prop_assert_eq!(&result.metadata.dynamic, &dynamic);

                let dynamic_deps = result.metadata.dynamic.iter().any(|d| d == "dependencies");
                prop_assert_eq!(result.dependencies.is_empty(), dynamic_deps);
            }

            #[test]
            fn optional_dependencies_carry_extra_context(
                extras in prop::collection::btree_map(
                    bare_key(),
                    prop::collection::vec(dep_name(), 0..4),
                    0..4,
                ),
            ) {
                let mut optional = toml::Table::new();
                for (extra, deps) in &extras {
                    optional.insert(
                        extra.clone(),
                        Value::Array(deps.iter().cloned().map(Value::String).collect()),
                    );
                }
                let mut project = toml::Table::new();
                project.insert("name".into(), Value::String("x".into()));
                project.insert("optional-dependencies".into(), Value::Table(optional));
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");

                let expected: usize = extras.values().map(Vec::len).sum();
                prop_assert_eq!(result.dependencies.len(), expected);
                for dep in &result.dependencies {
                    let DependencyContext::OptionalExtra(extra) = &dep.context else {
                        let context = format!("{:?}", dep.context);
                        return Err(TestCaseError::fail(format!(
                            "unexpected context: {context}"
                        )));
                    };
                    prop_assert!(extras.contains_key(extra));
                    let label_prefix = format!("project.optional-dependencies.{extra}[");
                    prop_assert!(dep.origin.label.starts_with(&label_prefix));
                }
            }

            #[test]
            fn entry_points_roundtrip_across_groups(
                scripts in prop::collection::btree_map(bare_key(), "[a-z_.:]{1,20}", 0..4),
                gui_scripts in prop::collection::btree_map(bare_key(), "[a-z_.:]{1,20}", 0..4),
            ) {
                let to_table = |map: &std::collections::BTreeMap<String, String>| {
                    let mut table = toml::Table::new();
                    for (key, value) in map {
                        table.insert(key.clone(), Value::String(value.clone()));
                    }
                    table
                };
                let mut project = toml::Table::new();
                project.insert("name".into(), Value::String("x".into()));
                project.insert("scripts".into(), Value::Table(to_table(&scripts)));
                project.insert("gui-scripts".into(), Value::Table(to_table(&gui_scripts)));
                let mut doc = toml::Table::new();
                doc.insert("project".into(), Value::Table(project));

                let result = extract(&toml::to_string(&doc).expect("serialize"))
                    .expect("valid pyproject");

                prop_assert_eq!(
                    result.entry_points.len(),
                    scripts.len() + gui_scripts.len()
                );
                for entry in &result.entry_points {
                    let expected = match entry.group.as_str() {
                        "console" => scripts.get(&entry.name),
                        "gui" => gui_scripts.get(&entry.name),
                        other => {
                            let group = other.to_owned();
                            return Err(TestCaseError::fail(format!(
                                "unexpected group: {group}"
                            )));
                        }
                    };
                    prop_assert_eq!(Some(&entry.target), expected);
                }
            }

            #[test]
            fn tool_section_warnings_match_subset(
                poetry in proptest::bool::ANY,
                pdm in proptest::bool::ANY,
                hatch in proptest::bool::ANY,
            ) {
                let mut contents = String::new();
                if poetry {
                    contents.push_str("[tool.poetry]\n");
                }
                if pdm {
                    contents.push_str("[tool.pdm]\n");
                }
                if hatch {
                    contents.push_str("[tool.hatch]\n");
                }

                let result = extract(&contents).expect("valid pyproject");
                let mut expected = Vec::new();
                if poetry {
                    expected.push(ManifestWarning::PoetryDetected);
                }
                if pdm {
                    expected.push(ManifestWarning::PdmDetected);
                }
                if hatch {
                    expected.push(ManifestWarning::HatchDetected);
                }
                prop_assert_eq!(result.warnings, expected);
            }
        }
    }
}
