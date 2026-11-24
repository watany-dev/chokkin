//! Static shell-command reading shared by the task-file and tool-config scanners.
//!
//! Only the command word of each `;` / `|` / `&` / newline separated segment is
//! considered (separators inside quotes and `case` patterns split nothing), after skipping env assignments, wrappers (`env`, `uv run`, ...)
//! and `python -m`. Arguments and variables are never treated as commands, so
//! `echo pytest` or `$(COVERAGE) run` stay silent; the body of a `$(...)`
//! substitution is a script of its own, read as more segments.

use crate::parser::ParseSummary;
use crate::sources::{DiscoveredFile, FileContext};

use super::ci_installs::installs;
use super::config_scan::is_distribution_name;
use super::plugin_map::plugin_module_distribution;
use super::types::{
    BinaryUsage, CommandTarget, CommandTargetRef, ModuleReference, ReferenceOrigin,
    RequirementsRef, SymbolReference, ToolConfigRef,
};

/// Predicate for binary names worth reporting (the binary map plus known tools).
pub(super) type KnownBinary<'a> = &'a dyn Fn(&str) -> bool;

/// Environment managers that install rather than use dependencies; reporting
/// `pip install` in a Dockerfile as an undeclared `pip` would only be noise.
const ENVIRONMENT_TOOLS: [&str; 11] = [
    "pip", "pip3", "uv", "uvx", "pipx", "poetry", "pdm", "hatch", "rye", "pipenv", "conda",
];
/// Keywords after which the next word is the command (`if pytest; then`, #1359).
const KEYWORDS: [&str; 9] = [
    "if", "elif", "while", "until", "then", "do", "else", "!", "{",
];

/// A command that runs the command after its options and operands.
struct Wrapper {
    name: &'static str,
    /// Short options whose value is the next word unless attached (`-n10`).
    short_values: &'static str,
    /// Options whose value is the next word unless attached with `=`.
    long_values: &'static [&'static str],
    /// Operands before the command (`timeout DURATION`, `flock FILE`).
    operands: usize,
}

const fn wrapper(
    name: &'static str,
    short_values: &'static str,
    long_values: &'static [&'static str],
    operands: usize,
) -> Wrapper {
    Wrapper {
        name,
        short_values,
        long_values,
        operands,
    }
}

/// Wrappers as their man pages define them (#1386, #1387, #1388, #1390).
/// `eval` and `trap` run a string: a quoted one is split as `sh -c` is, and
/// the words after a `trap` action are its signals.
const WRAPPERS: [Wrapper; 18] = [
    wrapper("exec", "a", &[], 0),
    wrapper("env", "uCP", &["--unset", "--chdir"], 0),
    wrapper("time", "fo", &["--format", "--output"], 0),
    wrapper("nohup", "", &[], 0),
    wrapper("command", "", &[], 0),
    wrapper(
        "sudo",
        "ugCDhprtTU",
        &[
            "--user",
            "--group",
            "--chdir",
            "--close-from",
            "--host",
            "--prompt",
            "--role",
            "--type",
            "--other-user",
            "--command-timeout",
        ],
        0,
    ),
    wrapper("doas", "uC", &[], 0),
    wrapper("nice", "n", &["--adjustment"], 0),
    wrapper("stdbuf", "ioe", &["--input", "--output", "--error"], 0),
    wrapper("setsid", "", &[], 0),
    wrapper("ionice", "cnpPu", &["--class", "--classdata"], 0),
    wrapper("xargs", "nILPsdEa", &[], 0),
    wrapper("timeout", "sk", &["--signal", "--kill-after"], 1),
    wrapper("taskset", "", &[], 1),
    wrapper(
        "flock",
        "wE",
        &["--timeout", "--wait", "--conflict-exit-code"],
        1,
    ),
    wrapper("watch", "nq", &["--interval", "--equexit"], 0),
    wrapper("eval", "", &[], 0),
    wrapper("trap", "", &[], 0),
];
const SHELLS: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];
const RUNNERS: [&str; 6] = ["uv", "poetry", "pdm", "hatch", "rye", "pipenv"];
/// `conda run [-n NAME | -p PREFIX] COMMAND` and its drop-in clones (#1394).
const CONDA_RUNNERS: [&str; 3] = ["conda", "mamba", "micromamba"];
const CONDA_RUN_VALUE_OPTIONS: [&str; 5] = ["-n", "--name", "-p", "--prefix", "--cwd"];
/// `find` primaries whose words up to `;` / `+` are a command (#1389).
const FIND_EXEC: [&str; 4] = ["-exec", "-execdir", "-ok", "-okdir"];

/// Hits collected by one scanner, merged into `ConfigScanResult` by the caller.
#[derive(Debug, Default)]
pub(super) struct SourceHits {
    pub(super) binaries: Vec<BinaryUsage>,
    pub(super) distributions: Vec<String>,
    pub(super) module_refs: Vec<ModuleReference>,
    pub(super) command_targets: Vec<CommandTargetRef>,
    /// Distributions the commands `pip install`, which provide their
    /// binaries the way a manifest declaration does (#1241).
    pub(super) installed: Vec<String>,
    /// Requirements files those commands install from (#1273).
    pub(super) requirement_files: Vec<RequirementsRef>,
    /// Config files a tool is pointed at (`mypy --config-file x.ini`, #1273).
    pub(super) tool_configs: Vec<ToolConfigRef>,
    /// Classes command options name by path; their modules are in
    /// `module_refs` (#1239).
    pub(super) used_symbols: Vec<SymbolReference>,
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
                inferred: false,
            });
        }
        self.command_targets
            .extend(command_target_refs(command, origin));
        let uses = command_uses(command);
        self.distributions.extend(uses.distributions);
        for module in uses.modules {
            self.push_module(&module, origin);
        }
        for (module, symbol) in uses.symbols {
            self.push_module(&module, origin);
            self.used_symbols.push(SymbolReference {
                module,
                symbol,
                origin: origin.clone(),
            });
        }
        for (tool, path) in uses.tool_configs {
            self.tool_configs.push(ToolConfigRef {
                tool: tool.to_owned(),
                path,
                origin: origin.clone(),
            });
        }
        let installs = installs(command);
        self.installed.extend(installs.distributions);
        self.requirement_files
            .extend(
                installs
                    .requirement_files
                    .into_iter()
                    .map(|path| RequirementsRef {
                        path,
                        origin: origin.clone(),
                        missing_ok: false,
                    }),
            );
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

/// What a command's arguments make used beyond its command word: the
/// distributions a worker or pool name loads, and the modules and classes
/// an option names (#1240, #1239).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CommandUses {
    pub(super) distributions: Vec<String>,
    pub(super) modules: Vec<String>,
    /// `(module, name)` of a class named by path; the tool imports the
    /// module and looks the name up in it.
    pub(super) symbols: Vec<(String, String)>,
    /// `(tool, path)` of a config file an option points the tool at.
    pub(super) tool_configs: Vec<(&'static str, String)>,
}

/// Celery subcommands a plugin distribution registers under
/// `celery.commands`.
const CELERY_PLUGIN_COMMANDS: [(&str, &str); 1] = [("flower", "flower")];

/// Pool (`celery worker -P`) and worker class (`gunicorn -k`) names that load
/// a distribution. A built-in name (`prefork`, `sync`, `gthread`) loads none,
/// and a name in neither list nor dotted is unknown and adds nothing.
const CONCURRENCY_DISTRIBUTIONS: [(&str, &str); 3] = [
    ("gevent", "gevent"),
    ("eventlet", "eventlet"),
    ("tornado", "tornado"),
];

/// Celery's global options whose value is the next word, so it is not the
/// subcommand.
const CELERY_VALUE_OPTIONS: [&str; 8] = [
    "-A",
    "--app",
    "-b",
    "--broker",
    "--result-backend",
    "--loader",
    "--config",
    "--workdir",
];

