//! Static shell-command reading shared by the task-file and tool-config scanners.
//!
//! Only the command word of each `;` / `|` / `&` / newline separated segment is
//! considered, after skipping env assignments, wrappers (`env`, `uv run`, ...)
//! and `python -m`. Arguments, `$(...)` substitutions and variables are never
//! treated as commands, so `echo pytest` or `$(COVERAGE) run` stay silent.

use crate::parser::ParseSummary;

use super::plugin_map::plugin_module_distribution;
use super::types::{
    BinaryUsage, CommandTarget, CommandTargetRef, ModuleReference, ReferenceOrigin,
};

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
    pub(super) command_targets: Vec<CommandTargetRef>,
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
        self.command_targets
            .extend(command_target_refs(command, origin));
    }

    /// [`Self::push_command`] for a command the deployed image or process
    /// runs (#912).
    pub(super) fn push_deploy_command(
        &mut self,
        command: &str,
        known: KnownBinary<'_>,
        origin: &ReferenceOrigin,
    ) {
        let start = self.command_targets.len();
        self.push_command(command, known, origin);
        for target in self.command_targets.get_mut(start..).unwrap_or_default() {
            target.deploy = true;
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

/// [`command_targets`] as records the gap stage reads (#913). A script's
/// path is joined to its directory; an absolute one is tried by each of its
/// suffixes, since an image or CI job may check the project out anywhere, and
/// one that leaves the root names no project file, so it is dropped.
pub(super) fn command_target_refs(
    command: &str,
    origin: &ReferenceOrigin,
) -> Vec<CommandTargetRef> {
    command_targets(command)
        .into_iter()
        .filter_map(|target| {
            let target = match target {
                PythonTarget::Script(run) => script_target(&run)?,
                PythonTarget::Run(module) => CommandTarget::Run { module },
                PythonTarget::Module(module) => CommandTarget::Module { module },
                PythonTarget::App { module, symbol } => CommandTarget::App { module, symbol },
                PythonTarget::Opaque(text) => CommandTarget::Opaque { text },
            };
            Some(CommandTargetRef {
                target,
                origin: origin.clone(),
                deploy: false,
            })
        })
        .collect()
}

/// The apps literal `uvicorn.run`/`uvicorn.Config` calls load, read as a
/// `uvicorn` command's app (#1004); one that is no name path is dropped, as
/// uvicorn fails to load it, and one `app_dir=` relocates is opaque.
pub(super) fn parsed_server_apps(parse: &ParseSummary) -> impl Iterator<Item = CommandTargetRef> {
    parse.modules.iter().flat_map(|parsed| {
        parsed.server_apps.iter().filter_map(|server| {
            let target = match runner_target("uvicorn", &[&server.app]) {
                PythonTarget::App { module, .. } if server.app_dir => {
                    CommandTarget::Opaque { text: module }
                },
                PythonTarget::App { module, symbol } => CommandTarget::App { module, symbol },
                _ => return None,
            };
            Some(CommandTargetRef {
                target,
                origin: ReferenceOrigin {
                    file: parsed.path.clone(),
                    line: Some(server.line),
                    label: server.call.clone(),
                },
                deploy: false,
            })
        })
    })
}

fn script_target(run: &ScriptRun) -> Option<CommandTarget> {
    let parts: Vec<&str> = run
        .directory
        .iter()
        .map(String::as_str)
        .chain([run.path.as_str()])
        .collect();
    if parts.iter().any(|part| is_computed(part)) {
        return Some(CommandTarget::Opaque {
            text: run.path.clone(),
        });
    }
    let absolute = parts.iter().any(|part| part.starts_with(['/', '\\']));
    let mut components = Vec::new();
    for part in parts {
        if part.starts_with(['/', '\\']) {
            components.clear();
        }
        for component in part.split(['/', '\\']) {
            match component {
                "" | "." => {},
                ".." => {
                    components.pop()?;
                },
                name => components.push(name),
            }
        }
    }
    let paths = if absolute {
        (0..components.len())
            .filter_map(|start| components.get(start..))
            .map(|rest| rest.join("/"))
            .collect()
    } else {
        vec![components.join("/")]
    };
    Some(CommandTarget::Script { paths })
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
    for segment in join_continuations(command).split(['\n', ';', '|', '&']) {
        let words: Vec<&str> = segment
            .split_whitespace()
            .map(clean_word)
            .filter(|word| !word.is_empty())
            .collect();
        visit(&words);
    }
}

fn join_continuations(command: &str) -> String {
    command.replace("\\\r\n", " ").replace("\\\n", " ")
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
    command_targets(command)
        .into_iter()
        .filter_map(|target| match target {
            PythonTarget::Script(run) => Some(run),
            PythonTarget::Run(_)
            | PythonTarget::Module(_)
            | PythonTarget::App { .. }
            | PythonTarget::Opaque(_) => None,
        })
        .collect()
}

/// Python code one command runs (#913).
pub(super) enum PythonTarget {
    /// A `.py` file run by path.
    Script(ScriptRun),
    /// A module run with `-m` (#912).
    Run(String),
    /// A module a task runner loads or `-c` code imports, or one run with
    /// `-m` from a directory the command moves the import root to.
    Module(String),
    /// The application a WSGI/ASGI server loads (#1004).
    App {
        /// Dotted module name.
        module: String,
        /// The attribute path served, when the command names one or the
        /// server has a default.
        symbol: Option<String>,
    },
    /// A variable, a substitution, or an argument shape that is not read.
    Opaque(String),
}

/// WSGI/ASGI servers and task runners whose argument names the module they load.
const MODULE_RUNNERS: [&str; 7] = [
    "gunicorn",
    "uvicorn",
    "hypercorn",
    "daphne",
    "granian",
    "waitress-serve",
    "celery",
];

/// The attribute a server loads from a module named without one; uvicorn,
/// daphne and waitress-serve require it.
fn default_app_attr(runner: &str) -> Option<&'static str> {
    match runner {
        "gunicorn" => Some("application"),
        "hypercorn" | "granian" => Some("app"),
        _ => None,
    }
}

/// What each segment that runs Python (a `python` word, `uv run`, or one of
/// [`MODULE_RUNNERS`]) runs. A segment whose command word is anything else, a
/// variable included, runs nothing this reads; a bare `python` or one with
/// flags alone runs no file. A `cd <dir>` segment applies to the scripts
/// after it.
pub(super) fn command_targets(command: &str) -> Vec<PythonTarget> {
    let mut cwd: Option<String> = None;
    let mut targets = Vec::new();
    let mut runs_code = false;
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
        match segment_target(words) {
            Some(Run::Target(PythonTarget::Script(mut run))) => {
                if let Some(cwd) = &cwd {
                    run.directory = Some(
                        run.directory
                            .map_or_else(|| cwd.clone(), |dir| format!("{cwd}/{dir}")),
                    );
                }
                targets.push(PythonTarget::Script(run));
            },
            Some(Run::Target(target)) if cwd.is_some() || sets_pythonpath(words) => {
                targets.push(relocated(target));
            },
            Some(Run::Target(target)) => targets.push(target),
            Some(Run::Code) => runs_code = true,
            None => {},
        }
    });
    if runs_code {
        targets.extend(code_targets(command));
    }
    targets
}

