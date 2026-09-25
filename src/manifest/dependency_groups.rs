//! PEP 735 `include-group` expansion for `[dependency-groups]`.

use std::collections::{BTreeMap, VecDeque};

use toml::Value;

use super::pep508_util::normalize_distribution_name;
use super::pyproject::push_dependency_table;
use super::types::{DeclaredDependency, DependencyContext};
use super::warnings::ManifestWarning;

/// One `[dependency-groups]` array as written.
struct GroupEntry<'a> {
    key: &'a str,
    items: &'a [Value],
    /// Indices of directly included groups that exist.
    includes: Vec<usize>,
}

/// Push every group requirement once, under the group that declares it, and
/// record the include chains that pull it into other groups.
///
/// Included requirements are not copied into the including group, so CHK002 /
/// CHK009 and `--fix` still see exactly one declaration per written requirement.
pub(super) fn extract_dependency_groups(
    groups: &toml::Table,
    rel: &str,
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
) {
    let start = dependencies.len();
    push_dependency_table(
        groups,
        rel,
        DependencyContext::Group,
        "dependency-groups",
        dependencies,
        warnings,
    );
    let entries = collect_entries(groups, rel, warnings);
    let paths = include_paths(&entries, rel, warnings);
    let by_group = entries
        .iter()
        .map(|entry| entry.key)
        .zip(paths)
        .collect::<BTreeMap<_, _>>();
    for dep in dependencies.iter_mut().skip(start) {
        if let DependencyContext::Group(group) = &dep.context
            && let Some(chains) = by_group.get(group.as_str())
        {
            dep.included_via.clone_from(chains);
        }
    }
}

fn collect_entries<'a>(
    groups: &'a toml::Table,
    rel: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> Vec<GroupEntry<'a>> {
    let mut entries = groups
        .iter()
        .filter_map(|(key, value)| {
            value.as_array().map(|items| GroupEntry {
                key: key.as_str(),
                items: items.as_slice(),
                includes: Vec::new(),
            })
        })
        .collect::<Vec<_>>();
    let mut by_name = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        by_name
            .entry(normalize_distribution_name(entry.key))
            .or_insert(index);
    }
    for entry in &mut entries {
        for target in entry.items.iter().filter_map(include_target) {
            if let Some(&index) = by_name.get(&normalize_distribution_name(target)) {
                entry.includes.push(index);
            } else {
                warnings.push(ManifestWarning::DependencyGroupIncludeUndefined {
                    file: rel.to_owned(),
                    group: entry.key.to_owned(),
                    include: target.to_owned(),
                });
            }
        }
    }
    entries
}

fn include_target(item: &Value) -> Option<&str> {
    item.as_table()?.get("include-group")?.as_str()
}