/// [`CommandUses`] of each segment of `command`. Only the tool at the
/// command word (or after `python -m`) is read, so `echo celery flower`
/// names nothing; a computed value (`-k $WORKER`) is skipped, as the class
/// it loads is unknown and the command word already counts as a usage.
pub(super) fn command_uses(command: &str) -> CommandUses {
    let mut uses = CommandUses::default();
    for_each_segment(command, |words| {
        let Some((tool, args)) = segment_tool(words) else {
            return;
        };
        match tool {
            "celery" => {
                let subcommand = celery_subcommand(args);
                if let Some(distribution) = subcommand.and_then(|name| {
                    CELERY_PLUGIN_COMMANDS
                        .into_iter()
                        .find_map(|(command, dist)| (command == name).then_some(dist))
                }) {
                    uses.distributions.push(distribution.to_owned());
                }
                if let Some(pool) = option_value(args, Some("-P"), "--pool") {
                    uses.push_class(pool);
                }
                // `-S` is `--scheduler` for `beat` only; the worker's is `--statedb`.
                let short = (subcommand == Some("beat")).then_some("-S");
                if let Some(scheduler) = option_value(args, short, "--scheduler") {
                    uses.symbols.extend(symbol_path(scheduler));
                }
            },
            "gunicorn" => {
                if let Some(worker) = option_value(args, Some("-k"), "--worker-class") {
                    uses.push_class(worker);
                }
            },
            "mypy" => {
                if let Some(path) = option_value(args, None, "--config-file") {
                    uses.tool_configs.push(("mypy", path.to_owned()));
                }
            },
            "pytest" | "py.test" => {
                if let Some(path) = option_value(args, Some("-c"), "--config-file") {
                    uses.tool_configs.push(("pytest", path.to_owned()));
                }
                // pytest-django's settings module.
                if let Some(settings) = option_value(args, None, "--ds")
                    && !is_computed(settings)
                    && is_dotted_identifier(settings)
                {
                    uses.modules.push(settings.to_owned());
                }
            },
            _ => {},
        }
    });
    uses
}

impl CommandUses {
    /// A pool or worker class: a known name's distribution, the distribution
    /// of gunicorn's `egg:dist#name`, or the module of a dotted class
    /// (`uvicorn.workers.UvicornWorker`).
    fn push_class(&mut self, class: &str) {
        if let Some(spec) = class.strip_prefix("egg:") {
            let distribution = spec.split('#').next().unwrap_or(spec);
            if !is_computed(distribution) && is_distribution_name(distribution) {
                self.distributions.push(distribution.to_owned());
            }
        } else if let Some(distribution) = CONCURRENCY_DISTRIBUTIONS
            .into_iter()
            .find_map(|(name, dist)| (name == class).then_some(dist))
        {
            self.distributions.push(distribution.to_owned());
        } else if class.contains(['.', ':']) {
            self.symbols.extend(symbol_path(class));
        }
    }
}

/// The tool a segment runs and its arguments: the command word, or the
/// module `python -m` runs.
fn segment_tool<'w, 'a>(words: &'w [&'a str]) -> Option<(&'a str, &'w [&'a str])> {
    let (first, args) = words.get(command_start(words)..)?.split_first()?;
    if !is_python_word(first) {
        return Some((command_name(first), args));
    }
    match python_main(args)? {
        PythonMain::Module(module, rest) => Some((module, rest)),
        PythonMain::Code | PythonMain::Stdin | PythonMain::Script(_) => None,
    }
}

/// The first word that is neither an option nor a global option's value.
fn celery_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if CELERY_VALUE_OPTIONS.contains(arg) {
            args.next();
        } else if !arg.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

/// The value of `short` / `long` as `-k v`, `-kv`, `--long v` or `--long=v`.
fn option_value<'a>(args: &[&'a str], short: Option<&str>, long: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(index, arg)| {
        if *arg == long || Some(*arg) == short {
            return args.get(index + 1).copied();
        }
        arg.strip_prefix(long)
            .and_then(|value| value.strip_prefix('='))
            .or_else(|| short.and_then(|short| arg.strip_prefix(short)))
            .filter(|value| !value.is_empty())
    })
}

/// The module and top-level name of a `module:attr` or `module.attr` path,
/// as celery's and gunicorn's `symbol_by_name` / `load_class` split it. A
/// computed path or one that is no name path names nothing.
fn symbol_path(path: &str) -> Option<(String, String)> {
    if is_computed(path) {
        return None;
    }
    let (module, attr) = path.split_once(':').or_else(|| path.rsplit_once('.'))?;
    let name = attr.split('.').next().unwrap_or(attr);
    (is_dotted_identifier(module) && is_dotted_identifier(name))
        .then(|| (module.to_owned(), name.to_owned()))
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
fn parsed_server_apps(parse: &ParseSummary) -> impl Iterator<Item = CommandTargetRef> {
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

/// What the parsed sources run: server apps and notebook commands.
pub(super) fn parsed_command_targets<'p>(
    parse: &'p ParseSummary,
    files: &'p [DiscoveredFile],
) -> impl Iterator<Item = CommandTargetRef> + 'p {
    parsed_server_apps(parse).chain(parsed_notebook_commands(parse, files))
}

/// The scripts and modules notebook cells run with `%run` or a shell escape
/// (#992), read as a task file's commands. A cell runs in the notebook's
/// directory, so a script's path is joined to it; `-m` finds the module on
/// the project's import roots, as the kernel's installed project does. A
/// runtime notebook's targets keep their own context, as a deploy
/// command's do; any other notebook's are dev.
fn parsed_notebook_commands<'p>(
    parse: &'p ParseSummary,
    files: &'p [DiscoveredFile],
) -> impl Iterator<Item = CommandTargetRef> + 'p {
    let notebooks = parse
        .modules
        .iter()
        .filter(|parsed| !parsed.notebook_commands.is_empty());
    notebooks.flat_map(move |parsed| {
        let deploy = files
            .iter()
            .any(|file| file.path == parsed.path && file.context == FileContext::Runtime);
        let dir = parsed.path.rsplit_once('/').map(|(dir, _)| dir);
        parsed.notebook_commands.iter().flat_map(move |run| {
            let origin = ReferenceOrigin {
                file: parsed.path.clone(),
                line: Some(run.line),
                label: "notebook".to_owned(),
            };
            let plain = command_target_refs(&run.command, &origin);
            let targets: Vec<CommandTargetRef> = match dir {
                // Only a script's path is joined: `cd` would also move the
                // import root a `-m` module is found on.
                Some(dir) => command_target_refs(&format!("cd {dir}\n{}", run.command), &origin)
                    .into_iter()
                    .filter(is_script)
                    .chain(plain.into_iter().filter(|target| !is_script(target)))
                    .collect(),
                None => plain,
            };
            targets.into_iter().map(move |mut target| {
                target.deploy = deploy;
                target
            })
        })
    })
}

fn is_script(target: &CommandTargetRef) -> bool {
    matches!(target.target, CommandTarget::Script { .. })
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
        if runs_with_extras(words.get(..start).unwrap_or_default()) {
            return;
        }
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

/// `uv run --with X tool`: uv installs the extras for this run only, so the
/// tool may come from them, as with `uvx`, and is no undeclared usage.
fn runs_with_extras(prefix: &[&str]) -> bool {
    let Some(run) = prefix
        .windows(2)
        .position(|pair| command_name(pair[0]) == "uv" && pair[1] == "run")
    else {
        return false;
    };
    prefix
        .get(run + 2..)
        .unwrap_or_default()
        .iter()
        .any(|word| {
            let option = word.split_once('=').map_or(*word, |(option, _)| option);
            matches!(
                option,
                "-w" | "--with" | "--with-editable" | "--with-requirements"
            )
        })
}

/// Calls `visit` with the words of each simple command in `command`, joining
/// backslash continuations first (see [`ShellSplitter`]).
fn for_each_segment(command: &str, mut visit: impl FnMut(&[&str])) {
    let chars: Vec<char> = join_continuations(command).chars().collect();
    let mut segments = Vec::new();
    ShellSplitter::new(0, &mut segments).split(&chars);
    for words in &mut segments {
        strip_hatch_env(words);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        visit(&words);
    }
}

/// `hatch run ENV:CMD` runs CMD in the environment ENV (#1393).
fn strip_hatch_env(words: &mut [String]) {
    let Some(run) = words
        .windows(2)
        .position(|pair| command_name(&pair[0]) == "hatch" && pair[1] == "run")
    else {
        return;
    };
    let options = words.get(run + 2..).unwrap_or_default();
    let options: Vec<&str> = options.iter().map(String::as_str).collect();
    let at = run + 2 + option_width(&options, "", &[]);
    if let Some(word) = words.get_mut(at)
        && let Some((env, command)) = word.split_once(':')
        && !env.is_empty()
        && !command.is_empty()
        && env
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        *word = command.to_owned();
    }
}

