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
    let joined = command.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut binaries: Vec<String> = Vec::new();
    for segment in joined.split(['\n', ';', '|', '&']) {
        let words: Vec<&str> = segment
            .split_whitespace()
            .map(clean_word)
            .filter(|word| !word.is_empty())
            .collect();
        let start = command_start(&words);
        if let Some(binary) = words
            .get(start..)
            .and_then(|rest| command_binary(rest, known))
            && !binaries.contains(&binary)
        {
            binaries.push(binary);
        }
    }
    binaries
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
        assert!(binaries("echo pytest").is_empty());
        assert!(binaries("$(COVERAGE) run -m pytest").is_empty());
        assert!(binaries("echo $(shell pytest --version)").is_empty());
        assert!(binaries("${PYTEST} tests").is_empty());
        assert!(binaries("pip install celery").is_empty());
        assert!(binaries("python manage.py migrate").is_empty());
    }

    #[test]
    fn backslash_continuations_stay_in_one_segment() {
        assert!(binaries("echo start \\\n  pytest").is_empty());
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
}