/// `PYTHONPATH=src uvicorn app:app`: the app's module may be imported from
/// the directory the assignment names.
fn sets_pythonpath(words: &[&str]) -> bool {
    words.iter().any(|word| word.starts_with("PYTHONPATH="))
}

/// A segment's target, or `python -c` code, which the whole command holds
/// since its `;` split it into segments.
enum Run {
    Target(PythonTarget),
    Code,
}

fn segment_target(words: &[&str]) -> Option<Run> {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let rest = words.get(index + 1..).unwrap_or_default();
        if command_name(word) == "uv" && rest.first() == Some(&"run") {
            return uv_run_target(rest.get(1..)?);
        }
        if command_name(word) == "pipx" && rest.first() == Some(&"run") {
            return pipx_run_target(rest.get(1..)?);
        }
        if is_python_word(word) || is_python_variable(word) {
            return python_target(rest);
        }
        if let Some(runner) = module_runner(word) {
            return Some(Run::Target(runner_target(runner, rest)));
        }
        let width = prefix_width(word, rest.first().copied());
        if width == 0 {
            return None;
        }
        index += width;
    }
    None
}

/// `$(PYTHON)`, `${PYTHON3}`, `{{python}}`: an interpreter a task file names
/// by variable.
fn is_python_variable(word: &str) -> bool {
    (word.starts_with('$') || word.starts_with("{{"))
        && word
            .trim_matches(['$', '(', ')', '{', '}', ' '])
            .to_ascii_lowercase()
            .contains("python")
}

fn module_runner(word: &str) -> Option<&'static str> {
    let name = command_name(word);
    MODULE_RUNNERS.into_iter().find(|runner| *runner == name)
}

pub(super) fn is_computed(word: &str) -> bool {
    word.contains(['$', '{', '}', '%', '*'])
}

fn python_target(args: &[&str]) -> Option<Run> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if let Some(module) = arg.strip_prefix("-m") {
            let rest = args.get(index..).unwrap_or_default();
            let (module, rest) = if module.is_empty() {
                let (module, rest) = rest.split_first()?;
                (*module, rest)
            } else {
                (module, rest)
            };
            return Some(Run::Target(match module_runner(module) {
                Some(runner) => runner_target(runner, rest),
                None => module_target(module),
            }));
        }
        if arg.starts_with("-c") {
            return Some(Run::Code);
        }
        if matches!(*arg, "-X" | "-W") {
            index += 1;
        } else if *arg == "-" {
            return Some(Run::Target(PythonTarget::Opaque((*arg).to_owned())));
        } else if !arg.starts_with('-') {
            return Some(Run::Target(path_target(arg)));
        }
    }
    None
}

fn module_target(module: &str) -> PythonTarget {
    if is_dotted_identifier(module) {
        PythonTarget::Run(module.to_owned())
    } else {
        PythonTarget::Opaque(module.to_owned())
    }
}

/// A positional `python` argument: a `.py` path, or else something this does
/// not place (a directory or zip app with a `__main__`, a computed path).
fn path_target(arg: &str) -> PythonTarget {
    if !is_computed(arg) && is_python_path(arg) {
        PythonTarget::Script(ScriptRun {
            directory: None,
            path: arg.to_owned(),
        })
    } else {
        PythonTarget::Opaque(arg.to_owned())
    }
}

