//! Commands from task runners and deploy files: Makefile, justfile,
//! Dockerfile / Containerfile, Procfile, `.gitlab-ci.yml` and PDM scripts.
//!
//! Variables, includes and imports are never followed; each command line is
//! read as written.

use std::path::{Path, PathBuf};

use toml::Value;

use super::commands::{KnownBinary, SourceHits};
use super::config_text::{
    PyprojectDoc, is_yaml_block_scalar, leading_spaces, logical_lines, origin_at, read_root_file,
    toml_words, yaml_block_body,
};
use super::types::ReferenceOrigin;

/// Candidates in the order make / just search them; only the first found is read.
const MAKEFILE_NAMES: [&str; 3] = ["GNUmakefile", "makefile", "Makefile"];
const JUSTFILE_NAMES: [&str; 3] = ["justfile", "Justfile", ".justfile"];
const PROCFILE: &str = "Procfile";
const GITLAB_CI: &str = ".gitlab-ci.yml";
const DOCKER_INSTRUCTIONS: [&str; 3] = ["RUN", "CMD", "ENTRYPOINT"];
const GITLAB_SCRIPT_KEYS: [&str; 3] = ["script:", "before_script:", "after_script:"];
const PDM_SCRIPTS: &str = "tool.pdm.scripts";

pub(super) fn input_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = MAKEFILE_NAMES
        .into_iter()
        .chain(JUSTFILE_NAMES)
        .chain([PROCFILE, GITLAB_CI])
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    paths.extend(dockerfile_paths(root));
    paths
}

pub(super) fn scan(
    root: &Path,
    pyproject: Option<&PyprojectDoc>,
    known: KnownBinary<'_>,
) -> SourceHits {
    let mut hits = SourceHits::default();
    if let Some((rel, contents)) = read_first(root, &MAKEFILE_NAMES) {
        scan_makefile(&rel, &contents, known, &mut hits);
    }
    if let Some((rel, contents)) = read_first(root, &JUSTFILE_NAMES) {
        scan_justfile(&rel, &contents, known, &mut hits);
    }
    for path in dockerfile_paths(root) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if let Some((rel, contents)) = read_root_file(root, name) {
            scan_dockerfile(&rel, &contents, known, &mut hits);
        }
    }
    if let Some((rel, contents)) = read_root_file(root, PROCFILE) {
        scan_procfile(&rel, &contents, known, &mut hits);
    }
    if let Some((rel, contents)) = read_root_file(root, GITLAB_CI) {
        GitlabScripts::new(&rel, &contents, known, &mut hits).scan();
    }
    if let Some(doc) = pyproject {
        scan_pdm_scripts(doc, known, &mut hits);
    }
    hits
}

fn read_first(root: &Path, names: &[&str]) -> Option<(String, String)> {
    names.iter().find_map(|name| read_root_file(root, name))
}

/// Root-level Dockerfiles, sorted so first-seen origins are stable.
fn dockerfile_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_dockerfile_name)
        })
        .collect();
    paths.sort();
    paths
}

fn is_dockerfile_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".dockerignore") {
        return false;
    }
    matches!(lower.as_str(), "dockerfile" | "containerfile")
        || lower.ends_with(".dockerfile")
        || lower.starts_with("dockerfile.")
        || lower.starts_with("containerfile.")
}

