//! Dev-tool plugin extractors for tox, nox, pre-commit, and GitHub Actions.

use std::collections::HashSet;
use std::path::Path;

use crate::config::PluginId;
use crate::path_util::rel_to_root;
use crate::resolver::{VenvIndex, build_binary_map};

use super::commands::command_binaries;
use super::config_text::{is_yaml_block_scalar, leading_spaces, yaml_block_body};
use super::context::PluginContext;
use super::types::{PluginContribution, ReferenceOrigin};
use super::util::{origin_for_file, push_binary, read_pyproject_table};

/// Extract static dev-tool config hints.
#[must_use]
pub(super) fn extract(plugin: PluginId, ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Tox => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "tox",
            &[("tox.ini", "tox.ini")],
            &["tox"],
        ),
        PluginId::Nox => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "nox",
            &[("noxfile.py", "noxfile.py")],
            &["nox"],
        ),
        PluginId::PreCommit => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "pre-commit",
            &[(".pre-commit-config.yaml", ".pre-commit-config.yaml")],
            &["pre-commit", "pre_commit"],
        ),
        PluginId::GithubActions => extract_github_actions(ctx, &mut contrib),
        _ => {},
    }

    contrib
}

fn extract_file_or_tool_table(
    root: &Path,
    contrib: &mut PluginContribution,
    binary: &str,
    files: &[(&str, &str)],
    tool_keys: &[&str],
) {
    for (file_name, label) in files {
        let path = root.join(file_name);
        if !path.is_file() {
            continue;
        }
        push_binary(contrib, binary, origin_for_file(root, &path, *label));
        return;
    }

    let pyproject = root.join("pyproject.toml");
    if !pyproject.is_file() {
        return;
    }
    let Ok(table) = read_pyproject_table(&pyproject) else {
        return;
    };
    let Some(tool) = table.get("tool").and_then(toml::Value::as_table) else {
        return;
    };
    for key in tool_keys {
        if tool.contains_key(*key) {
            push_binary(
                contrib,
                binary,
                origin_for_file(root, &pyproject, format!("tool.{key}")),
            );
            return;
        }
    }
}

fn extract_github_actions(ctx: &PluginContext<'_>, contrib: &mut PluginContribution) {
    let root = ctx.root.path.as_path();
    let workflows_dir = root.join(".github").join("workflows");
    if !workflows_dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&workflows_dir) else {
        return;
    };
    let binary_map = build_binary_map(ctx.config, &VenvIndex::default());
    let known = |name: &str| binary_map.contains_key(name);
    let mut seen = HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !is_workflow_file(&path) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = rel_to_root(root, &path);
        for (line_index, command) in workflow_run_commands(&contents) {
            for binary in command_binaries(&command, &known) {
                let key = (rel.clone(), line_index, binary.clone());
                if !seen.insert(key) {
                    continue;
                }
                push_binary(
                    contrib,
                    &binary,
                    ReferenceOrigin {
                        file: rel.clone(),
                        line: u32::try_from(line_index + 1).ok(),
                        label: "github-actions.run".to_owned(),
                    },
                );
            }
        }
    }
}

pub(super) fn is_workflow_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("yml" | "yaml")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkflowRunValue<'a> {
    indent: usize,
    command: &'a str,
}

pub(super) fn workflow_run_commands(contents: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut commands = Vec::new();
    let mut index = 0;

    while let Some(line) = lines.get(index) {
        let Some(run) = workflow_run_value(line) else {
            index += 1;
            continue;
        };

        if is_yaml_block_scalar(run.command) {
            let (block, cursor) = yaml_block_body(&lines, index + 1, run.indent);
            if !block.trim().is_empty() {
                commands.push((index, block));
            }
            index = cursor;
            continue;
        }

        if !run.command.is_empty() {
            commands.push((index, run.command.to_owned()));
        }
        index += 1;
    }

    commands
}

fn workflow_run_value(line: &str) -> Option<WorkflowRunValue<'_>> {
    let indent = leading_spaces(line);
    let trimmed = line.trim_start();
    let command = trimmed
        .strip_prefix("run:")
        .or_else(|| trimmed.strip_prefix("- run:"))
        .map(str::trim)?;
    Some(WorkflowRunValue { indent, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_run_commands_keep_block_scalars_and_single_lines() {
        let contents = "jobs:\n  t:\n    steps:\n      - run: ruff check .\n      - name: x\n        run: |\n          pytest\n          uv run ruff format\n      - run: mypy src\n";
        let commands = workflow_run_commands(contents);
        assert_eq!(
            commands,
            [
                (3, "ruff check .".to_owned()),
                (5, "pytest\nuv run ruff format".to_owned()),
                (8, "mypy src".to_owned()),
            ]
        );
    }
}
