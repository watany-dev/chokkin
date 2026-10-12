//! Static shell-command reading shared by the task-file and tool-config scanners.
//!
//! Only the command word of each `;` / `|` / `&` / newline separated segment is
//! considered, after skipping env assignments, wrappers (`env`, `uv run`, ...)
//! and `python -m`. Arguments, `$(...)` substitutions and variables are never
//! treated as commands, so `echo pytest` or `$(COVERAGE) run` stay silent.

use super::plugin_map::plugin_module_distribution;
use super::types::{BinaryUsage, ModuleReference, ReferenceOrigin};

/// Predicate for binary names worth reporting (the binary map plus known tools).
pub(super) type KnownBinary<'a> = &'a dyn Fn(&str) -> bool;

/// Environment managers that install rather than use dependencies; reporting
/// `pip install` in a Dockerfile as an undeclared `pip` would only be noise.
const ENVIRONMENT_TOOLS: [&str; 11] = [
    "pip", "pip3", "uv", "uvx", "pipx", "poetry", "pdm", "hatch", "rye", "pipenv", "conda",
];
const WRAPPERS: [&str; 10] = [
    "exec", "env", "time", "nohup", "command", "sudo", "then", "do", "else", "!",
];
const RUNNERS: [&str; 6] = ["uv", "poetry", "pdm", "hatch", "rye", "pipenv"];

/// Hits collected by one scanner, merged into `ConfigScanResult` by the caller.
#[derive(Debug, Default)]
pub(super) struct SourceHits {
    pub(super) binaries: Vec<BinaryUsage>,
    pub(super) distributions: Vec<String>,
    pub(super) module_refs: Vec<ModuleReference>,
}

impl SourceHits {
    pub(super) fn push_command(
        &mut self,
        command: &str,
        known: KnownBinary<'_>,
        origin: &ReferenceOrigin,
    ) {
        for binary in command_binaries(command, known) {
            self.binaries.push(BinaryUsage {
                binary,
                origin: origin.clone(),
                config_section: false,
            });
        }
    }

    pub(super) fn push_module(&mut self, module: &str, origin: &ReferenceOrigin) {
        let module = module.trim();
        if is_dotted_identifier(module) {
            self.module_refs.push(ModuleReference {
                module: module.to_owned(),
                origin: origin.clone(),
            });
        }
    }

    /// Plugin modules whose import root does not match their distribution
    /// (`xdist` → pytest-xdist) are recorded as distributions directly.
    pub(super) fn push_plugin_module(&mut self, module: &str, origin: &ReferenceOrigin) {
        if let Some(distribution) = plugin_module_distribution(module) {
            self.distributions.push(distribution.to_owned());
            return;
        }
        self.push_module(module, origin);
    }
}

/// Known binaries invoked as the command word of `command`, deduplicated.
pub(super) fn command_binaries(command: &str, known: KnownBinary<'_>) -> Vec<String> {
    let mut binaries: Vec<String> = Vec::new();
    for_each_segment(command, |words| {
        let start = command_start(words);
        if let Some(binary) = words
            .get(start..)
            .and_then(|rest| command_binary(rest, known))
            && !binaries.contains(&binary)
        {
            binaries.push(binary);
        }
    });
    binaries
}

/// Calls `visit` with the cleaned words of each `;` / `|` / `&` / newline
/// separated segment, joining backslash continuations first.
fn for_each_segment(command: &str, mut visit: impl FnMut(&[&str])) {
    let joined = command.replace("\\\r\n", " ").replace("\\\n", " ");
    for segment in joined.split(['\n', ';', '|', '&']) {
        let words: Vec<&str> = segment
            .split_whitespace()
            .map(clean_word)
            .filter(|word| !word.is_empty())
            .collect();
        visit(&words);
    }
}

/// A `.py` file a command runs by path, with the directory (`cd`, `uv run
/// --directory`) it is relative to.
pub(super) struct ScriptRun {
    pub(super) directory: Option<String>,
    pub(super) path: String,
}

/// `uv run` options whose value is the next word.
const UV_RUN_VALUE_OPTIONS: [&str; 26] = [
    "-p",
    "--python",
    "-w",
    "--with",
    "--with-editable",
    "--with-requirements",
    "--group",
    "--no-group",
    "--only-group",
    "--extra",
    "--no-extra",
    "--package",
    "--project",
    "--env-file",
    "--index",
    "-i",
    "--index-url",
    "--default-index",
    "-f",
    "--find-links",
    "-C",
    "--config-setting",
    "--exclude-newer",
    "-P",
    "--upgrade-package",
    "--resolution",
];

