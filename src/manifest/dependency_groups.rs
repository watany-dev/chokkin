//! PEP 735 `include-group` expansion for `[dependency-groups]`.

use std::collections::{BTreeMap, VecDeque};

use toml::Value;

use super::pep508_util::normalize_distribution_name;
use super::pyproject::push_dependency_table;
use super::types::{DeclaredDependency, DependencyContext, GroupInclude};
use super::warnings::ManifestWarning;

/// One `[dependency-groups]` array as written.
struct GroupEntry<'a> {
    key: &'a str,
    items: &'a [Value],
    /// Indices of directly included groups that exist.
    includes: Vec<usize>,
}

/// Push every group requirement once, under the group that declares it, and
/// record the groups that include it.
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

/// For each group, every group that pulls it in (directly or transitively)
/// with the first hop of the shortest include chain from it. Cycles are
/// reported once and otherwise cut.
fn include_paths(
    entries: &[GroupEntry<'_>],
    rel: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> Vec<Vec<GroupInclude>> {
    let name = |index: usize| entries.get(index).map(|entry| entry.key.to_owned());
    let mut paths = vec![Vec::new(); entries.len()];
    for start in 0..entries.len() {
        let search = bfs(entries, start);
        for (target, slot) in paths.iter_mut().enumerate() {
            if target != start
                && let Some(Some(hop)) = search.first_hop.get(target)
                && let (Some(group), Some(via)) = (name(start), name(*hop))
            {
                slot.push(GroupInclude { group, via });
            }
        }
        let on_cycle = search.parents.get(start).copied().flatten().is_some();
        let cycle = include_chain(&search.parents, start, start);
        // Only the lowest-index member reports, so one cycle is one warning.
        if on_cycle && cycle.iter().all(|&node| node >= start) {
            warnings.push(ManifestWarning::DependencyGroupIncludeCycle {
                file: rel.to_owned(),
                groups: cycle.into_iter().filter_map(name).collect(),
            });
        }
    }
    paths
}

/// The include chains behind `included_via`, each ordered from the including
/// group to the declaring one, following each entry's `via` through the
/// entry of the group it names.
pub(crate) fn include_chains(included_via: &[GroupInclude]) -> Vec<Vec<&str>> {
    let next: BTreeMap<&str, &str> = included_via
        .iter()
        .map(|include| (include.group.as_str(), include.via.as_str()))
        .collect();
    included_via
        .iter()
        .map(|include| {
            let mut chain = vec![include.group.as_str(), include.via.as_str()];
            // Every hop is a shortest chain one step shorter, so the walk
            // ends; the bound only guards a hand-built `included_via`.
            for _ in 0..included_via.len() {
                let Some(&hop) = chain.last().and_then(|last| next.get(last)) else {
                    break;
                };
                chain.push(hop);
            }
            chain
        })
        .collect()
}

/// A BFS over includes from one group.
struct Search {
    /// Predecessor of each group reachable from the start. The start itself
    /// only gets one when it sits on a cycle.
    parents: Vec<Option<usize>>,
    /// The group the start includes first on its way to each one.
    first_hop: Vec<Option<usize>>,
}

fn bfs(entries: &[GroupEntry<'_>], start: usize) -> Search {
    let mut parents = vec![None; entries.len()];
    let mut first_hop = vec![None; entries.len()];
    let mut queue = VecDeque::from([start]);
    while let Some(current) = queue.pop_front() {
        let Some(entry) = entries.get(current) else {
            continue;
        };
        let hop = if current == start {
            None
        } else {
            first_hop.get(current).copied().flatten()
        };
        for &next in &entry.includes {
            if let Some(slot) = parents.get_mut(next)
                && slot.is_none()
            {
                *slot = Some(current);
                if let Some(first) = first_hop.get_mut(next) {
                    *first = Some(hop.unwrap_or(next));
                }
                if next != start {
                    queue.push_back(next);
                }
            }
        }
    }
    Search { parents, first_hop }
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
    use std::fmt::Write as _;

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

    fn via(deps: &[DeclaredDependency], name: &str) -> Vec<Vec<String>> {
        let dep = deps
            .iter()
            .find(|dep| dep.name == name)
            .expect("dependency present");
        include_chains(&dep.included_via)
            .into_iter()
            .map(|chain| chain.into_iter().map(str::to_owned).collect())
            .collect()
    }

    fn chain(groups: &[&str]) -> Vec<String> {
        groups.iter().map(|group| (*group).to_owned()).collect()
    }

    #[test]
    fn requirements_stay_in_declaring_group_with_label_index() {
        let (deps, warnings) = extract(
            "[dependency-groups]\ntest = [\"pytest\"]\ndev = [{include-group = \"test\"}, \"ruff\"]\n",
        );
        assert_eq!(warnings, []);
        assert_eq!(deps.len(), 2);
        let pytest = deps
            .iter()
            .find(|dep| dep.name == "pytest")
            .expect("pytest");
        assert_eq!(pytest.context, DependencyContext::Group("test".to_owned()));
        assert_eq!(pytest.origin.label, "dependency-groups.test[0]");
        assert_eq!(via(&deps, "pytest"), [chain(&["dev", "test"])]);
        let ruff = deps.iter().find(|dep| dep.name == "ruff").expect("ruff");
        assert_eq!(ruff.origin.label, "dependency-groups.dev[1]");
        assert_eq!(via(&deps, "ruff"), Vec::<Vec<String>>::new());
    }

    #[test]
    fn multi_level_includes_record_every_including_group() {
        let (deps, warnings) = extract(
            "[dependency-groups]\nall = [{include-group = \"dev\"}]\ndev = [{include-group = \"test\"}]\ntest = [\"pytest\"]\n",
        );
        assert_eq!(warnings, []);
        assert_eq!(
            via(&deps, "pytest"),
            [chain(&["all", "dev", "test"]), chain(&["dev", "test"])]
        );
    }

    #[test]
    fn a_long_include_line_keeps_one_entry_per_includer() {
        // #1046: spelling out every chain of `n` groups including each other
        // in a line took O(n^3) names.
        let n = 300;
        let mut groups = String::from("[dependency-groups]\n");
        for i in 0..n {
            let include = if i + 1 < n {
                format!(", {{include-group = \"g{}\"}}", i + 1)
            } else {
                String::new()
            };
            writeln!(groups, "g{i} = [\"pkg{i}\"{include}]").expect("write to String");
        }
        let (deps, warnings) = extract(&groups);
        assert_eq!(warnings, []);
        let last = deps
            .iter()
            .find(|dep| dep.name == format!("pkg{}", n - 1))
            .expect("last group's requirement");
        assert_eq!(last.included_via.len(), n - 1);
        let chains = include_chains(&last.included_via);
        assert_eq!(chains.iter().map(Vec::len).max(), Some(n));
    }

    #[test]
    fn include_names_are_normalized() {
        let (deps, warnings) = extract(
            "[dependency-groups]\nDev_Tools = [\"ruff\"]\ndev = [{include-group = \"dev-tools\"}]\n",
        );
        assert_eq!(warnings, []);
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
        assert_eq!(via(&deps, "pytest"), Vec::<Vec<String>>::new());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn includes_name_every_reaching_group_with_a_shortest_chain(
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
                // Direct includes between existing groups, by group index.
                let includes = |from: usize| -> Vec<usize> {
                    edges[from].iter().copied().filter(|&to| to < edges.len()).collect()
                };
                // Include distance from every group to `target`.
                let distances_to = |target: usize| -> Vec<Option<usize>> {
                    let mut distance = vec![None; edges.len()];
                    distance[target] = Some(0);
                    for step in 1..=edges.len() {
                        for from in 0..edges.len() {
                            if distance[from].is_none()
                                && includes(from).iter().any(|&to| distance[to] == Some(step - 1))
                            {
                                distance[from] = Some(step);
                            }
                        }
                    }
                    distance
                };
                let index_of = |group: &str| -> usize {
                    group.trim_start_matches("g-").parse().expect("generated group name")
                };
                for (target, dep) in dependencies.iter().enumerate() {
                    prop_assert_eq!(&dep.context, &DependencyContext::Group(format!("g-{target}")));
                    let distance = distances_to(target);
                    let chains = include_chains(&dep.included_via);
                    let includers: Vec<usize> =
                        chains.iter().map(|chain| index_of(chain[0])).collect();
                    let expected: Vec<usize> = (0..edges.len())
                        .filter(|&from| from != target && distance[from].is_some())
                        .collect();
                    // Every group that reaches this one, once, in group order.
                    prop_assert_eq!(includers, expected);
                    for chain in &chains {
                        let hops: Vec<usize> = chain.iter().map(|group| index_of(group)).collect();
                        // A shortest chain of direct includes ending here.
                        prop_assert_eq!(hops.last(), Some(&target));
                        prop_assert_eq!(Some(hops.len() - 1), distance[hops[0]]);
                        for pair in hops.windows(2) {
                            prop_assert!(includes(pair[0]).contains(&pair[1]), "{chain:?}");
                        }
                    }
                }
            }
        }
    }
}