/// How deep `sh -c "..."` scripts are read inside one another.
const MAX_SHELL_NESTING: usize = 4;

/// Splits a shell string into simple commands at unquoted newlines, `;`,
/// `|`, `&`, `(` and `)` (#1341).
///
/// Quoting is read as a shell reads it: `'...'`, `"..."`, `\x` and the
/// bodies of `$(...)`, `$((...))`, `${...}` and backticks are data inside one
/// word, so `echo "<source|wheel>"` and `X="${1:?a|wheel}"` run only `echo`
/// and nothing. A substitution keeps its raw text, `$` included, so the word
/// stays computed; the body of a `$(...)` or backtick one is split as a
/// script of its own (#1381). A backslash escapes only a
/// character the shell treats as special, so `C:\venv\pytest.exe` keeps
/// its separators.
///
/// The pattern list of a `case` arm (`a|wheel)`, `(a) ;;`) is skipped where
/// a pattern may start: after `case WORD in`, after `;;` / `;&` / `;;&`, and
/// at the start of a line outside a subshell, since a script is read line by
/// line and its `case` line is not in view. An unterminated quote or
/// substitution runs to the end of the string as data, so the words after it
/// name no command. `sh -c` / `bash -c` scripts are split in turn.
struct ShellSplitter<'o> {
    depth: usize,
    out: &'o mut Vec<Vec<String>>,
    words: Vec<String>,
    word: Option<String>,
    pattern_next: bool,
    subshells: usize,
    /// The last character read was an unquoted `>` or `<`.
    after_redirect: bool,
}

impl<'o> ShellSplitter<'o> {
    fn new(depth: usize, out: &'o mut Vec<Vec<String>>) -> Self {
        Self {
            depth,
            out,
            words: Vec::new(),
            word: None,
            pattern_next: true,
            subshells: 0,
            after_redirect: false,
        }
    }

    fn split(&mut self, chars: &[char]) {
        let mut at = 0;
        while let Some(&ch) = chars.get(at) {
            if self.pattern_next && self.word.is_none() && self.words.is_empty() {
                if matches!(ch, ' ' | '\t' | '\r') {
                    at += 1;
                    continue;
                }
                self.pattern_next = false;
                if self.subshells == 0
                    && let Some(end) = case_pattern_end(chars, at)
                {
                    debug_assert!(end > at);
                    at = end;
                    continue;
                }
            }
            let after_redirect = std::mem::take(&mut self.after_redirect);
            let next = match ch {
                ' ' | '\t' | '\r' => {
                    self.end_word();
                    at + 1
                },
                // `2>&1`, `>&2`, `&>log`: a redirection, not a separator (#1383).
                '&' if after_redirect || chars.get(at + 1) == Some(&'>') => {
                    self.push(ch);
                    at + 1
                },
                '\n' | ';' | '|' | '&' | '(' | ')' => self.separator(chars, at),
                '#' if self.word.is_none() => chars
                    .get(at..)
                    .and_then(|rest| rest.iter().position(|&ch| ch == '\n'))
                    .map_or(chars.len(), |offset| at + offset),
                '\'' => {
                    let end = closing(chars, at + 1, '\'');
                    self.push_raw(chars.get(at + 1..end).unwrap_or_default());
                    end + 1
                },
                '"' => self.double_quoted(chars, at + 1),
                '\\' => self.escape(chars, at, is_shell_special),
                '$' | '`' => self.substitution(chars, at, false),
                _ => {
                    self.after_redirect = matches!(ch, '>' | '<');
                    self.push(ch);
                    at + 1
                },
            };
            // A step that does not advance would rescan, and grow the word, forever.
            debug_assert!(next > at);
            at = next;
        }
        self.end_segment();
    }

    /// Ends the segment at the separator at `at`; returns the index after it.
    fn separator(&mut self, chars: &[char], at: usize) -> usize {
        self.end_segment();
        match chars.get(at..).unwrap_or_default() {
            ['\n', ..] => {
                self.pattern_next = true;
                at + 1
            },
            [';', ';', '&', ..] => {
                self.pattern_next = true;
                at + 3
            },
            [';', ';' | '&', ..] => {
                self.pattern_next = true;
                at + 2
            },
            // `name()` opens and closes at once: no subshell is left open.
            ['(', ..] => {
                self.subshells += 1;
                at + 1
            },
            [')', ..] => {
                self.subshells = self.subshells.saturating_sub(1);
                at + 1
            },
            _ => at + 1,
        }
    }

    /// The backslash at `at`: the next character when `escapes` it, else a
    /// literal backslash.
    fn escape(&mut self, chars: &[char], at: usize, escapes: fn(char) -> bool) -> usize {
        match chars.get(at + 1) {
            Some(&next) if escapes(next) => {
                self.push(next);
                at + 2
            },
            _ => {
                self.push('\\');
                at + 1
            },
        }
    }

    /// The text of a `"..."` starting at `at`; returns the index after it.
    fn double_quoted(&mut self, chars: &[char], mut at: usize) -> usize {
        self.word.get_or_insert_with(String::new);
        while let Some(&ch) = chars.get(at) {
            let next = match ch {
                '"' => return at + 1,
                '\\' => self.escape(chars, at, |next| matches!(next, '"' | '$' | '`' | '\\')),
                '$' | '`' => self.substitution(chars, at, true),
                _ => {
                    self.push(ch);
                    at + 1
                },
            };
            // A step that does not advance would rescan, and grow the word, forever.
            debug_assert!(next > at);
            at = next;
        }
        at
    }

    /// `$(...)`, `$((...))`, `${...}` or a backtick body at `at`, kept raw in
    /// the current word; a lone `$` is itself. Inside `"..."` an apostrophe
    /// in `${...}` is a literal (`"${MSG:-it's}"`), not a quote.
    fn substitution(&mut self, chars: &[char], at: usize, quoted: bool) -> usize {
        let end = match (chars.get(at), chars.get(at + 1)) {
            (Some('`'), _) => closing(chars, at + 1, '`') + 1,
            (Some('$'), Some('(')) => balanced(chars, at, ('(', ')'), false),
            (Some('$'), Some('{')) => balanced(chars, at, ('{', '}'), quoted),
            _ => at + 1,
        };
        let end = end.min(chars.len());
        self.push_raw(chars.get(at..end).unwrap_or_default());
        self.command_substitution(chars, at, end);
        end
    }

    /// Splits the body of a `$(...)` or backtick substitution spanning
    /// `at..end` as a script of its own, since the shell runs it (#1381). An
    /// arithmetic `$((...))` runs nothing.
    fn command_substitution(&mut self, chars: &[char], at: usize, end: usize) {
        let body = match chars.get(at..end).unwrap_or_default() {
            ['$', '(', '(', ..] | [] => return,
            ['$', '(', body @ ..] | ['`', body @ ..] => body,
            _ => return,
        };
        let body = if let [inner @ .., ')' | '`'] = body {
            inner
        } else {
            body
        };
        if self.depth >= MAX_SHELL_NESTING {
            return;
        }
        let mut segments = Vec::new();
        ShellSplitter::new(self.depth + 1, &mut segments).split(body);
        // A `cd` there moves only the substitution's subshell.
        self.out.extend(segments.into_iter().filter(|words| {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            words
                .get(command_start(&words))
                .is_none_or(|word| command_name(word) != "cd")
        }));
    }

    fn push(&mut self, ch: char) {
        self.word.get_or_insert_with(String::new).push(ch);
    }

    fn push_raw(&mut self, chars: &[char]) {
        self.word.get_or_insert_with(String::new).extend(chars);
    }

    fn end_word(&mut self) {
        let Some(word) = self.word.take() else {
            return;
        };
        if word.is_empty() {
            return;
        }
        self.words.push(word);
        // `case WORD in`, also after `then` / `do`: a pattern list follows.
        let start = self
            .words
            .iter()
            .take_while(|word| KEYWORDS.contains(&word.as_str()))
            .count();
        if let Some([case, _, .., last]) = self.words.get(start..)
            && case == "case"
            && last == "in"
        {
            self.end_segment();
            self.pattern_next = true;
        }
    }

