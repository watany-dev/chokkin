//! dbt `profiles.yml`: the adapter each target's `type` names, which dbt
//! loads by name and the project never imports (#1256).
//!
//! A literal `type: snowflake` uses `dbt-snowflake`; adapters follow that
//! naming, so a type outside [`ADAPTERS`] still names `dbt-<type>`. A
//! computed value (Jinja such as `{{ env_var(...) }}`, a YAML alias) may
//! select any adapter, so it uses every one [`ADAPTERS`] lists; an adapter
//! outside the list stays reported then. A `type` key at the top level is
//! a profile name, not a target, and is skipped.

use super::config_text::{leading_spaces, yaml_scalar};

/// Adapter types and the distributions that provide them.
const ADAPTERS: &[(&str, &[&str])] = &[
    ("athena", &["dbt-athena", "dbt-athena-community"]),
    ("bigquery", &["dbt-bigquery"]),
    ("clickhouse", &["dbt-clickhouse"]),
    ("databricks", &["dbt-databricks"]),
    ("duckdb", &["dbt-duckdb"]),
    ("fabric", &["dbt-fabric"]),
    ("postgres", &["dbt-postgres"]),
    ("redshift", &["dbt-redshift"]),
    ("snowflake", &["dbt-snowflake"]),
    ("spark", &["dbt-spark"]),
    ("sqlserver", &["dbt-sqlserver"]),
    ("trino", &["dbt-trino"]),
];

/// The adapter distributions `contents` (a `profiles.yml`) uses.
pub(super) fn adapter_distributions(contents: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in contents.lines() {
        let Some(value) = line.trim_start().strip_prefix("type:") else {
            continue;
        };
        if leading_spaces(line) == 0 {
            continue;
        }
        let value = yaml_scalar(value).trim();
        if value.is_empty() {
            continue;
        }
        if value.contains("{{") || value.starts_with('*') {
            found.extend(
                ADAPTERS
                    .iter()
                    .flat_map(|(_, dists)| dists.iter().map(|dist| (*dist).to_owned())),
            );
            continue;
        }
        let adapter = value.to_ascii_lowercase();
        match ADAPTERS.iter().find(|(name, _)| *name == adapter) {
            Some((_, dists)) => found.extend(dists.iter().map(|dist| (*dist).to_owned())),
            None => found.push(format!("dbt-{adapter}")),
        }
    }
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_types_name_their_adapters() {
        let profiles = "dagster_open_platform:\n  target: prod\n  outputs:\n    prod:\n      type: snowflake  # warehouse\n      threads: 8\n    ci:\n      type: \"Postgres\"\n    local:\n      type: materialize\n    athena:\n      type: athena\n";
        assert_eq!(
            adapter_distributions(profiles),
            [
                "dbt-athena",
                "dbt-athena-community",
                "dbt-materialize",
                "dbt-postgres",
                "dbt-snowflake",
            ]
        );
    }

    #[test]
    fn a_computed_type_may_select_any_listed_adapter() {
        let profiles = "p:\n  outputs:\n    dev:\n      type: \"{{ env_var('DBT_ADAPTER') }}\"\n";
        let found = adapter_distributions(profiles);
        assert!(found.contains(&"dbt-snowflake".to_owned()));
        assert!(found.contains(&"dbt-trino".to_owned()));
        assert!(!found.contains(&"dbt-materialize".to_owned()));
    }

    #[test]
    fn top_level_and_empty_types_are_no_adapter() {
        assert!(
            adapter_distributions("type: snowflake\np:\n  outputs:\n    dev:\n      type:\n")
                .is_empty()
        );
        assert!(adapter_distributions("# type: snowflake\n").is_empty());
    }
}
