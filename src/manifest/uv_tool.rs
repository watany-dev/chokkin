//! `[tool.uv]` extraction beyond workspace members (R-04).

use toml::Value;

use super::pep508_util::normalize_distribution_name;
use super::types::{
    DeclaredDependency, DependencyContext, DependencyOrigin, UvDefaultGroups, UvSource,
    UvSourceKind, UvToolSettings,
};
use super::util::{DependencyPush, push_dependency};
use super::warnings::ManifestWarning;

/// What `[tool.uv]` contributes to a manifest.
#[derive(Debug, Default)]
pub struct UvToolExtraction {
    /// Legacy `dev-dependencies`, declared under the `dev` group like uv does.
    pub dependencies: Vec<DeclaredDependency>,
    /// `constraint-dependencies` and `override-dependencies`.
    pub constraints: Vec<DeclaredDependency>,
    /// Sources and default groups.
    pub settings: UvToolSettings,
}

/// Read `[tool.uv]` from a parsed `pyproject.toml`.
#[must_use]
pub fn extract_uv_tool(
    table: &toml::Table,
    rel: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> UvToolExtraction {
    let mut result = UvToolExtraction::default();
    let Some(uv) = table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("uv"))
        .and_then(Value::as_table)
    else {
        return result;
    };

    for key in [
        "dev-dependencies",
        "constraint-dependencies",
        "override-dependencies",
    ] {
        let Some(items) = uv.get(key).and_then(Value::as_array) else {
            continue;
        };
        let (sink, context) = if key == "dev-dependencies" {
            (
                &mut result.dependencies,
                DependencyContext::Group("dev".to_owned()),
            )
        } else {
            (&mut result.constraints, DependencyContext::Runtime)
        };
        for (index, raw) in items.iter().enumerate() {
            if let Some(raw) = raw.as_str() {
                push_dependency(DependencyPush {
                    dependencies: sink,
                    warnings,
                    raw,
                    context: context.clone(),
                    file: rel,
                    label: format!("tool.uv.{key}[{index}]"),
                    line: None,
                });
            }
        }
    }

    result.settings.default_groups = uv.get("default-groups").and_then(parse_default_groups);
    if let Some(sources) = uv.get("sources").and_then(Value::as_table) {
        result.settings.sources = parse_sources(sources, rel);
    }
    result
}

fn parse_default_groups(value: &Value) -> Option<UvDefaultGroups> {
    match value {
        Value::String(all) if all == "all" => Some(UvDefaultGroups::All),
        Value::Array(items) => Some(UvDefaultGroups::Groups(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )),
        _ => None,
    }
}

/// A source may be one table or an array of marker-scoped tables.
fn parse_sources(sources: &toml::Table, rel: &str) -> Vec<UvSource> {
    let mut parsed = Vec::new();
    for (name, value) in sources {
        let name_norm = normalize_distribution_name(name);
        let entries: Vec<(&toml::Table, String)> = match value {
            Value::Table(entry) => vec![(entry, format!("tool.uv.sources.{name}"))],
            Value::Array(items) => items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    item.as_table()
                        .map(|entry| (entry, format!("tool.uv.sources.{name}[{index}]")))
                })
                .collect(),
            _ => Vec::new(),
        };
        for (entry, label) in entries {
            parsed.push(UvSource {
                name: name_norm.clone(),
                kind: source_kind(entry),
                origin: DependencyOrigin {
                    file: rel.to_owned(),
                    line: None,
                    label,
                },
            });
        }
    }
    parsed
}