/// Scripts run as `python[3] [flags] <path>.py` or `uv run [options]
/// [python] <path>.py` (`-s` / `--script` included) in `command` (#797).
///
/// A `cd <dir>` segment applies to the segments after it.
pub(super) fn command_scripts(command: &str) -> Vec<ScriptRun> {
    let mut cwd: Option<String> = None;
    let mut scripts = Vec::new();
    for_each_segment(command, |words| {
        let rest = words.get(command_start(words)..).unwrap_or_default();
        if let [cd, dir] = rest
            && command_name(cd) == "cd"
        {
            cwd = Some(
                cwd.as_ref()
                    .map_or_else(|| (*dir).to_owned(), |cwd| format!("{cwd}/{dir}")),
            );
            return;
        }
        if let Some(mut run) = segment_script(words) {
            if let Some(cwd) = &cwd {
                run.directory = Some(
                    run.directory
                        .map_or_else(|| cwd.clone(), |dir| format!("{cwd}/{dir}")),
                );
            }
            scripts.push(run);
        }
    });
    scripts
}

fn segment_script(words: &[&str]) -> Option<ScriptRun> {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let rest = words.get(index + 1..).unwrap_or_default();
        if command_name(word) == "uv" && rest.first() == Some(&"run") {
            return uv_run_script(rest.get(1..)?);
        }
        if is_python_word(word) {
            return python_script(rest).map(|path| ScriptRun {
                directory: None,
                path: path.to_owned(),
            });
        }
        let width = prefix_width(word, rest.first().copied());
        if width == 0 {
            return None;
        }
        index += width;
    }
    None
}

fn python_script<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg.starts_with("-m") || arg.starts_with("-c") {
            return None;
        }
        if matches!(*arg, "-X" | "-W") {
            args.next();
        } else if !arg.starts_with('-') {
            return is_python_path(arg).then_some(*arg);
        }
    }
    None
}

fn uv_run_script(args: &[&str]) -> Option<ScriptRun> {
    let mut directory = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if let Some(value) = arg.strip_prefix("--directory=") {
            directory = Some(value);
        } else if *arg == "--directory" {
            directory = args.get(index).copied();
            index += 1;
        } else if UV_RUN_VALUE_OPTIONS.contains(arg) {
            index += 1;
        } else if !arg.starts_with('-') {
            let path = if is_python_word(arg) {
                python_script(args.get(index..)?)?
            } else {
                is_python_path(arg).then_some(*arg)?
            };
            return Some(ScriptRun {
                directory: directory.map(str::to_owned),
                path: path.to_owned(),
            });
        }
    }
    None
}

fn is_python_path(word: &str) -> bool {
    std::path::Path::new(word)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
}

fn clean_word(word: &str) -> &str {
    let word = word.trim_matches(['"', '\'', '`']);
    if word.starts_with('$') {
        word
    } else {
        word.trim_matches(['(', ')'])
    }
}

fn command_start(words: &[&str]) -> usize {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let width = prefix_width(word, words.get(index + 1).copied());
        if width == 0 {
            break;
        }
        index += width;
    }
    index
}

fn prefix_width(word: &str, next: Option<&str>) -> usize {
    if word.starts_with('-') || is_env_assignment(word) || WRAPPERS.contains(&word) {
        return 1;
    }
    let name = command_name(word);
    let takes_script = matches!(name, "sh" | "bash") && next == Some("-c");
    let runs_command = RUNNERS.contains(&name) && next == Some("run");
    if takes_script || runs_command { 2 } else { 0 }
}

fn is_env_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

fn command_binary(words: &[&str], known: KnownBinary<'_>) -> Option<String> {
    let first = *words.first()?;
    if first.starts_with('$') || first.starts_with("{{") {
        return None;
    }
    if is_python_word(first) {
        return python_module(words.get(1..)?, known);
    }
    known_binary(command_name(first), known)
}

/// Module run by `python [flags] -m module`, as a binary name.
fn python_module(args: &[&str], known: KnownBinary<'_>) -> Option<String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let module = match arg.strip_prefix("-m") {
            Some("") => args.next().copied()?,
            Some(module) => module,
            None if arg.starts_with('-') => continue,
            None => return None,
        };
        return known_binary(module.split('.').next().unwrap_or(module), known);
    }
    None
}

fn is_python_word(word: &str) -> bool {
    let name = command_name(word).to_ascii_lowercase();
    name == "py"
        || name
            .strip_prefix("python")
            .is_some_and(|version| version.chars().all(|ch| ch.is_ascii_digit() || ch == '.'))
}

/// Basename without a Windows `.exe` suffix.
fn command_name(word: &str) -> &str {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let stem_len = name.len().saturating_sub(4);
    match (name.get(..stem_len), name.get(stem_len..)) {
        (Some(stem), Some(ext)) if !stem.is_empty() && ext.eq_ignore_ascii_case(".exe") => stem,
        _ => name,
    }
}