/// The module a runner loads: celery's `-A` / `--app`, else the first
/// `module:attr` argument, else a lone dotted positional (gunicorn's
/// `module` form). An app named only in a config file is not read.
///
/// A server's app keeps its attribute: the name before a factory's `(`, or
/// the server's default for a bare module. An attribute that is no name path
/// (`apps[0]`) leaves the module alone, and an option that moves the import
/// root makes the app opaque.
fn runner_target(runner: &str, args: &[&str]) -> PythonTarget {
    let app = if runner == "celery" {
        celery_app(args)
    } else {
        server_app(args)
    };
    let Some(app) = app else {
        return PythonTarget::Opaque(runner.to_owned());
    };
    let (module, attr) = app
        .split_once(':')
        .map_or((app, None), |(module, attr)| (module, Some(attr)));
    if is_computed(app) || !is_dotted_identifier(module) {
        return PythonTarget::Opaque(app.to_owned());
    }
    if runner == "celery" {
        return PythonTarget::Module(module.to_owned());
    }
    if args.iter().any(|arg| moves_import_root(arg)) {
        return PythonTarget::Opaque(module.to_owned());
    }
    let symbol = match attr {
        Some(attr) => {
            let name = attr.split_once('(').map_or(attr, |(name, _)| name);
            is_dotted_identifier(name).then(|| name.to_owned())
        },
        None => default_app_attr(runner).map(str::to_owned),
    };
    PythonTarget::App {
        module: module.to_owned(),
        symbol,
    }
}

/// Options that import the app from another directory: uvicorn's
/// `--app-dir`, gunicorn's `--chdir` and `--pythonpath`, granian's
/// `--working-dir`.
fn moves_import_root(arg: &str) -> bool {
    let option = arg.split_once('=').map_or(arg, |(option, _)| option);
    matches!(
        option,
        "--app-dir" | "--chdir" | "--pythonpath" | "--working-dir"
    )
}

/// An app imported from a directory the command changes to or puts on the
/// import path: which file its module names is unknown, so it is opaque
/// rather than a guess (ADR 0005). A module run from there is no entry
/// either, but the gap stage still checks the file it names in the root.
fn relocated(target: PythonTarget) -> PythonTarget {
    match target {
        PythonTarget::App { module, .. } => PythonTarget::Opaque(module),
        PythonTarget::Run(module) => PythonTarget::Module(module),
        target => target,
    }
}

fn celery_app<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if matches!(*arg, "-A" | "--app") {
            return args.next().copied();
        }
        if let Some(app) = arg
            .strip_prefix("--app=")
            .or_else(|| arg.strip_prefix("-A"))
        {
            return Some(app);
        }
    }
    None
}

fn server_app<'a>(args: &[&'a str]) -> Option<&'a str> {
    // A word right after an option may be its value (`-c python:conf`,
    // `-k egg:dist#worker`), so such a reference is the last resort; after a
    // known flag (`--reload mypkg:app`) or an option holding its value
    // (`-w4 mypkg.wsgi`) it is the app.
    let follows_option = |index: usize| index > 0 && takes_value(args[index - 1]);
    let unbound = || {
        (0..args.len())
            .filter(|&index| !follows_option(index))
            .map(|index| args[index])
    };
    unbound()
        .find(|arg| is_app_reference(arg))
        .or_else(|| {
            unbound().find(|arg| {
                !arg.starts_with('-') && (is_computed(arg) || is_dotted_identifier(arg))
            })
        })
        .or_else(|| args.iter().copied().find(|arg| is_app_reference(arg)))
}

/// Server options that take no value, so the next word is not theirs.
const SERVER_FLAGS: [&str; 22] = [
    "--access-log",
    "--call",
    "--capture-output",
    "--check-config",
    "--daemon",
    "--date-header",
    "--debug",
    "--enable-stdio-inheritance",
    "--expose-tracebacks",
    "--factory",
    "--ipv4",
    "--ipv6",
    "--preload",
    "--print-config",
    "--proxy-headers",
    "--reload",
    "--reuse-port",
    "--server-header",
    "--use-colors",
    "--ws",
    "-D",
    "-R",
];

/// Whether `arg` is an option whose value is the next word: not `--opt=value`
/// or `-w4`, a known flag, or a `--no-*` switch. An unknown long option is
/// read as taking one.
fn takes_value(arg: &str) -> bool {
    if arg.contains('=') || SERVER_FLAGS.contains(&arg) || arg.starts_with("--no-") {
        return false;
    }
    arg.starts_with("--") || (arg.starts_with('-') && arg.len() == 2)
}