    fn end_segment(&mut self) {
        if let Some(word) = self.word.take().filter(|word| !word.is_empty()) {
            self.words.push(word);
        }
        if self.words.is_empty() {
            return;
        }
        let mut words = std::mem::take(&mut self.words);
        // A command word holding whitespace is a quoted script: `sh -c "a; b"`,
        // or a whole command quoted once more (`run: "pytest -q"`).
        let start = command_start(&words.iter().map(String::as_str).collect::<Vec<_>>());
        let script = words
            .get(start)
            .is_some_and(|word| word.contains([' ', '\t', '\n']) && self.depth < MAX_SHELL_NESTING)
            .then(|| {
                let script = words.swap_remove(start);
                words.truncate(start);
                script
            });
        if !words.is_empty() {
            self.out.push(words);
        }
        if let Some(script) = script {
            let chars: Vec<char> = script.chars().collect();
            ShellSplitter::new(self.depth + 1, self.out).split(&chars);
        }
    }
}

/// A character a backslash escapes outside quotes.
fn is_shell_special(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t'
            | '\n'
            | '"'
            | '\''
            | '\\'
            | '|'
            | ';'
            | '&'
            | '$'
            | '`'
            | '('
            | ')'
            | '<'
            | '>'
            | '#'
    )
}

/// Index of the first `close` at or after `from`, or the end.
fn closing(chars: &[char], from: usize, close: char) -> usize {
    chars
        .get(from..)
        .and_then(|rest| rest.iter().position(|&ch| ch == close))
        .map_or(chars.len(), |offset| from + offset)
}

/// Index after the `close` matching the first `open` at or after `at`,
/// skipping quotes
/// (apostrophes are literals when `literal_apostrophe`), or the end.
fn balanced(
    chars: &[char],
    at: usize,
    (open, close): (char, char),
    literal_apostrophe: bool,
) -> usize {
    let mut depth = 0usize;
    let mut index = at;
    while let Some(&ch) = chars.get(index) {
        let next = match ch {
            '\'' if !literal_apostrophe => closing(chars, index + 1, '\'') + 1,
            '"' => {
                let mut end = index + 1;
                while let Some(&ch) = chars.get(end) {
                    let step = match ch {
                        '\\' => 2,
                        '"' => break,
                        _ => 1,
                    };
                    let before = end;
                    end += step;
                    debug_assert!(end > before);
                }
                end + 1
            },
            '\\' => index + 2,
            _ if ch == open => {
                depth += 1;
                index + 1
            },
            _ if ch == close => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
                index + 1
            },
            _ => index + 1,
        };
        debug_assert!(next > index);
        index = next;
    }
    chars.len()
}