fn known_binary(name: &str, known: KnownBinary<'_>) -> Option<String> {
    (!ENVIRONMENT_TOOLS.contains(&name) && known(name)).then(|| name.to_owned())
}

fn is_dotted_identifier(module: &str) -> bool {
    !module.is_empty()
        && module.split('.').all(|part| {
            part.chars()
                .next()
                .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
                && part.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(name: &str) -> bool {
        matches!(
            name,
            "pytest" | "gunicorn" | "celery" | "alembic" | "coverage" | "pip" | "uvicorn"
        )
    }

    fn binaries(command: &str) -> Vec<String> {
        command_binaries(command, &known)
    }

    #[test]
    fn reads_command_word_after_wrappers() {
        assert_eq!(binaries("uv run pytest -q"), ["pytest"]);
        assert_eq!(binaries("FOO=1 exec gunicorn app:app"), ["gunicorn"]);
        assert_eq!(binaries("python -m celery -A app worker"), ["celery"]);
        assert_eq!(binaries(".venv/bin/python3.12 -u -m pytest"), ["pytest"]);
        assert_eq!(binaries("sh -c \"alembic upgrade head\""), ["alembic"]);
        assert_eq!(binaries("cd app && uvicorn main:app"), ["uvicorn"]);
        assert_eq!(binaries("C:\\venv\\Scripts\\pytest.exe"), ["pytest"]);
    }

    #[test]
    fn ignores_arguments_substitutions_and_environment_tools() {
        assert_eq!(binaries("echo pytest"), Vec::<String>::new());
        assert_eq!(binaries("$(COVERAGE) run -m pytest"), Vec::<String>::new());
        assert_eq!(
            binaries("echo $(shell pytest --version)"),
            Vec::<String>::new()
        );
        assert_eq!(binaries("${PYTEST} tests"), Vec::<String>::new());
        assert_eq!(binaries("pip install celery"), Vec::<String>::new());
        assert_eq!(binaries("python manage.py migrate"), Vec::<String>::new());
    }

    #[test]
    fn backslash_continuations_stay_in_one_segment() {
        assert_eq!(binaries("echo start \\\n  pytest"), Vec::<String>::new());
        assert_eq!(binaries("pytest \\\r\n  --cov"), ["pytest"]);
    }

    #[test]
    fn plugin_modules_map_to_distributions_or_module_refs() {
        let origin = ReferenceOrigin {
            file: "pytest.ini".to_owned(),
            line: Some(2),
            label: "pytest.addopts".to_owned(),
        };
        let mut hits = SourceHits::default();
        hits.push_plugin_module("xdist", &origin);
        hits.push_plugin_module("pytest_django", &origin);
        hits.push_module("not a module", &origin);
        assert_eq!(hits.distributions, ["pytest-xdist"]);
        let modules: Vec<_> = hits.module_refs.iter().map(|m| m.module.as_str()).collect();
        assert_eq!(modules, ["pytest_django"]);
    }

    fn scripts(command: &str) -> Vec<(Option<String>, String)> {
        command_scripts(command)
            .into_iter()
            .map(|run| (run.directory, run.path))
            .collect()
    }

    #[test]
    fn command_scripts_read_python_and_uv_run_paths() {
        let script = |path: &str| (None, path.to_owned());
        assert_eq!(
            scripts("python3 -u fastmcp/tests/_harness.py junit.xml"),
            [script("fastmcp/tests/_harness.py")]
        );
        assert_eq!(
            scripts("FOO=1 uv run --frozen -w rich scripts/gen.py --check"),
            [script("scripts/gen.py")]
        );
        assert_eq!(
            scripts("sudo -E bash -c \"poetry run python tools/a.py\""),
            [script("tools/a.py")]
        );
        assert_eq!(
            scripts("cd docs\npython build.py && uv run --directory api gen.py"),
            [
                (Some("docs".to_owned()), "build.py".to_owned()),
                (Some("docs/api".to_owned()), "gen.py".to_owned()),
            ]
        );
        assert_eq!(
            scripts("uv run -p 3.12 --group codegen --directory task-sdk -s dev/gen.py"),
            [(Some("task-sdk".to_owned()), "dev/gen.py".to_owned())]
        );
        assert_eq!(
            scripts("uv run --directory=docs python -X dev build.py"),
            [(Some("docs".to_owned()), "build.py".to_owned())]
        );
        assert_eq!(scripts("python -m pytest tests/test_a.py"), []);
        assert_eq!(scripts("uv run pytest tests/test_a.py"), []);
        assert_eq!(scripts("uv run --with gen.py ruff check"), []);
        assert_eq!(scripts("echo python gen.py"), []);
    }
}