/// `module:attr`, `module:factory()` or a computed `$APP_MODULE`-like word
/// with a colon.
fn is_app_reference(arg: &str) -> bool {
    arg.split_once(':').is_some_and(|(module, attr)| {
        (is_dotted_identifier(module) || is_computed(module))
            && !attr.is_empty()
            && !attr.starts_with('/')
            // `-b localhost:8000` binds a port; it names no app.
            && !attr.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn uv_run_target(args: &[&str]) -> Option<Run> {
    let mut directory = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if let Some(value) = arg.strip_prefix("--directory=") {
            directory = Some(value);
        } else if *arg == "--directory" {
            directory = args.get(index).copied();
            index += 1;
        } else if matches!(*arg, "-m" | "--module") {
            // `uv run -m gunicorn app:app` runs the server as `python -m` does.
            let (module, rest) = args.get(index..)?.split_first()?;
            let target = match module_runner(module) {
                Some(runner) => runner_target(runner, rest),
                None => module_target(module),
            };
            return Some(Run::Target(match directory {
                Some(_) => relocated(target),
                None => target,
            }));
        } else if UV_RUN_VALUE_OPTIONS.contains(arg) {
            index += 1;
        } else if !arg.starts_with('-') {
            let rest = args.get(index..).unwrap_or_default();
            let mut run = if is_python_word(arg) || is_python_variable(arg) {
                python_target(rest)?
            } else if let Some(runner) = module_runner(arg) {
                Run::Target(runner_target(runner, rest))
            } else if is_python_path(arg) {
                Run::Target(path_target(arg))
            } else {
                return None;
            };
            if let Run::Target(PythonTarget::Script(script)) = &mut run {
                script.directory = directory.map(str::to_owned);
            }
            return Some(match (run, directory) {
                (Run::Target(target), Some(_)) => Run::Target(relocated(target)),
                (run, _) => run,
            });
        }
    }
    None
}

/// `pipx run` options whose value is the next word.
const PIPX_RUN_VALUE_OPTIONS: [&str; 4] = ["--spec", "--python", "--pip-args", "--index-url"];

/// `pipx run [options] <path>.py` runs a local script (pipx ≥ 1.3, #956).
/// Any other first word, or any word after `--spec`, is an app pipx installs
/// in its own venv, and a URL is no project file, so neither runs anything
/// this reads.
fn pipx_run_target(args: &[&str]) -> Option<Run> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if *arg == "--spec" || arg.starts_with("--spec=") {
            return None;
        }
        if PIPX_RUN_VALUE_OPTIONS.contains(arg) {
            index += 1;
        } else if !arg.starts_with('-') {
            let script = !arg.contains("://") && !is_computed(arg) && is_python_path(arg);
            return script.then(|| Run::Target(path_target(arg)));
        }
    }
    None
}

/// Modules `python -c` code imports. Code that imports by a computed name
/// (`__import__`, `importlib`, `runpy`, `exec`) is opaque, and so is the
/// command when the code is a variable.
fn code_targets(command: &str) -> Vec<PythonTarget> {
    let command = join_continuations(command);
    let Some(code) = python_code(&command) else {
        return vec![PythonTarget::Opaque("-c".to_owned())];
    };
    if code.contains("__import__")
        || code.contains("importlib")
        || code.contains("runpy")
        || code.contains("exec(")
        || code.trim_start_matches([' ', '"', '\'']).starts_with('$')
    {
        return vec![PythonTarget::Opaque("-c".to_owned())];
    }
    let tokens: Vec<&str> = code
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == ','))
        .flat_map(|word| word.split_inclusive(','))
        .flat_map(|word| {
            word.strip_suffix(',')
                .map_or([word, ""], |name| [name, ","])
        })
        .filter(|token| !token.is_empty())
        .collect();
    let mut modules: Vec<PythonTarget> = Vec::new();
    let mut from: Option<&str> = None;
    let mut index = 0;
    while let Some(&token) = tokens.get(index) {
        index += 1;
        if token == "from" {
            from = tokens.get(index).copied();
            if let Some(module) = from {
                push_module(&mut modules, module);
            }
        } else if token == "import" {
            // `import a as x, b`: every name in the list is a module. A name
            // after `from a import` may be the submodule `a.name`.
            let from = from.take();
            while let Some(name) = tokens.get(index) {
                match from {
                    Some(from) => push_module(&mut modules, &format!("{from}.{name}")),
                    None => push_module(&mut modules, name),
                }
                index += 1;
                if tokens.get(index) == Some(&"as") {
                    index += 2;
                }
                if tokens.get(index) != Some(&",") {
                    break;
                }
                index += 1;
            }
        }
    }
    modules
}

/// The code after python's own `-c`: the first `-c` word in a segment that
/// runs python, not one of `pre-commit` or `pip install -c constraints.txt`.
fn python_code(command: &str) -> Option<&str> {
    command.match_indices("-c").find_map(|(at, _)| {
        let before = command.get(..at)?;
        let segment = before
            .rsplit(['\n', ';', '&', '|'])
            .next()
            .unwrap_or(before);
        let runs_python = segment
            .split_whitespace()
            .any(|word| is_python_word(word) || is_python_variable(word));
        (runs_python && before.ends_with([' ', '\t'])).then(|| command.get(at + 2..))?
    })
}