/// Index of the `)` closing a `case` arm's pattern list starting at `at`
/// (`a|b)`, `a | "b c")`), which the caller reads as a separator, or `None`
/// when the text there is not one: a pattern is one word, the `)` follows
/// the last one directly, and a list never holds a separator, a redirection,
/// a subshell or a substitution. The optional `(` before the list is not
/// read, so `(pytest)` stays a subshell.
fn case_pattern_end(chars: &[char], at: usize) -> Option<usize> {
    let mut index = at;
    loop {
        while matches!(chars.get(index), Some(' ' | '\t')) {
            index += 1;
        }
        let start = index;
        while let Some(&ch) = chars.get(index) {
            let next = match ch {
                ' ' | '\t' | '|' | ')' => break,
                '\'' => closing(chars, index + 1, '\'') + 1,
                '"' => closing(chars, index + 1, '"') + 1,
                '\\' => index + 2,
                // `(` also refuses a `$(...)` substitution.
                '(' | ';' | '&' | '\n' | '<' | '>' | '`' => return None,
                _ => index + 1,
            };
            debug_assert!(next > index);
            index = next;
        }
        if index == start {
            return None;
        }
        if chars.get(index) == Some(&')') {
            return Some(index);
        }
        while matches!(chars.get(index), Some(' ' | '\t')) {
            index += 1;
        }
        if chars.get(index) != Some(&'|') {
            return None;
        }
        index += 1;
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
const UV_RUN_VALUE_OPTIONS: [&str; 27] = [
    "--directory",
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
        if command_name(word) == "streamlit" && rest.first() == Some(&"run") {
            return streamlit_target(rest.get(1..)?);
        }
        if is_python_word(word) || is_python_variable(word) {
            return python_target(rest);
        }
        if let Some(runner) = module_runner(word) {
            return Some(Run::Target(runner_target(runner, rest)));
        }
        let Prefix::Skip(width) = prefix(words, index) else {
            return None;
        };
        index += width;
    }
    None
}

/// `streamlit run [options] app.py [args]` runs the script as `python` does
/// (#992). Its options may take a value as the next word, so the script is
/// the first `.py` or computed word; a URL or a directory is not read.
fn streamlit_target(args: &[&str]) -> Option<Run> {
    let script = args
        .iter()
        .find(|arg| is_python_path(arg) || is_computed(arg))?;
    Some(Run::Target(path_target(script)))
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
    Some(match python_main(args)? {
        PythonMain::Module(module, rest) => Run::Target(match module_runner(module) {
            Some(runner) => runner_target(runner, rest),
            None => module_target(module),
        }),
        PythonMain::Code => Run::Code,
        PythonMain::Stdin => Run::Target(PythonTarget::Opaque("-".to_owned())),
        PythonMain::Script(arg) => Run::Target(path_target(arg)),
    })
}

/// What a Python command line runs, read as the interpreter reads its options.
enum PythonMain<'w, 'a> {
    /// `-m MOD` and the arguments after it.
    Module(&'a str, &'w [&'a str]),
    /// `-c CODE`.
    Code,
    /// `-`: the script is read from stdin.
    Stdin,
    /// A script path, or what stands in its place.
    Script(&'a str),
}

/// The interpreter's command line (getopt style): flags cluster (`-um MOD` is
/// `-u -m MOD`, #1396), `-m` / `-X` / `-W` take the rest of their cluster or
/// the next word, `--check-hash-based-pycs` takes the next word (#1397), and
/// `--` makes the next word the script. `None` when nothing runs (a bare
/// interpreter, flags alone).
fn python_main<'w, 'a>(args: &'w [&'a str]) -> Option<PythonMain<'w, 'a>> {
    let mut index = 0;
    while let Some(&arg) = args.get(index) {
        index += 1;
        if arg == "-" {
            return Some(PythonMain::Stdin);
        }
        if arg == "--" {
            return args.get(index).map(|script| PythonMain::Script(script));
        }
        if arg == "--check-hash-based-pycs" {
            index += 1;
            continue;
        }
        let Some(cluster) = arg.strip_prefix('-') else {
            return Some(PythonMain::Script(arg));
        };
        if cluster.starts_with('-') {
            continue;
        }
        for (at, flag) in cluster.char_indices() {
            let value = &cluster[at + flag.len_utf8()..];
            match flag {
                'c' => return Some(PythonMain::Code),
                'm' if value.is_empty() => {
                    let module = args.get(index)?;
                    return Some(PythonMain::Module(module, args.get(index + 1..)?));
                },
                'm' => return Some(PythonMain::Module(value, args.get(index..)?)),
                'X' | 'W' => {
                    if value.is_empty() {
                        index += 1;
                    }
                    break;
                },
                _ => {},
            }
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
    let (module, attr) = split_app(app).map_or((app, None), |(module, attr)| (module, Some(attr)));
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
/// with a colon. A word starting with `-` is an option, perhaps glued to its
/// value (`-w${WORKERS:-4}`), and names no app.
fn is_app_reference(arg: &str) -> bool {
    !arg.starts_with('-')
        && split_app(arg).is_some_and(|(module, attr)| {
            (is_dotted_identifier(module) || is_computed(module))
            && !attr.is_empty()
            && !attr.starts_with('/')
            // `-b localhost:8000` binds a port; it names no app.
            && !attr.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Splits at the first `:` outside braces: the one in `${APP:-default}`
/// separates no module from an attribute.
fn split_app(arg: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let (colon, _) = arg.char_indices().find(|&(_, char)| {
        match char {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {},
        }
        char == ':' && depth == 0
    })?;
    Some((&arg[..colon], &arg[colon + 1..]))
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
            // `uv run app.py` runs a script; anything else (`python`, a
            // server, another wrapper, #1400) is read as a command of its own.
            let mut run = if is_python_path(arg) {
                Run::Target(path_target(arg))
            } else {
                segment_target(args.get(index - 1..)?)?
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
const PIPX_RUN_VALUE_OPTIONS: [&str; 5] = ["--spec", "--python", "--pip-args", "--index-url", "-i"];

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
            let script = !arg.contains("://") && is_python_path(arg);
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

/// Index of the command word: past every [`Prefix::Skip`], or `words.len()`
/// when the segment runs no command.
fn command_start(words: &[&str]) -> usize {
    let mut index = 0;
    loop {
        match prefix(words, index) {
            Prefix::Skip(width) => index += width,
            Prefix::Command => return index,
            Prefix::Nothing => return words.len(),
        }
    }
}

/// What the word at `index` is to the command of a segment.
enum Prefix {
    /// It and the words after it precede the command: skip this many.
    Skip(usize),
    /// It is the command word.
    Command,
    /// The segment runs no command from here (`command -v black`, #1385),
    /// or has no word left.
    Nothing,
}

/// Skipped: an option, an env assignment, a keyword, a redirection
/// (`>log`, `2> err`, #1383), `function NAME` (#1382), a [`WRAPPERS`] entry
/// (by basename, so `/usr/bin/env` too, #1392) with its options and
/// operands, `find ... -exec`, a shell's options up to its `-c` script, and
/// `uv run` / `conda run -n ENV` style runners with their options.
fn prefix(words: &[&str], index: usize) -> Prefix {
    let Some(&word) = words.get(index) else {
        return Prefix::Nothing;
    };
    let rest = words.get(index + 1..).unwrap_or_default();
    if word.starts_with('-') || is_env_assignment(word) || KEYWORDS.contains(&word) {
        return Prefix::Skip(1);
    }
    if let Some(target) = redirection_target(word) {
        return Prefix::Skip(if target.is_empty() { 2 } else { 1 });
    }
    if word == "function" {
        return Prefix::Skip(2);
    }
    let name = command_name(word);
    let next = rest.first().copied();
    if name == "command"
        && next.is_some_and(|next| {
            next.starts_with('-') && !next.starts_with("--") && next.contains(['v', 'V'])
        })
    {
        return Prefix::Nothing;
    }
    if name == "find" {
        return rest
            .iter()
            .position(|word| FIND_EXEC.contains(word))
            .map_or(Prefix::Command, |at| Prefix::Skip(at + 2));
    }
    // `sh -c`, `bash -lc`, `sh -o pipefail -c`: the script is the command word.
    if SHELLS.contains(&name)
        && next.is_some_and(|next| next.starts_with('-') && !next.starts_with("--"))
    {
        return Prefix::Skip(1 + option_width(rest, "oO", &[]));
    }
    if let Some(wrapper) = WRAPPERS.iter().find(|wrapper| wrapper.name == name) {
        // `flock FILE -c 'cmd'`: the `-c` after the operand is skipped as an
        // option, and the string is split as `sh -c` scripts are.
        let options = option_width(rest, wrapper.short_values, wrapper.long_values);
        return Prefix::Skip(1 + options + wrapper.operands);
    }
    if next == Some("run") {
        let options = rest.get(1..).unwrap_or_default();
        if RUNNERS.contains(&name) {
            let values: &[&str] = if name == "uv" {
                &UV_RUN_VALUE_OPTIONS
            } else {
                &[]
            };
            return Prefix::Skip(2 + option_width(options, "", values));
        }
        if CONDA_RUNNERS.contains(&name) {
            return Prefix::Skip(2 + option_width(options, "", &CONDA_RUN_VALUE_OPTIONS));
        }
    }
    Prefix::Command
}

/// How many leading words of `args` are options, with the values of those in
/// `long_values` (matched whole, so `-p` fits too) and of a cluster ending in
/// one of `short_values`. A `--` ends them and is counted.
fn option_width(args: &[&str], short_values: &str, long_values: &[&str]) -> usize {
    let mut width = 0;
    while let Some(&arg) = args.get(width) {
        if arg == "--" {
            return width + 1;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        width += 1;
        if long_values.contains(&arg) {
            width += 1;
        } else if !arg.starts_with("--")
            && let Some(cluster) = arg.get(1..)
            && let Some(at) = cluster.find(|ch| short_values.contains(ch))
            && cluster
                .get(at..)
                .is_some_and(|tail| tail.chars().count() == 1)
        {
            width += 1;
        }
    }
    width
}

/// The target of a redirection word (`2>` → `""`, `>log` → `log`), or
/// `None` when the word is no redirection.
fn redirection_target(word: &str) -> Option<&str> {
    const OPERATORS: [&str; 12] = [
        "&>>", "<<<", "<<-", ">>", ">&", "<&", "<>", ">|", "&>", "<<", ">", "<",
    ];
    let rest = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
    OPERATORS
        .into_iter()
        .find_map(|operator| rest.strip_prefix(operator))
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
    let PythonMain::Module(module, _) = python_main(args)? else {
        return None;
    };
    known_binary(module.split('.').next().unwrap_or(module), known)
}

/// `python`, `python3.12`, `py`, and `pypy` / `pypy3.10`, which share
/// the interpreter's command line (#1399).
fn is_python_word(word: &str) -> bool {
    let name = command_name(word).to_ascii_lowercase();
    name == "py"
        || ["python", "pypy"].into_iter().any(|stem| {
            name.strip_prefix(stem)
                .is_some_and(|version| version.chars().all(|ch| ch.is_ascii_digit() || ch == '.'))
        })
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

    /// The shell runs a command substitution's body (#1381); an arithmetic
    /// expansion and a parameter expansion run nothing.
    #[test]
    fn command_substitutions_run_their_bodies() {
        for command in [
            "out=$(pytest -q)",
            "echo \"$(pytest)\"",
            "ver=`pytest --version`",
            "for x in $(pytest --co -q); do :; done",
            "echo $(cd x && $(pytest))",
        ] {
            assert_eq!(binaries(command), ["pytest"], "{command}");
        }
        for command in ["echo $((pytest + 1))", "echo ${pytest}", "echo '$(pytest)'"] {
            assert_eq!(binaries(command), Vec::<String>::new(), "{command}");
        }
        assert_eq!(targets("out=$(python -m pkg.tool)"), ["run pkg.tool"]);
        // Substitutions nest to MAX_SHELL_NESTING.
        assert_eq!(binaries("echo $($($($(pytest))))"), ["pytest"]);
        assert_eq!(binaries("echo $($($($($(pytest)))))"), Vec::<String>::new());
        // A `cd` inside a substitution moves no later script.
        assert_eq!(
            targets("python gen.py --rev $(cd vendor && git rev-parse HEAD)"),
            ["script gen.py"]
        );
    }

    /// `function NAME { ... }` defines a function like `NAME() { ... }` (#1382).
    #[test]
    fn function_keyword_bodies_run() {
        assert_eq!(binaries("function run { pytest; }"), ["pytest"]);
        assert_eq!(binaries("function run() { pytest; }"), ["pytest"]);
    }

    /// A redirection may precede the command word (#1383).
    #[test]
    fn leading_redirections_are_skipped() {
        for command in [
            ">log pytest",
            "2>err pytest",
            "</dev/null pytest",
            "> log pytest",
            "2>&1 pytest",
            "&>log pytest",
            ">>log 2>&1 pytest",
        ] {
            assert_eq!(binaries(command), ["pytest"], "{command}");
        }
        assert_eq!(binaries("pytest -q 2>&1 | tee log"), ["pytest"]);
        // `&>` stays in the segment it redirects.
        assert_eq!(segments("pytest &>log -q"), [["pytest", "&>log", "-q"]]);
        // A quoted `>` is no redirection: the `&` after it is a separator.
        assert_eq!(binaries("echo \">\"& pytest"), ["pytest"]);
    }

    /// `command -v` / `-V` only looks a name up (#1385).
    #[test]
    fn command_lookup_runs_nothing() {
        assert_eq!(
            binaries("command -v pytest >/dev/null"),
            Vec::<String>::new()
        );
        assert_eq!(binaries("command -V pytest"), Vec::<String>::new());
        assert_eq!(binaries("command -p pytest"), ["pytest"]);
    }

    /// A wrapper option's value is not the command (#1386), and every
    /// wrapper's command after its options and operands is read (#1387,
    /// #1388, #1389, #1390, #1392).
    #[test]
    fn wrappers_skip_option_values_and_operands() {
        for command in [
            "sudo -u app pytest",
            "sudo -E pytest",
            "sudo --user app pytest",
            "env -u HOME pytest",
            "env -C dir pytest",
            "env -S 'pytest -q'",
            "exec -a name pytest",
            "sh -o pipefail -c 'pytest -q'",
            "bash -euo pipefail -c 'pytest'",
            "bash -o pipefail pytest",
            "uv run -p 3.12 pytest",
            "nice -n 10 pytest",
            "nice -10 pytest",
            "stdbuf -oL pytest",
            "stdbuf -o L pytest",
            "setsid pytest",
            "ionice -c 3 pytest",
            "doas -u app pytest",
            "ls | xargs -n1 pytest",
            "ls | xargs -I {} pytest {}",
            "timeout 600 pytest",
            "timeout -s KILL 10m pytest",
            "timeout -- 10 pytest",
            "taskset -c 0 pytest",
            "flock /tmp/lock pytest",
            "flock -w 5 /tmp/lock pytest",
            "flock /tmp/lock -c 'pytest -q'",
            "find . -name '*.py' -exec pytest {} \\;",
            "find . -execdir pytest {} +",
            "eval 'pytest -q'",
            "eval pytest",
            "trap 'pytest' EXIT",
            "watch -n 5 pytest",
            "/usr/bin/env pytest",
            "/usr/bin/nohup pytest",
            "conda run -n app pytest",
            "mamba run --no-capture-output -p ./env pytest",
            "hatch run lint:pytest",
            "env A=1 B=2 hatch run -v lint:pytest",
            "poetry run flock -w 5 /lock pytest",
            "hatch run pytest",
            "uv run nohup pytest",
        ] {
            assert_eq!(binaries(command), ["pytest"], "{command}");
        }
        assert_eq!(binaries("find . -name pytest"), Vec::<String>::new());
        // The extras of `uv run --with` may provide the tool.
        assert_eq!(binaries("uv run --with rich pytest"), Vec::<String>::new());
        assert_eq!(
            binaries("env A=1 B=2 C=3 uv run --with rich pytest"),
            Vec::<String>::new()
        );
        assert_eq!(
            binaries("uv run --with=\"$DEPS\" pytest"),
            Vec::<String>::new()
        );
        assert_eq!(
            binaries("uv run --with rich python -m pytest"),
            Vec::<String>::new()
        );
        assert_eq!(binaries("trap - EXIT"), Vec::<String>::new());
    }

    /// Python targets behind the wrappers, the interpreter's option clusters
    /// and values, and `pypy` (#1393, #1394, #1396, #1397, #1399, #1400).
    #[test]
    fn python_targets_behind_wrappers_and_option_clusters() {
        for command in [
            "sudo -u app python -m pkg.tool",
            "hatch run dev:python -m pkg.tool",
            "conda run -n app python -m pkg.tool",
            "timeout 600 python -m pkg.tool",
            "find data -exec python -m pkg.tool {} \\;",
            "/usr/bin/env python3 -m pkg.tool",
            "uv run nohup python -m pkg.tool",
            "uv run time python -m pkg.tool",
            "python -um pkg.tool",
            "python -Om pkg.tool",
            "python -mpkg.tool",
            "python -X dev -m pkg.tool",
            "python -Xdev -m pkg.tool",
            "python --check-hash-based-pycs never -m pkg.tool",
            "pypy3 -m pkg.tool",
            "pypy3.10 -m pkg.tool",
        ] {
            assert_eq!(targets(command), ["run pkg.tool"], "{command}");
        }
        assert_eq!(
            targets("uv run --no-sync pdm run python pkg/gen.py"),
            ["script pkg/gen.py"]
        );
        assert_eq!(targets("python -- pkg/gen.py"), ["script pkg/gen.py"]);
        assert_eq!(
            targets("python -W ignore pkg/gen.py"),
            ["script pkg/gen.py"]
        );
        assert_eq!(binaries("python -um pytest"), ["pytest"]);
        assert_eq!(binaries("pypy3 -m pytest"), ["pytest"]);
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

    /// A condition's command runs (#1359).
    #[test]
    fn reads_command_word_after_condition_keywords() {
        for command in [
            "if pytest -q; then echo ok; fi",
            "if ! pytest -q; then exit 1; fi",
            "while pytest -x; do sleep 1; done",
            "until pytest; do sleep 1; done",
            "if x; then :; elif pytest; then :; fi",
        ] {
            assert_eq!(binaries(command), ["pytest"], "{command}");
        }
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

    /// An assignment needs a shell name before `=`; a path through a
    /// variable and a script before `-m` name no binary.
    #[test]
    fn unread_command_words_name_no_binary() {
        assert_eq!(binaries("1X=1 pytest"), Vec::<String>::new());
        assert_eq!(binaries("X-Y=1 pytest"), Vec::<String>::new());
        assert_eq!(binaries("$VENV/bin/pytest"), Vec::<String>::new());
        assert_eq!(binaries("{{venv}}/bin/pytest"), Vec::<String>::new());
        assert_eq!(binaries("python manage.py -m pytest"), Vec::<String>::new());
    }

    fn segments(command: &str) -> Vec<Vec<String>> {
        let mut segments = Vec::new();
        for_each_segment(command, |words| {
            segments.push(words.iter().map(|word| (*word).to_owned()).collect());
        });
        segments
    }

    /// Subshell depth and the separators after an arm decide where a pattern
    /// may start.
    #[test]
    fn separators_track_subshells_and_case_arms() {
        // A closed subshell leaves the next line at top level.
        assert_eq!(segments("(a)\nwheel|x) pytest"), [["a"], ["pytest"]]);
        // Inside an open subshell a line start is a command, not a pattern.
        assert_eq!(
            segments("(\n  foo | bar)\npytest"),
            [["foo"], ["bar"], ["pytest"]]
        );
        // The arm after `;;`, `;&` and `;;&` starts with a pattern, wherever
        // the separator sits.
        for separator in [";;", ";&", ";;&"] {
            assert_eq!(
                segments(&format!(
                    "case \"$1\" in start) serve {separator} wheel|stop) pytest {separator} esac"
                )),
                [
                    vec!["case", "$1", "in"],
                    vec!["serve"],
                    vec!["pytest"],
                    vec!["esac"]
                ],
                "{separator}"
            );
        }
        // `name()` defines a function: its body still runs commands.
        assert_eq!(
            segments("longname() { pytest; }"),
            [vec!["longname"], vec!["{", "pytest"], vec!["}"]]
        );
    }

    /// A substitution ends at the bracket that balances its opening one;
    /// quoted and escaped brackets do not count.
    #[test]
    fn substitutions_end_at_the_balancing_bracket() {
        fn end(text: &str, brackets: (char, char), literal_apostrophe: bool) -> String {
            let chars: Vec<char> = text.chars().collect();
            chars[..balanced(&chars, 0, brackets, literal_apostrophe)]
                .iter()
                .collect()
        }
        let parens = ('(', ')');
        for text in [
            "$(a (b) c)",
            "$(echo ')' x)",
            "$(echo \")\" x)",
            "$(echo \"\\\")\" x)",
            "$(echo \\) x)",
        ] {
            assert_eq!(end(&format!("{text} tail)"), parens, false), text);
        }
        // Inside `"..."` an apostrophe is a literal (`"${x:-'}'}"`).
        assert_eq!(end("${x:-'}'} tail}", ('{', '}'), true), "${x:-'}");
        assert_eq!(end("${x:-'}'} tail}", ('{', '}'), false), "${x:-'}'}");
        // An unterminated substitution runs to the end.
        assert_eq!(end("$(echo \"x", parens, false), "$(echo \"x");
    }

    /// A quoted pattern may hold `)` and `|`; the list ends at the first
    /// unquoted `)`.
    #[test]
    fn quoted_case_patterns_hold_brackets() {
        for pattern in ["'a)b'|wheel", "\"a)b\"|wheel", "x|'a)b'", "x|\"a)b\""] {
            let line = format!("{pattern}) pytest");
            let chars: Vec<char> = line.chars().collect();
            assert_eq!(
                case_pattern_end(&chars, 0),
                Some(pattern.len()),
                "{pattern}"
            );
            assert_eq!(segments(&line), [["pytest"]], "{pattern}");
        }
    }

    /// Quotes and escapes are removed; substitutions keep their raw text.
    #[test]
    fn words_keep_substitutions_raw() {
        assert_eq!(segments("echo \"a\\\"b\" c"), [["echo", "a\"b", "c"]]);
        assert_eq!(
            segments("echo \"x$(a \"b c\")y\" z"),
            [vec!["a", "b c"], vec!["echo", "x$(a \"b c\")y", "z"]]
        );
        assert_eq!(
            segments("X=`a b` c"),
            [vec!["a", "b"], vec!["X=`a b`", "c"]]
        );
        assert_eq!(
            segments("X=$(a b) c ${d e}"),
            [vec!["a", "b"], vec!["X=$(a b)", "c", "${d e}"]]
        );
    }

    /// Scripts nested past [`MAX_SHELL_NESTING`] stay one opaque word.
    #[test]
    fn shell_scripts_nest_to_a_bound() {
        let chars: Vec<char> = "sh -c 'pytest -q'".chars().collect();
        let mut out = Vec::new();
        ShellSplitter::new(MAX_SHELL_NESTING, &mut out).split(&chars);
        assert_eq!(out, [["sh", "-c", "pytest -q"]]);
        let mut out = Vec::new();
        ShellSplitter::new(MAX_SHELL_NESTING - 1, &mut out).split(&chars);
        assert_eq!(out, [vec!["sh", "-c"], vec!["pytest", "-q"]]);
    }

    /// A `|` inside quotes, a parameter expansion or a `case` pattern list
    /// separates no command; a real pipeline still does (#1341).
    #[test]
    fn quoted_text_and_case_patterns_run_no_command() {
        for command in [
            "echo \"<source|pytest|docker>\"",
            "echo '<source|pytest|docker>'",
            "DIST=\"${1:?usage: <source|pytest|docker>}\"",
            "DIST=${1:?usage: <source|pytest|docker>}",
            "DIST=\"<source|pytest|docker>\"",
            "DIST=\"a pytest\"",
            "echo a\\|pytest",
            "X=$(echo a | tr a b)",
            "X=`echo a | tr a b`",
            "  source|pytest|docker) ;;",
            "  source | \"pytest\")",
            "if x; then case \"$1\" in api|pytest) run ;; esac; fi",
            "pytest) echo x ;;",
            "case \"$1\" in\n  source|pytest|docker) ;;\n  *) exit 1 ;;\nesac",
            "case \"$1\" in source|pytest) ;; celery) ;; esac",
            "echo \"unterminated | pytest",
        ] {
            assert_eq!(binaries(command), Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn pipelines_subshells_and_shell_scripts_still_run_commands() {
        assert_eq!(binaries("producer | pytest -q"), ["pytest"]);
        assert_eq!(binaries("echo \"a|b\" | pytest -q"), ["pytest"]);
        assert_eq!(binaries("(cd app && pytest)"), ["pytest"]);
        assert_eq!(binaries("cd app && (pytest)"), ["pytest"]);
        assert_eq!(
            binaries("bash -c 'cd x; celery worker | tee log'"),
            ["celery"]
        );
        assert_eq!(binaries("case \"$1\" in a) pytest ;; esac"), ["pytest"]);
        assert_eq!(binaries("  a|b) pytest -x ;;"), ["pytest"]);
        // A quoted or escaped `)` does not close the pattern list.
        for command in [
            "'a)b'|celery) pytest ;;",
            "\"a)b\"|celery) pytest ;;",
            "a\\)b|celery) pytest ;;",
            "$x|celery) pytest ;;",
        ] {
            assert_eq!(binaries(command), ["pytest"], "{command}");
        }
        assert_eq!(binaries("$(x)|celery) pytest"), ["celery", "pytest"]);
        assert_eq!(binaries("main() { pytest; }"), ["pytest"]);
        assert_eq!(binaries("pytest  # | celery"), ["pytest"]);
        assert_eq!(binaries("\"pytest -q\""), ["pytest"]);
        assert_eq!(binaries("bash -lc 'cd app && celery worker'"), ["celery"]);
        assert_eq!(binaries("bash -e -c \"pytest -q\""), ["pytest"]);
        assert_eq!(binaries("(pytest)"), ["pytest"]);
        assert_eq!(binaries("bash pytest"), Vec::<String>::new());
        assert_eq!(binaries("bash --norc pytest"), Vec::<String>::new());
        assert_eq!(binaries("(cd x\n  pytest )"), ["pytest"]);
        assert_eq!(
            binaries("echo \"${MSG:-it's done}\"; pytest -q"),
            ["pytest"]
        );
    }

    /// The module half of an app reference is a dotted name or computed,
    /// and the attribute half is a non-empty name, not a path or a port.
    #[test]
    fn app_references_need_a_module_and_an_attribute() {
        for arg in ["pkg.app:app", "pkg:create()", "$APP_MODULE:app"] {
            assert!(is_app_reference(arg), "{arg}");
        }
        for arg in ["my-pkg:app", "pkg:", "pkg:/static", "localhost:8000", "app"] {
            assert!(!is_app_reference(arg), "{arg}");
        }
    }

    #[test]
    fn dotted_identifiers_take_underscores_but_no_punctuation() {
        for module in ["pkg", "_private.mod", "pkg.sub_mod", "pkg2.v1"] {
            assert!(is_dotted_identifier(module), "{module}");
        }
        for module in ["", "my-pkg", "pkg.", "1pkg", "pkg.-x"] {
            assert!(!is_dotted_identifier(module), "{module}");
        }
    }

    #[test]
    fn backslash_continuations_stay_in_one_segment() {
        assert_eq!(binaries("echo start \\\n  pytest"), Vec::<String>::new());
        assert_eq!(binaries("pytest \\\r\n  --cov"), ["pytest"]);
    }

    fn uses(command: &str) -> (Vec<String>, Vec<String>) {
        let uses = command_uses(command);
        let symbols = uses
            .symbols
            .into_iter()
            .map(|(module, name)| format!("{module}:{name}"));
        (
            uses.distributions,
            uses.modules.into_iter().chain(symbols).collect(),
        )
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// A celery plugin subcommand, a pool or worker class, and the class a
    /// dotted worker name loads (#1240).
    #[test]
    fn command_arguments_name_worker_distributions() {
        let none = (Vec::new(), Vec::new());
        let dist = |name: &str| (names(&[name]), Vec::new());
        assert_eq!(
            uses("celery --app wger --broker=\"${B}\" flower --basic_auth=a:b"),
            dist("flower")
        );
        assert_eq!(uses("celery -A proj -b redis:// flower"), dist("flower"));
        assert_eq!(uses("celery -A flower worker"), none);
        assert_eq!(
            uses("exec celery --app wger worker --pool=gevent -c 4"),
            dist("gevent")
        );
        assert_eq!(
            uses("python -m celery -A app worker -P eventlet"),
            dist("eventlet")
        );
        assert_eq!(uses("celery -A app worker -Pgevent"), dist("gevent"));
        assert_eq!(uses("celery -A app worker -P prefork"), none);
        assert_eq!(uses("celery -A app worker -P $POOL"), none);
        assert_eq!(uses("gunicorn -k gevent app:app"), dist("gevent"));
        assert_eq!(
            uses("uv run gunicorn --worker-class=eventlet app:app"),
            dist("eventlet")
        );
        assert_eq!(
            uses("gunicorn -k uvicorn.workers.UvicornWorker main:app"),
            (Vec::new(), names(&["uvicorn.workers:UvicornWorker"]))
        );
        assert_eq!(uses("gunicorn -k gthread app:app"), none);
        assert_eq!(
            uses("gunicorn -k egg:meinheld#gunicorn_worker app:app"),
            dist("meinheld")
        );
        assert_eq!(uses("gunicorn -k egg:-bad#worker app:app"), none);
        assert_eq!(uses("echo celery flower && echo gunicorn -k gevent"), none);
    }

    /// The tool after `python -m` is found past interpreter flags, with
    /// `-X` / `-W` taking a value; a script names no tool.
    #[test]
    fn python_flags_precede_the_module() {
        let none = (Vec::new(), Vec::new());
        let dist = |name: &str| (names(&[name]), Vec::new());
        assert_eq!(uses("python -m celery flower"), dist("flower"));
        assert_eq!(uses("python -mcelery flower"), dist("flower"));
        assert_eq!(uses("python -X dev -m celery flower"), dist("flower"));
        assert_eq!(uses("python -W ignore -m celery flower"), dist("flower"));
        assert_eq!(
            uses("python -u -m gunicorn -k gevent app:app"),
            dist("gevent")
        );
        assert_eq!(uses("python -X -m celery flower"), none);
        assert_eq!(uses("python manage.py -m celery flower"), none);
        assert_eq!(uses("python -u"), none);
        assert_eq!(uses("python -m"), none);
    }

    /// celery's `--scheduler` names a class and pytest-django's `--ds` a
    /// module (#1239); `-S` is the scheduler only for `beat`.
    #[test]
    fn command_options_name_modules() {
        assert_eq!(
            uses(
                "celery --app saleor.celeryconf:app beat --scheduler \
                 saleor.schedulers.schedulers.DatabaseScheduler"
            )
            .1,
            names(&["saleor.schedulers.schedulers:DatabaseScheduler"])
        );
        assert_eq!(
            uses("celery -A proj beat -S django_celery_beat.schedulers:DatabaseScheduler").1,
            names(&["django_celery_beat.schedulers:DatabaseScheduler"])
        );
        assert_eq!(
            uses("celery -A proj worker -S worker.state").1,
            Vec::<String>::new()
        );
        assert_eq!(
            uses("pytest --ds=saleor.tests.settings -n auto").1,
            names(&["saleor.tests.settings"])
        );
        assert_eq!(
            uses("python -m pytest --ds app.settings").1,
            names(&["app.settings"])
        );
        assert_eq!(
            uses("celery -A proj beat --scheduler 1bad.mod:Scheduler").1,
            Vec::<String>::new()
        );
        assert_eq!(
            uses("celery -A proj beat --scheduler pkg.mod:1bad").1,
            Vec::<String>::new()
        );
        assert_eq!(uses("pytest --ds=$SETTINGS").1, Vec::<String>::new());
        assert_eq!(uses("tox -- --ds=app.settings").1, Vec::<String>::new());
    }

    /// requests' publish recipe installs twine right before `twine upload`
    /// (#1241).
    #[test]
    fn installs_in_commands_provide_distributions() {
        let origin = ReferenceOrigin {
            file: "Makefile".to_owned(),
            line: Some(20),
            label: "make recipe".to_owned(),
        };
        let mut hits = SourceHits::default();
        hits.push_command("python -m pip install 'twine>=1.5.0'", &known, &origin);
        hits.push_command("twine upload dist/*", &known, &origin);
        hits.push_command("pip install -r requirements-dev.txt", &known, &origin);
        assert_eq!(hits.installed, ["twine"]);
        assert_eq!(hits.requirement_files.len(), 1);
        assert_eq!(hits.requirement_files[0].path, "requirements-dev.txt");
    }

    /// mypy's `tox.ini` points mypy at `mypy_self_check.ini` (#1273).
    #[test]
    fn config_file_options_name_tool_configs() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            (
                "python -m mypy --config-file mypy_self_check.ini misc",
                &[("mypy", "mypy_self_check.ini")],
            ),
            ("mypy --config-file=ci/mypy.ini", &[("mypy", "ci/mypy.ini")]),
            (
                "pytest -c ci/pytest.ini tests",
                &[("pytest", "ci/pytest.ini")],
            ),
            ("pytest --config-file ci.toml", &[("pytest", "ci.toml")]),
            ("mypy src", &[]),
        ];
        for (command, expected) in cases {
            let uses = command_uses(command);
            let found: Vec<(&str, &str)> = uses
                .tool_configs
                .iter()
                .map(|(tool, path)| (*tool, path.as_str()))
                .collect();
            assert_eq!(found, *expected, "{command}");
        }
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
        assert_eq!(scripts("pipx run --spec ./tools a.py"), []);
        assert_eq!(scripts("pipx run --spec=./tools a.py"), []);
        assert_eq!(scripts("pipx run https://example.com/gen.py"), []);
        assert_eq!(scripts("pipx install gen.py"), []);
        assert_eq!(scripts("pipx run $SCRIPT"), []);
        assert_eq!(
            scripts("pipx run -i https://pypi.example/simple tools/a.py"),
            [script("tools/a.py")]
        );
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

    /// An option glued to a variable is its value, and the `:` inside
    /// `${...}` splits no module from an attribute.
    #[test]
    fn command_targets_skip_option_values_and_variable_defaults() {
        for (command, app) in [
            ("gunicorn -w${W:-$N} mypkg.wsgi:app", "app mypkg.wsgi:app"),
            ("gunicorn -b \"$BIND\" mypkg.wsgi:app", "app mypkg.wsgi:app"),
            ("gunicorn ${APP:-mypkg.wsgi}", "opaque ${APP:-mypkg.wsgi}"),
            ("gunicorn ${APP}:app", "opaque ${APP}:app"),
            ("gunicorn $APP_MODULE:app", "opaque $APP_MODULE:app"),
            (
                "gunicorn -b$HOST:$PORT mypkg.wsgi:app",
                "app mypkg.wsgi:app",
            ),
            ("gunicorn ${ARGS:-$D} mypkg.wsgi:app", "app mypkg.wsgi:app"),
            ("gunicorn $OPTS ${APP}:app", "opaque ${APP}:app"),
            // A colon word whose left side names no module is no reference.
            ("gunicorn my-pkg.wsgi:app", "opaque gunicorn"),
        ] {
            assert_eq!(targets(command), [app], "{command}");
        }
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
    fn streamlit_run_runs_its_script() {
        assert_eq!(targets("streamlit run app.py"), ["script app.py"]);
        assert_eq!(
            targets("uv run streamlit run --server.port 80 ui/app.py -- --debug"),
            ["script ui/app.py"]
        );
        assert_eq!(targets("streamlit run $APP"), ["opaque $APP"]);
        assert_eq!(
            targets("streamlit run https://x.io/app"),
            Vec::<String>::new()
        );
        assert_eq!(targets("streamlit hello"), Vec::<String>::new());
        assert_eq!(
            targets("uv run streamlit config show x.py"),
            Vec::<String>::new()
        );
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

    #[test]
    fn notebook_commands_run_in_the_notebooks_directory() {
        use crate::parser::{NotebookCommand, ParsedModule};
        use crate::sources::FileKind;

        let notebook = |path: &str, commands: &[&str]| ParsedModule {
            path: path.to_owned(),
            notebook_commands: commands
                .iter()
                .map(|command| NotebookCommand {
                    command: (*command).to_owned(),
                    line: 2,
                })
                .collect(),
            ..ParsedModule::default()
        };
        let parse = ParseSummary {
            modules: vec![
                notebook(
                    "notebooks/demo.ipynb",
                    &[
                        "python helper.py",
                        "python ../scripts/train.py",
                        "python -m pkg.cli",
                    ],
                ),
                notebook("app.ipynb", &["python $script"]),
            ],
        };
        let files = [DiscoveredFile {
            path: "app.ipynb".to_owned(),
            kind: FileKind::Notebook,
            context: FileContext::Runtime,
        }];
        let refs: Vec<(String, CommandTarget, bool)> = parsed_notebook_commands(&parse, &files)
            .map(|reference| (reference.origin.file, reference.target, reference.deploy))
            .collect();
        let script = |path: &str| CommandTarget::Script {
            paths: vec![path.to_owned()],
        };
        let demo = "notebooks/demo.ipynb".to_owned();
        assert_eq!(
            refs,
            [
                (demo.clone(), script("notebooks/helper.py"), false),
                (demo.clone(), script("scripts/train.py"), false),
                (
                    demo,
                    CommandTarget::Run {
                        module: "pkg.cli".to_owned()
                    },
                    false
                ),
                (
                    "app.ipynb".to_owned(),
                    CommandTarget::Opaque {
                        text: "$script".to_owned()
                    },
                    true
                ),
            ]
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