/// For each group, the shortest include chain from every group that pulls it
/// in (directly or transitively), ordered from the including group down to
/// the group itself. Cycles are reported once and otherwise cut.
fn include_paths(
    entries: &[GroupEntry<'_>],
    rel: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> Vec<Vec<Vec<String>>> {
    let names = |chain: Vec<usize>| -> Vec<String> {
        chain
            .into_iter()
            .filter_map(|index| entries.get(index))
            .map(|entry| entry.key.to_owned())
            .collect()
    };
    let mut paths = vec![Vec::new(); entries.len()];
    for start in 0..entries.len() {
        let parents = bfs_parents(entries, start);
        for (target, slot) in paths.iter_mut().enumerate() {
            if target != start && parents.get(target).copied().flatten().is_some() {
                slot.push(names(include_chain(&parents, start, target)));
            }
        }
        let on_cycle = parents.get(start).copied().flatten().is_some();
        let cycle = include_chain(&parents, start, start);
        // Only the lowest-index member reports, so one cycle is one warning.
        if on_cycle && cycle.iter().all(|&node| node >= start) {
            warnings.push(ManifestWarning::DependencyGroupIncludeCycle {
                file: rel.to_owned(),
                groups: names(cycle),
            });
        }
    }
    paths
}

/// BFS predecessor of each group reachable from `start` via includes.
/// `start` itself only gets a predecessor when it sits on a cycle.
fn bfs_parents(entries: &[GroupEntry<'_>], start: usize) -> Vec<Option<usize>> {
    let mut parents = vec![None; entries.len()];
    let mut queue = VecDeque::from([start]);
    while let Some(current) = queue.pop_front() {
        let Some(entry) = entries.get(current) else {
            continue;
        };
        for &next in &entry.includes {
            if let Some(slot) = parents.get_mut(next)
                && slot.is_none()
            {
                *slot = Some(current);
                if next != start {
                    queue.push_back(next);
                }
            }
        }
    }
    parents
}

fn include_chain(parents: &[Option<usize>], start: usize, target: usize) -> Vec<usize> {
    let mut chain = vec![target];
    let mut node = target;
    for _ in 0..parents.len() {
        let Some(parent) = parents.get(node).copied().flatten() else {
            break;
        };
        chain.push(parent);
        if parent == start {
            break;
        }
        node = parent;
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(groups_toml: &str) -> (Vec<DeclaredDependency>, Vec<ManifestWarning>) {
        let doc: toml::Table = toml::from_str(groups_toml).expect("valid toml");
        let groups = doc
            .get("dependency-groups")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        let mut dependencies = Vec::new();
        let mut warnings = Vec::new();
        extract_dependency_groups(&groups, "pyproject.toml", &mut dependencies, &mut warnings);
        (dependencies, warnings)
    }

    fn via<'a>(deps: &'a [DeclaredDependency], name: &str) -> &'a [Vec<String>] {
        &deps
            .iter()
            .find(|dep| dep.name == name)
            .expect("dependency present")
            .included_via
    }

    fn chain(groups: &[&str]) -> Vec<String> {
        groups.iter().map(|group| (*group).to_owned()).collect()
    }

    #[test]
    fn requirements_stay_in_declaring_group_with_label_index() {
        let (deps, warnings) = extract(
            "[dependency-groups]\ntest = [\"pytest\"]\ndev = [{include-group = \"test\"}, \"ruff\"]\n",
        );
        assert!(warnings.is_empty());
        assert_eq!(deps.len(), 2);
        let pytest = deps
            .iter()
            .find(|dep| dep.name == "pytest")
            .expect("pytest");
        assert_eq!(pytest.context, DependencyContext::Group("test".to_owned()));
        assert_eq!(pytest.origin.label, "dependency-groups.test[0]");
        assert_eq!(pytest.included_via, vec![chain(&["dev", "test"])]);
        let ruff = deps.iter().find(|dep| dep.name == "ruff").expect("ruff");
        assert_eq!(ruff.origin.label, "dependency-groups.dev[1]");
        assert!(ruff.included_via.is_empty());
    }

    #[test]
    fn multi_level_includes_record_every_including_group() {
        let (deps, warnings) = extract(
            "[dependency-groups]\nall = [{include-group = \"dev\"}]\ndev = [{include-group = \"test\"}]\ntest = [\"pytest\"]\n",
        );
        assert!(warnings.is_empty());
        assert_eq!(
            via(&deps, "pytest"),
            [chain(&["all", "dev", "test"]), chain(&["dev", "test"])]
        );
    }

    #[test]
    fn include_names_are_normalized() {
        let (deps, warnings) = extract(
            "[dependency-groups]\nDev_Tools = [\"ruff\"]\ndev = [{include-group = \"dev-tools\"}]\n",
        );
        assert!(warnings.is_empty());
        assert_eq!(via(&deps, "ruff"), [chain(&["dev", "Dev_Tools"])]);
    }

    #[test]
    fn undefined_include_warns_and_keeps_other_requirements() {
        let (deps, warnings) =
            extract("[dependency-groups]\ndev = [{include-group = \"missing\"}, \"pytest\"]\n");
        assert_eq!(deps.len(), 1);
        assert_eq!(
            warnings,
            vec![ManifestWarning::DependencyGroupIncludeUndefined {
                file: "pyproject.toml".to_owned(),
                group: "dev".to_owned(),
                include: "missing".to_owned(),
            }]
        );
    }

    #[test]
    fn cycle_warns_once_and_still_expands() {
        let (deps, warnings) = extract(
            "[dependency-groups]\na = [{include-group = \"b\"}, \"alpha\"]\nb = [{include-group = \"a\"}, \"beta\"]\n",
        );
        assert_eq!(
            warnings,
            vec![ManifestWarning::DependencyGroupIncludeCycle {
                file: "pyproject.toml".to_owned(),
                groups: chain(&["a", "b", "a"]),
            }]
        );
        assert_eq!(via(&deps, "alpha"), [chain(&["b", "a"])]);
        assert_eq!(via(&deps, "beta"), [chain(&["a", "b"])]);
    }

    #[test]
    fn self_include_is_a_cycle() {
        let (deps, warnings) =
            extract("[dependency-groups]\ndev = [{include-group = \"dev\"}, \"pytest\"]\n");
        assert_eq!(
            warnings,
            vec![ManifestWarning::DependencyGroupIncludeCycle {
                file: "pyproject.toml".to_owned(),
                groups: chain(&["dev", "dev"]),
            }]
        );
        assert!(via(&deps, "pytest").is_empty());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn arbitrary_group_graphs_never_panic(
                edges in prop::collection::vec(
                    prop::collection::vec(0usize..8, 0..5),
                    0..8,
                ),
            ) {
                let mut groups = toml::Table::new();
                for (index, targets) in edges.iter().enumerate() {
                    let mut items: Vec<Value> = targets
                        .iter()
                        .map(|target| {
                            let mut include = toml::Table::new();
                            include.insert(
                                "include-group".into(),
                                Value::String(format!("G_{target}")),
                            );
                            Value::Table(include)
                        })
                        .collect();
                    items.push(Value::String(format!("dep{index}")));
                    groups.insert(format!("g-{index}"), Value::Array(items));
                }
                let mut dependencies = Vec::new();
                let mut warnings = Vec::new();
                extract_dependency_groups(&groups, "pyproject.toml", &mut dependencies, &mut warnings);

                prop_assert_eq!(dependencies.len(), edges.len());
                for dep in &dependencies {
                    for path in &dep.included_via {
                        prop_assert!(path.len() >= 2);
                        if let DependencyContext::Group(group) = &dep.context {
                            prop_assert_eq!(path.last(), Some(group));
                        }
                    }
                }
            }
        }
    }
}