fn push_module(modules: &mut Vec<PythonTarget>, module: &str) {
    if is_dotted_identifier(module)
        && !modules
            .iter()
            .any(|seen| matches!(seen, PythonTarget::Module(name) if name == module))
    {
        modules.push(PythonTarget::Module(module.to_owned()));
    }
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

pub(super) fn is_dotted_identifier(module: &str) -> bool {
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
        assert_eq!(scripts("python -m gen.py"), []);
        assert_eq!(scripts("docker run gen.py"), []);
        assert_eq!(scripts("uv run pytest tests/test_a.py"), []);
        assert_eq!(scripts("uv run --with gen.py ruff check"), []);
        assert_eq!(scripts("echo python gen.py"), []);
        // A variable interpreter after `uv run` is python too (#861).
        assert_eq!(scripts("uv run $(PYTHON) gen.py"), [script("gen.py")]);
    }

    /// pipx ≥ 1.3 runs a local `.py` file; any other first word is an app
    /// pipx installs in its own venv (#956).
    #[test]
    fn command_scripts_read_pipx_run_paths() {
        let script = |path: &str| (None, path.to_owned());
        assert_eq!(
            scripts("pipx run scripts/gen.py"),
            [script("scripts/gen.py")]
        );
        assert_eq!(
            scripts("pipx run --python 3.12 --no-cache tools/a.py --check"),
            [script("tools/a.py")]
        );
        assert_eq!(
            scripts("cd docs && pipx run build.py"),
            [(Some("docs".to_owned()), "build.py".to_owned())]
        );
        assert_eq!(scripts("pipx run ruff check gen.py"), []);
        assert_eq!(scripts("pipx run --spec black==24.1 black gen.py"), []);
        assert_eq!(scripts("pipx run https://example.com/gen.py"), []);
        assert_eq!(scripts("pipx install gen.py"), []);
        assert_eq!(scripts("pipx run $SCRIPT"), []);
        assert_eq!(binaries("pipx run ruff check"), Vec::<String>::new());
    }

    fn targets(command: &str) -> Vec<String> {
        command_targets(command)
            .into_iter()
            .map(|target| match target {
                PythonTarget::Script(run) => match run.directory {
                    Some(dir) => format!("script {dir}/{}", run.path),
                    None => format!("script {}", run.path),
                },
                PythonTarget::Run(module) => format!("run {module}"),
                PythonTarget::Module(module) => format!("module {module}"),
                PythonTarget::App { module, symbol } => match symbol {
                    Some(symbol) => format!("app {module}:{symbol}"),
                    None => format!("app {module}"),
                },
                PythonTarget::Opaque(text) => format!("opaque {text}"),
            })
            .collect()
    }

    #[test]
    fn command_targets_read_server_apps() {
        for (command, app) in [
            (
                "granian --interface asgi mypkg.asgi:application",
                "app mypkg.asgi:application",
            ),
            (
                "daphne -b 0.0.0.0 -p 8000 mypkg.asgi:application",
                "app mypkg.asgi:application",
            ),
            (
                "waitress-serve --port=8080 mypkg.wsgi:app",
                "app mypkg.wsgi:app",
            ),
            ("uvicorn mypkg.app:api.app", "app mypkg.app:api.app"),
            // A module alone gets the server's default attribute, where it
            // has one.
            ("hypercorn mypkg.asgi", "app mypkg.asgi:app"),
            ("granian mypkg.asgi", "app mypkg.asgi:app"),
            ("uvicorn mypkg.main", "app mypkg.main"),
            ("waitress-serve mypkg.wsgi", "app mypkg.wsgi"),
            // An attribute that is not a name path still names the module.
            ("uvicorn 'mypkg.app:apps[0]'", "app mypkg.app"),
            ("uvicorn 'mypkg.app:$APP'", "opaque mypkg.app:$APP"),
            // An option's `scheme:value` is not the app.
            (
                "gunicorn -c python:mypkg.conf mypkg.wsgi:app",
                "app mypkg.wsgi:app",
            ),
            (
                "gunicorn -k egg:meinheld#gevent_worker mypkg.wsgi:app",
                "app mypkg.wsgi:app",
            ),
            (
                "gunicorn -c python:mypkg.conf mypkg.wsgi",
                "app mypkg.wsgi:application",
            ),
            (
                "gunicorn --workers=$WEB_CONCURRENCY mypkg.wsgi",
                "app mypkg.wsgi:application",
            ),
            // A flag takes no value, and neither does a short option holding
            // its own.
            (
                "gunicorn --config python:mypkg.conf --preload mypkg.wsgi:app",
                "app mypkg.wsgi:app",
            ),
            ("uvicorn --no-access-log mypkg.asgi", "app mypkg.asgi"),
            ("gunicorn -w4 mypkg.wsgi", "app mypkg.wsgi:application"),
            ("hypercorn --reload mypkg.asgi", "app mypkg.asgi:app"),
            (
                "gunicorn -b127.0.0.1:8000 -D mypkg.wsgi",
                "app mypkg.wsgi:application",
            ),
            // An app imported from another directory names no known file.
            ("cd src && uvicorn mypkg.asgi:app", "opaque mypkg.asgi"),
            ("uvicorn --app-dir src mypkg.asgi:app", "opaque mypkg.asgi"),
            ("gunicorn --chdir=/srv mypkg.wsgi", "opaque mypkg.wsgi"),
            (
                "gunicorn --pythonpath lib mypkg.wsgi:app",
                "opaque mypkg.wsgi",
            ),
            (
                "granian --working-dir web --interface asgi mypkg.asgi:app",
                "opaque mypkg.asgi",
            ),
            (
                "uv run --directory web uvicorn mypkg.asgi:app",
                "opaque mypkg.asgi",
            ),
            ("PYTHONPATH=src uvicorn mypkg.asgi:app", "opaque mypkg.asgi"),
            (
                "env PYTHONPATH=src gunicorn mypkg.wsgi",
                "opaque mypkg.wsgi",
            ),
        ] {
            assert_eq!(targets(command), [app], "{command}");
        }
    }

    /// `uv run -m gunicorn` serves the app as `python -m gunicorn` does.
    #[test]
    fn uv_run_module_runners_read_their_app() {
        assert_eq!(
            targets("uv run -m gunicorn mypkg.server:app"),
            ["app mypkg.server:app"]
        );
        assert_eq!(
            targets("uv run --directory web -m gunicorn mypkg.server:app"),
            ["opaque mypkg.server"]
        );
    }

    #[test]
    fn command_targets_read_modules_runners_and_code() {
        assert_eq!(targets("python -m mypkg.modrun --x"), ["run mypkg.modrun"]);
        assert_eq!(targets("python3 -mmypkg"), ["run mypkg"]);
        // `-c` after a backslash continuation is still python's (#861).
        assert_eq!(targets("python \\\n  -c 'import mypkg'"), ["module mypkg"]);
        assert_eq!(targets("uv run -m mypkg.cli"), ["run mypkg.cli"]);
        // A module run from another import root names no file there (#912).
        for command in [
            "cd tools && python -m gen",
            "PYTHONPATH=src python -m gen",
            "uv run --directory tools -m gen",
            "uv run --directory tools python -m gen",
        ] {
            assert_eq!(targets(command), ["module gen"], "{command}");
        }
        assert_eq!(
            targets("poetry run gunicorn -w 4 -b 0.0.0.0:80 mypkg.server:app"),
            ["app mypkg.server:app"]
        );
        assert_eq!(
            targets("uvicorn --factory mypkg.api:create_app() --reload"),
            ["app mypkg.api:create_app"]
        );
        assert_eq!(
            targets("gunicorn mypkg.wsgi"),
            ["app mypkg.wsgi:application"]
        );
        assert_eq!(
            targets("python -m celery -A mypkg.tasks worker"),
            ["module mypkg.tasks"]
        );
        assert_eq!(
            targets("celery --app=mypkg.tasks beat"),
            ["module mypkg.tasks"]
        );
        assert_eq!(
            targets("python -c \"import mypkg.tasks; from mypkg import jobs; mypkg.tasks.run()\""),
            ["module mypkg.tasks", "module mypkg", "module mypkg.jobs"]
        );
        assert_eq!(
            targets("python -c 'import mypkg.a as a, mypkg.b; from mypkg.c import d, e'"),
            [
                "module mypkg.a",
                "module mypkg.b",
                "module mypkg.c",
                "module mypkg.c.d",
                "module mypkg.c.e"
            ]
        );
        assert_eq!(
            targets("pip install -c c.txt x && python -c 'import mypkg.a'"),
            ["module mypkg.a"]
        );
        assert_eq!(
            targets("gunicorn -b localhost:8000 mypkg.wsgi"),
            ["app mypkg.wsgi:application"]
        );
        assert_eq!(
            targets("gunicorn --bind localhost:8000 mypkg.wsgi:app"),
            ["app mypkg.wsgi:app"]
        );
        assert_eq!(
            targets("python -m gunicorn mypkg.wsgi"),
            ["app mypkg.wsgi:application"]
        );
        assert_eq!(
            targets("python -c 'import x, mypkg.a as a, mypkg.b'"),
            ["module x", "module mypkg.a", "module mypkg.b"]
        );
        assert_eq!(
            targets("python -c 'import mypkg.a; import mypkg.a'"),
            ["module mypkg.a"]
        );
        // `-c` inside a word is not python's flag.
        assert_eq!(
            targets("python tools/pre-commit.py --from HEAD && python -c 'import mypkg.a'"),
            ["script tools/pre-commit.py", "module mypkg.a"]
        );
        assert_eq!(targets("python -c 'print(1)'"), Vec::<String>::new());
    }

    #[test]
    fn command_targets_mark_computed_and_unread_shapes_opaque() {
        assert_eq!(targets("python $(SCRIPT)"), ["opaque $(SCRIPT)"]);
        assert_eq!(targets("$(PYTHON) -m $(MODULE)"), ["opaque $(MODULE)"]);
        assert_eq!(targets("{{python}} tools/gen.py"), ["script tools/gen.py"]);
        assert_eq!(targets("gunicorn \"$APP_MODULE\""), ["opaque $APP_MODULE"]);
        assert_eq!(targets("gunicorn -c gunicorn.conf.py"), ["opaque gunicorn"]);
        assert_eq!(targets("python mydir"), ["opaque mydir"]);
        assert_eq!(targets("python - < gen.txt"), ["opaque -"]);
        assert_eq!(
            targets("python -c \"__import__(sys.argv[1])\""),
            ["opaque -c"]
        );
        assert_eq!(
            targets("python -c \"import runpy; runpy.run_module(name)\""),
            ["opaque -c"]
        );
        assert_eq!(
            targets("python -c \"exec(open('x').read())\""),
            ["opaque -c"]
        );
        assert_eq!(
            targets("pre-commit run && python -c \"$CODE\""),
            ["opaque -c"]
        );
        // Nothing runs Python here.
        assert_eq!(targets("python"), Vec::<String>::new());
        assert_eq!(targets("python3 --version"), Vec::<String>::new());
        assert_eq!(targets("$(CMD) gen.py"), Vec::<String>::new());
        assert_eq!(targets("uv run pytest tests"), Vec::<String>::new());
        assert_eq!(targets("echo python gen.py"), Vec::<String>::new());
    }

    #[test]
    fn command_target_refs_place_script_paths() {
        let origin = ReferenceOrigin {
            file: "Dockerfile".to_owned(),
            line: Some(3),
            label: "CMD".to_owned(),
        };
        let refs = |command: &str| -> Vec<CommandTarget> {
            command_target_refs(command, &origin)
                .into_iter()
                .map(|reference| reference.target)
                .collect()
        };
        let script = |paths: &[&str]| CommandTarget::Script {
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        };
        assert_eq!(
            refs("cd tools && python ./gen.py"),
            [script(&["tools/gen.py"])]
        );
        assert_eq!(
            refs("python /app/mypkg/gen.py"),
            [script(&["app/mypkg/gen.py", "mypkg/gen.py", "gen.py"])]
        );
        assert_eq!(refs("python ../other/gen.py"), []);
        assert_eq!(
            refs("cd $(DIR) && python gen.py"),
            [CommandTarget::Opaque {
                text: "gen.py".to_owned()
            }]
        );
    }

    /// The shell grammar spec §8 scopes command reading to (#861): segments
    /// split on newlines and `;` `|` `&`, words on any run of whitespace,
    /// prefixes skipped, and `cd` carried to the segments after it.
    mod props {
        use super::*;
        use proptest::prelude::*;

        /// One generated segment and what it should run.
        #[derive(Debug, Clone)]
        enum Segment {
            /// Runs `path` (relative to `directory`, if any).
            Script {
                words: Vec<String>,
                directory: Option<String>,
                path: String,
            },
            Cd(String),
            /// Runs no script by path.
            Other(&'static str),
            /// A whole-line `#` comment; the line ends after it.
            Comment,
        }

        fn component() -> impl Strategy<Value = String> {
            prop::sample::select(vec!["scripts", "tools", "a_b", "x-y", "dev"])
                .prop_map(str::to_owned)
        }

        fn directory() -> impl Strategy<Value = String> {
            prop::collection::vec(component(), 1..3).prop_map(|parts| parts.join("/"))
        }

        fn script_path() -> impl Strategy<Value = String> {
            (prop::collection::vec(component(), 0..3), "[a-z_]{1,6}").prop_map(
                |(mut parts, stem)| {
                    parts.push(format!("{stem}.py"));
                    parts.join("/")
                },
            )
        }

        fn words(text: &str) -> Vec<String> {
            text.split(' ')
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect()
        }

        /// An interpreter word with its flags.
        fn python() -> impl Strategy<Value = String> {
            (
                prop::sample::select(vec![
                    "python",
                    "python3",
                    "python3.12",
                    ".venv/bin/python",
                    "C:\\venv\\python.exe",
                    "py",
                    "$(PYTHON)",
                    "${PYTHON3}",
                    "{{python}}",
                ]),
                prop::collection::vec(
                    prop::sample::select(vec!["-u", "-B", "-O", "-X dev", "-W ignore"]),
                    0..3,
                ),
            )
                .prop_map(|(python, flags)| format!("{python} {}", flags.join(" ")))
        }

        /// What runs the path, and the `uv run --directory` it names.
        fn launcher() -> impl Strategy<Value = (String, Option<String>)> {
            let uv_options = prop::collection::vec(
                prop::sample::select(vec![
                    "--frozen",
                    "--no-sync",
                    "-w rich",
                    "--group dev",
                    "-p 3.12",
                    "-s",
                ]),
                0..3,
            );
            prop_oneof![
                python().prop_map(|python| (python, None)),
                (
                    prop::sample::select(vec!["poetry", "pdm", "hatch", "rye", "pipenv"]),
                    python(),
                )
                    .prop_map(|(runner, python)| (format!("{runner} run {python}"), None)),
                (
                    uv_options,
                    proptest::option::of((directory(), any::<bool>())),
                    proptest::option::of(python()),
                )
                    .prop_map(|(options, directory, python)| {
                        let at = match &directory {
                            Some((dir, true)) => format!("--directory={dir}"),
                            Some((dir, false)) => format!("--directory {dir}"),
                            None => String::new(),
                        };
                        (
                            format!(
                                "uv run {} {at} {}",
                                options.join(" "),
                                python.unwrap_or_default()
                            ),
                            directory.map(|(dir, _)| dir),
                        )
                    }),
            ]
        }

        /// `[env…] [wrapper…] [sh -c "] launcher path ["]`.
        fn script_segment() -> impl Strategy<Value = Segment> {
            let env = prop::collection::vec(
                prop::sample::select(vec!["FOO=1", "PYTHONPATH=src", "_X=a=b"]),
                0..3,
            );
            let wrappers = prop::collection::vec(
                prop::sample::select(vec![
                    "exec", "env", "time", "nohup", "command", "sudo -E", "env -i", "!",
                ]),
                0..3,
            );
            (
                env,
                wrappers,
                launcher(),
                script_path(),
                prop::sample::select(vec!["", "\"", "'"]),
                any::<bool>(),
                prop::sample::select(vec!["", "--check", "a b"]),
            )
                .prop_map(
                    |(env, wrappers, (launcher, directory), path, quote, shell, args)| {
                        let run = format!("{launcher} {quote}{path}{quote} {args}");
                        let text = if shell {
                            format!("{} {} sh -c \"{run}\"", env.join(" "), wrappers.join(" "))
                        } else {
                            format!("{} {} {run}", env.join(" "), wrappers.join(" "))
                        };
                        Segment::Script {
                            words: words(&text),
                            directory,
                            path,
                        }
                    },
                )
        }

        fn segment() -> impl Strategy<Value = Segment> {
            prop_oneof![
                4 => script_segment(),
                1 => directory().prop_map(Segment::Cd),
                2 => prop::sample::select(vec![
                    "echo python x.py",
                    "python -m pkg.tool x.py",
                    "python3 -mpkg",
                    "uv run pytest tests/test_x.py",
                    "uv run -m pkg.cli",
                    "pytest tests/test_x.py",
                    "python",
                    "python -u",
                    "python -",
                    "python -c 'import pkg'",
                    "make test",
                    "cd",
                    "$(COVERAGE) run x.py",
                    "docker run x.py",
                ])
                .prop_map(Segment::Other),
                1 => Just(Segment::Comment),
            ]
        }

        /// Segments with the separator after each one.
        fn command() -> impl Strategy<Value = Vec<(Segment, &'static str)>> {
            prop::collection::vec(
                (
                    segment(),
                    prop::sample::select(vec!["&&", ";", "|", "||", "&", "\n"]),
                ),
                1..6,
            )
        }

        fn render(segments: &[(Segment, &'static str)]) -> String {
            let mut text = String::new();
            for (segment, separator) in segments {
                let words = match segment {
                    Segment::Script { words, .. } => words.join(" "),
                    Segment::Cd(dir) => format!("cd {dir}"),
                    Segment::Other(other) => (*other).to_owned(),
                    Segment::Comment => "# python x.py".to_owned(),
                };
                let separator = if matches!(segment, Segment::Comment) {
                    "\n"
                } else {
                    separator
                };
                text.push_str(&words);
                text.push(' ');
                text.push_str(separator);
                text.push(' ');
            }
            text
        }

        fn reference(segments: &[(Segment, &'static str)]) -> Vec<(Option<String>, String)> {
            let mut cwd: Option<String> = None;
            let mut runs = Vec::new();
            for (segment, _) in segments {
                match segment {
                    Segment::Script {
                        directory, path, ..
                    } => {
                        let directory = match (&cwd, directory) {
                            (Some(cwd), Some(dir)) => Some(format!("{cwd}/{dir}")),
                            (Some(cwd), None) => Some(cwd.clone()),
                            (None, dir) => dir.clone(),
                        };
                        runs.push((directory, path.clone()));
                    },
                    Segment::Cd(dir) => {
                        cwd = Some(cwd.map_or_else(|| dir.clone(), |cwd| format!("{cwd}/{dir}")));
                    },
                    Segment::Other(_) | Segment::Comment => {},
                }
            }
            runs
        }

        /// Every space becomes a run of spaces, tabs or backslash
        /// continuations, and lines get leading and trailing blanks.
        fn reflow(command: &str, noise: &[u8]) -> String {
            let mut noise = noise.iter().cycle();
            let mut gap = |out: &mut String| {
                for _ in 0..=(noise.next().copied().unwrap_or(0) % 3) {
                    out.push_str(match noise.next().copied().unwrap_or(0) % 4 {
                        0 | 1 => " ",
                        2 => "\t",
                        _ => " \\\n ",
                    });
                }
            };
            let mut out = String::new();
            for ch in command.chars() {
                match ch {
                    ' ' => gap(&mut out),
                    '\n' => {
                        out.push_str(" \t");
                        out.push('\n');
                        out.push('\t');
                    },
                    ch => out.push(ch),
                }
            }
            out
        }

        proptest! {
            /// Scripts come out in segment order, each relative to the `cd`s
            /// before it and its own `uv run --directory`, whatever prefixes,
            /// quotes and separators surround them; nothing else is one.
            #[test]
            fn command_scripts_match_the_generated_runs(segments in command()) {
                prop_assert_eq!(scripts(&render(&segments)), reference(&segments));
            }

            /// Spaces, tabs, backslash continuations and blank line ends
            /// between words change no target and no binary.
            #[test]
            fn command_reading_ignores_whitespace(
                segments in command(),
                noise in prop::collection::vec(any::<u8>(), 1..32),
            ) {
                let command = render(&segments);
                let reflowed = reflow(&command, &noise);
                prop_assert_eq!(targets(&reflowed), targets(&command));
                prop_assert_eq!(binaries(&reflowed), binaries(&command));
            }

            /// Any word soup, cut options and unbalanced quotes included,
            /// reads without a panic.
            #[test]
            fn command_reading_never_panics(
                words in prop::collection::vec(
                    prop_oneof![
                        prop::sample::select(vec![
                            "uv", "run", "--directory", "--directory=", "-m", "-c", "-X", "-",
                            "python", "$(PYTHON)", "{{python}}", "cd", "..", "/", "\\", "x.py",
                            "/abs/x.py", "../x.py", "celery", "-A", "--app=", "gunicorn", ":",
                            "a:b", "sh", "bash", ";", "&&", "|", "\\\n", "\"", "'", "`", "(",
                            ")", "import", "from", "as", ",", "__import__", "FOO=1", "é",
                        ]).prop_map(str::to_owned),
                        ".{0,6}",
                    ],
                    0..16,
                ),
                glue in prop::sample::select(vec!["", " ", "\t", "\n"]),
            ) {
                let command = words.join(glue);
                let origin = ReferenceOrigin {
                    file: "Makefile".to_owned(),
                    line: Some(1),
                    label: "make".to_owned(),
                };
                let _ = command_binaries(&command, &known);
                let _ = command_target_refs(&command, &origin);
            }
        }
    }
}