fn source_kind(entry: &toml::Table) -> UvSourceKind {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    if let Some(path) = text("path") {
        let editable = entry
            .get("editable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return UvSourceKind::Path { path, editable };
    }
    if entry.get("workspace").and_then(Value::as_bool) == Some(true) {
        return UvSourceKind::Workspace;
    }
    if let Some(url) = text("git") {
        return UvSourceKind::Git(url);
    }
    if let Some(url) = text("url") {
        return UvSourceKind::Url(url);
    }
    if let Some(index) = text("index") {
        return UvSourceKind::Index(index);
    }
    UvSourceKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(contents: &str) -> UvToolExtraction {
        let table: toml::Table = toml::from_str(contents).expect("valid toml");
        let mut warnings = Vec::new();
        extract_uv_tool(&table, "pyproject.toml", &mut warnings)
    }

    #[test]
    fn legacy_dev_dependencies_join_dev_group() {
        let result = extract("[tool.uv]\ndev-dependencies = [\"pytest>=8\"]\n");
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].name, "pytest");
        assert_eq!(
            result.dependencies[0].context,
            DependencyContext::Group("dev".to_owned())
        );
        assert_eq!(
            result.dependencies[0].origin.label,
            "tool.uv.dev-dependencies[0]"
        );
    }

    #[test]
    fn constraints_and_overrides_stay_out_of_declarations() {
        let result = extract(
            "[tool.uv]\nconstraint-dependencies = [\"urllib3<2\"]\noverride-dependencies = [\"idna==3.7\"]\n",
        );
        assert!(result.dependencies.is_empty());
        let labels: Vec<_> = result
            .constraints
            .iter()
            .map(|dep| (dep.name.as_str(), dep.origin.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("urllib3", "tool.uv.constraint-dependencies[0]"),
                ("idna", "tool.uv.override-dependencies[0]"),
            ]
        );
    }

    #[test]
    fn default_groups_accepts_list_and_all() {
        let list = extract("[tool.uv]\ndefault-groups = [\"dev\", \"lint\"]\n");
        assert_eq!(
            list.settings.default_groups,
            Some(UvDefaultGroups::Groups(vec![
                "dev".to_owned(),
                "lint".to_owned()
            ]))
        );
        let all = extract("[tool.uv]\ndefault-groups = \"all\"\n");
        assert_eq!(all.settings.default_groups, Some(UvDefaultGroups::All));
        let invalid = extract("[tool.uv]\ndefault-groups = \"dev\"\n");
        assert_eq!(invalid.settings.default_groups, None);
    }

    #[test]
    fn unknown_source_shape_is_other() {
        let result = extract("[tool.uv.sources]\nfoo = { editable = true }\nbar = \"x\"\n");
        assert_eq!(result.settings.sources.len(), 1);
        assert_eq!(result.settings.sources[0].kind, UvSourceKind::Other);
    }

    #[test]
    fn sources_keep_kind_and_label() {
        let result = extract(concat!(
            "[tool.uv.sources]\n",
            "My_Lib = { path = \"libs/my-lib\", editable = true }\n",
            "billing = { workspace = true }\n",
            "httpx = { git = \"https://github.com/encode/httpx\" }\n",
            "torch = [\n",
            "  { index = \"cpu\", marker = \"sys_platform != 'darwin'\" },\n",
            "  { url = \"https://example.com/torch.whl\" },\n",
            "]\n",
        ));
        let kinds: Vec<_> = result
            .settings
            .sources
            .iter()
            .map(|source| {
                (
                    source.name.as_str(),
                    source.kind.clone(),
                    source.origin.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    "my-lib",
                    UvSourceKind::Path {
                        path: "libs/my-lib".to_owned(),
                        editable: true
                    },
                    "tool.uv.sources.My_Lib"
                ),
                (
                    "billing",
                    UvSourceKind::Workspace,
                    "tool.uv.sources.billing"
                ),
                (
                    "httpx",
                    UvSourceKind::Git("https://github.com/encode/httpx".to_owned()),
                    "tool.uv.sources.httpx"
                ),
                (
                    "torch",
                    UvSourceKind::Index("cpu".to_owned()),
                    "tool.uv.sources.torch[0]"
                ),
                (
                    "torch",
                    UvSourceKind::Url("https://example.com/torch.whl".to_owned()),
                    "tool.uv.sources.torch[1]"
                ),
            ]
        );
        assert!(result.settings.is_workspace_source("billing"));
        assert!(!result.settings.is_workspace_source("my-lib"));
    }
}
