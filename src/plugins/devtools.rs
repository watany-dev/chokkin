//! Dev-tool plugin extractors for tox, nox, pre-commit, and GitHub Actions.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::config::PluginId;
use crate::resolver::{VenvIndex, build_binary_map};

use super::context::PluginContext;
use super::types::{BinaryUsage, PluginContribution, ReferenceOrigin};
use super::util::{read_pyproject_table, relative_path};
use super::warnings::PluginsWarning;

/// Extract static dev-tool config hints.
#[must_use]
pub fn extract(
    plugin: PluginId,
    ctx: &PluginContext<'_>,
) -> (PluginContribution, Vec<PluginsWarning>) {
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
        _ => {}
    }

    let warnings = if contrib.binary_usages.is_empty() {
        vec![PluginsWarning::PluginNoOp { plugin }]
    } else {
        Vec::new()
    };
    (contrib, warnings)
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
        push_binary(
            contrib,
            binary,
            ReferenceOrigin {
                file: relative_path(root, &path),
                line: None,
                label: (*label).to_owned(),
            },
        );
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
                ReferenceOrigin {
                    file: relative_path(root, &pyproject),
                    line: None,
                    label: format!("tool.{key}"),
                },
            );
            return;
        }
    }
}

fn push_binary(contrib: &mut PluginContribution, binary: &str, origin: ReferenceOrigin) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
    });
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
    let mut seen = HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !is_workflow_file(&path) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = relative_path(root, &path);
        for (line_index, command) in workflow_run_commands(&contents) {
            for binary in command_known_binaries(&command, &binary_map) {
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

fn is_workflow_file(path: &Path) -> bool {
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

fn workflow_run_commands(contents: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut commands = Vec::new();
    let mut index = 0;

    while let Some(line) = lines.get(index) {
        let Some(run) = workflow_run_value(line) else {
            index += 1;
            continue;
        };

        if is_workflow_block_scalar(run.command) {
            let mut block = String::new();
            let mut cursor = index + 1;
            while let Some(block_line) = lines.get(cursor) {
                if block_line.trim().is_empty() {
                    block.push('\n');
                    cursor += 1;
                    continue;
                }
                if leading_spaces(block_line) <= run.indent {
                    break;
                }
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(block_line.trim_start());
                cursor += 1;
            }
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

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn is_workflow_block_scalar(command: &str) -> bool {
    let trimmed = command.trim_start();
    trimmed.starts_with('|') || trimmed.starts_with('>')
}

fn command_known_binaries(
    command: &str,
    binary_map: &BTreeMap<String, String>,
) -> Vec<String> {
    let tokens = command
        .split_whitespace()
        .map(clean_command_token)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut binaries = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if binary_map.contains_key(token.as_str()) {
            binaries.push(token.clone());
        }
        if is_python_binary(token)
            && tokens.get(index + 1).is_some_and(|next| next == "-m")
            && let Some(module) = tokens.get(index + 2)
            && binary_map.contains_key(module.as_str())
        {
            binaries.push(module.clone());
        }
    }
    binaries
}

fn clean_command_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_' | '.')
        })
        .to_owned()
}

fn is_python_binary(token: &str) -> bool {
    token == "python" || token == "python3" || token == "py"
}