fn scan_makefile(rel: &str, contents: &str, known: KnownBinary<'_>, hits: &mut SourceHits) {
    for (index, line) in logical_lines(contents) {
        let Some(recipe) = line.strip_prefix('\t') else {
            continue;
        };
        let command = recipe
            .trim()
            .trim_start_matches(['@', '-', '+'])
            .trim_start();
        if !command.starts_with('#') {
            hits.push_command(command, known, &origin_at(rel, index, "make recipe"));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JustState {
    Outside,
    RecipeStart,
    Commands,
    Script,
}

fn scan_justfile(rel: &str, contents: &str, known: KnownBinary<'_>, hits: &mut SourceHits) {
    let mut state = JustState::Outside;
    for (index, line) in logical_lines(contents) {
        state = next_just_state(state, &line);
        let command = line.trim().trim_start_matches(['@', '-']).trim_start();
        if state == JustState::Commands && !command.is_empty() && !command.starts_with('#') {
            hits.push_command(command, known, &origin_at(rel, index, "just recipe"));
        }
    }
}

/// Shebang recipes run another interpreter's source, so only `sh`-family
/// shebangs keep their body readable as shell commands.
fn next_just_state(state: JustState, line: &str) -> JustState {
    if line.trim().is_empty() {
        return state;
    }
    if !line.starts_with([' ', '\t']) {
        return if is_just_recipe_header(line) {
            JustState::RecipeStart
        } else {
            JustState::Outside
        };
    }
    let body = line.trim_start();
    match state {
        JustState::RecipeStart if body.starts_with("#!") && !body.contains("sh") => {
            JustState::Script
        },
        JustState::RecipeStart | JustState::Commands => JustState::Commands,
        other => other,
    }
}

fn is_just_recipe_header(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.starts_with(['#', '[']) && trimmed.contains(':') && !trimmed.contains(":=")
}

fn scan_dockerfile(rel: &str, contents: &str, known: KnownBinary<'_>, hits: &mut SourceHits) {
    for (index, line) in logical_lines(contents) {
        let Some((instruction, rest)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Some(label) = DOCKER_INSTRUCTIONS
            .into_iter()
            .find(|name| name.eq_ignore_ascii_case(instruction))
        else {
            continue;
        };
        let command = docker_command(rest.trim());
        hits.push_command(&command, known, &origin_at(rel, index, label));
    }
}

/// Exec form (`["gunicorn", "app:app"]`) joined into a shell-form command.
fn docker_command(rest: &str) -> String {
    if rest.starts_with('[')
        && let Ok(args) = serde_json::from_str::<Vec<String>>(rest)
    {
        return args.join(" ");
    }
    rest.to_owned()
}

fn scan_procfile(rel: &str, contents: &str, known: KnownBinary<'_>, hits: &mut SourceHits) {
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        let Some((process, command)) = trimmed.split_once(':') else {
            continue;
        };
        if process.is_empty() || process.starts_with('#') || process.contains(char::is_whitespace) {
            continue;
        }
        let label = format!("process {process}");
        hits.push_command(command, known, &origin_at(rel, index, &label));
    }
}

/// `script` / `before_script` / `after_script` values in `.gitlab-ci.yml`:
/// inline scalars and `[a, b]` lists, `- item` lists and `|` / `>` blocks.
struct GitlabScripts<'a, 'h> {
    rel: &'a str,
    lines: Vec<&'a str>,
    known: KnownBinary<'a>,
    hits: &'h mut SourceHits,
}

impl<'a, 'h> GitlabScripts<'a, 'h> {
    fn new(
        rel: &'a str,
        contents: &'a str,
        known: KnownBinary<'a>,
        hits: &'h mut SourceHits,
    ) -> Self {
        Self {
            rel,
            lines: contents.lines().collect(),
            known,
            hits,
        }
    }

    fn scan(&mut self) {
        let mut cursor = 0;
        while let Some(line) = self.lines.get(cursor).copied() {
            let Some(value) = gitlab_script_value(line.trim_start()) else {
                cursor += 1;
                continue;
            };
            let indent = leading_spaces(line);
            cursor = if value.is_empty() {
                self.items(cursor + 1, indent)
            } else {
                self.value(cursor, value, indent)
            };
        }
    }

    /// Reads the value on line `index` and returns the first unread line.
    fn value(&mut self, index: usize, value: &str, indent: usize) -> usize {
        if is_yaml_block_scalar(value) {
            let (body, next) = yaml_block_body(&self.lines, index + 1, indent);
            self.push(&body, index);
            return next;
        }
        for item in inline_items(value) {
            self.push(item, index);
        }
        index + 1
    }

    fn items(&mut self, start: usize, key_indent: usize) -> usize {
        let mut cursor = start;
        while let Some(line) = self.lines.get(cursor).copied() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                cursor += 1;
                continue;
            }
            let indent = leading_spaces(line);
            let Some(item) = trimmed.strip_prefix("- ").filter(|_| indent >= key_indent) else {
                break;
            };
            cursor = self.value(cursor, item.trim(), indent);
        }
        cursor
    }

    fn push(&mut self, command: &str, index: usize) {
        if !command.starts_with("!reference") {
            let origin = origin_at(self.rel, index, "script");
            self.hits.push_command(command, self.known, &origin);
        }
    }
}

fn gitlab_script_value(trimmed: &str) -> Option<&str> {
    let rest = GITLAB_SCRIPT_KEYS
        .into_iter()
        .find_map(|key| trimmed.strip_prefix(key))?
        .trim();
    Some(if rest.starts_with('#') { "" } else { rest })
}

fn inline_items(value: &str) -> Vec<&str> {
    let value = value.split(" #").next().unwrap_or(value).trim();
    let items: Vec<&str> = value.strip_prefix('[').map_or_else(
        || vec![value],
        |list| list.trim_end_matches(']').split(',').collect(),
    );
    items
        .into_iter()
        .map(|item| item.trim().trim_matches(['"', '\'']).trim())
        .filter(|item| !item.is_empty())
        .collect()
}

fn scan_pdm_scripts(doc: &PyprojectDoc, known: KnownBinary<'_>, hits: &mut SourceHits) {
    let Some(scripts) = doc.table(PDM_SCRIPTS) else {
        return;
    };
    for (name, value) in scripts {
        // `_` holds options shared by every script, not a script.
        if name != "_" {
            push_pdm_script(value, &doc.origin(PDM_SCRIPTS, name), known, hits);
        }
    }
}

fn push_pdm_script(
    value: &Value,
    origin: &ReferenceOrigin,
    known: KnownBinary<'_>,
    hits: &mut SourceHits,
) {
    let Some(table) = value.as_table() else {
        if let Some(command) = value.as_str() {
            hits.push_command(command, known, origin);
        }
        return;
    };
    let commands = ["cmd", "shell"]
        .into_iter()
        .filter_map(|key| table.get(key).and_then(toml_words));
    let composite = table
        .get("composite")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.as_str().map(str::to_owned));
    for command in commands.chain(composite) {
        hits.push_command(&command, known, origin);
    }
    if let Some(call) = table.get("call").and_then(Value::as_str) {
        hits.push_module(call.split(':').next().unwrap_or(call), origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(name: &str) -> bool {
        matches!(
            name,
            "pytest" | "gunicorn" | "celery" | "alembic" | "uvicorn" | "coverage" | "mypy"
        )
    }

    fn scanned(
        scan: fn(&str, &str, KnownBinary<'_>, &mut SourceHits),
        contents: &str,
    ) -> Vec<(String, Option<u32>)> {
        let mut hits = SourceHits::default();
        scan("file", contents, &known, &mut hits);
        hits.binaries
            .into_iter()
            .map(|usage| (usage.binary, usage.origin.line))
            .collect()
    }

    #[test]
    fn makefile_reads_recipes_without_following_variables() {
        let contents = "PYTEST = pytest\n\ntest:\n\t@uv run pytest -q\n\t-$(COVERAGE) report\n\t$(shell mypy --version)\n\techo start \\\n\t  alembic upgrade\n\t# celery\nserve:\n\tpython -m gunicorn app:app\n";
        assert_eq!(
            scanned(scan_makefile, contents),
            [
                ("pytest".to_owned(), Some(4)),
                ("gunicorn".to_owned(), Some(11))
            ]
        );
    }

    #[test]
    fn justfile_skips_settings_and_non_shell_shebang_recipes() {
        let contents = "set shell := [\"bash\", \"-c\"]\nalias t := test\n\n[private]\ntest:\n    @pytest -q\n\nmigrate:\n    #!/usr/bin/env python3\n    alembic = 1\n\nserve:\n    #!/usr/bin/env bash\n    celery -A app worker\n";
        assert_eq!(
            scanned(scan_justfile, contents),
            [
                ("pytest".to_owned(), Some(6)),
                ("celery".to_owned(), Some(14))
            ]
        );
    }

    #[test]
    fn dockerfile_reads_shell_and_exec_forms() {
        let contents = "FROM python:3.12\nRUN pip install -r requirements.txt && \\\n    alembic upgrade head\nENTRYPOINT [\"gunicorn\", \"app:app\"]\ncmd uvicorn app:app\nLABEL x=pytest\n";
        assert_eq!(
            scanned(scan_dockerfile, contents),
            [
                ("alembic".to_owned(), Some(2)),
                ("gunicorn".to_owned(), Some(4)),
                ("uvicorn".to_owned(), Some(5)),
            ]
        );
    }

    #[test]
    fn procfile_reads_process_commands() {
        let contents = "web: gunicorn app:app\n# worker: pytest\nworker: celery -A app worker\n";
        assert_eq!(
            scanned(scan_procfile, contents),
            [
                ("gunicorn".to_owned(), Some(1)),
                ("celery".to_owned(), Some(3))
            ]
        );
    }

    #[test]
    fn gitlab_ci_reads_script_lists_inline_values_and_blocks() {
        let contents = "test:\n  before_script: [\"uv sync\", \"alembic upgrade head\"]\n  script:\n    - pytest -q\n    - !reference [.setup, script]\n    - |\n      celery -A app inspect ping\n  after_script: coverage report # tail\ndeploy:\n  script: >\n    gunicorn app:app\n  variables:\n    X: pytest\n";
        let mut hits = SourceHits::default();
        GitlabScripts::new("ci", contents, &known, &mut hits).scan();
        let found: Vec<_> = hits
            .binaries
            .into_iter()
            .map(|usage| (usage.binary, usage.origin.line))
            .collect();
        assert_eq!(
            found,
            [
                ("alembic".to_owned(), Some(2)),
                ("pytest".to_owned(), Some(4)),
                ("celery".to_owned(), Some(6)),
                ("coverage".to_owned(), Some(8)),
                ("gunicorn".to_owned(), Some(10)),
            ]
        );
    }

    #[test]
    fn dockerfile_names_are_matched_case_insensitively() {
        for name in [
            "Dockerfile",
            "dockerfile",
            "api.Dockerfile",
            "Dockerfile.dev",
            "Containerfile",
        ] {
            assert!(is_dockerfile_name(name), "{name}");
        }
        assert!(!is_dockerfile_name("Dockerfile.dockerignore"));
        assert!(!is_dockerfile_name("docker-compose.yml"));
    }
}
