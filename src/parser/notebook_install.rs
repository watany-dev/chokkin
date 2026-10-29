//! Distributions a notebook cell installs into its own kernel (#991).
//!
//! `%pip install X`, `!pip install X`, `!{sys.executable} -m pip install X`
//! and `%conda install X` install `X` into the environment the notebook
//! runs in, which declares `X` for that notebook's imports the way a PEP 723
//! block declares a script's. Each argument is read as a requirement. One
//! that installs something not named on the line (`-r file`, a path, a
//! variable `IPython` expands, an option not known) is kept unread, without
//! a distribution, so its notebook's missing-dependency findings can be
//! lowered rather than kept (ADR 0005). A command that installs elsewhere
//! (`--target`, `--prefix`, `conda -n`) or does not install (`pip
//! uninstall`, `--dry-run`) declares nothing.

use crate::manifest::{
    archive_distribution_name, is_project_name, normalize_distribution_name, requirement_name,
};

/// One argument of an install command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Install {
    /// The argument as written, with its option for `-r` and `--file`.
    pub(super) argument: String,
    /// The normalized distribution it installs; `None` when it is not read.
    pub(super) distribution: Option<String>,
}

/// pip options that take a value, as the next word or after `=`.
const PIP_VALUED: &[&str] = &[
    "-i",
    "--index-url",
    "--extra-index-url",
    "-f",
    "--find-links",
    "-c",
    "--constraint",
    "--trusted-host",
    "--progress-bar",
    "--upgrade-strategy",
    "--platform",
    "--python-version",
    "--implementation",
    "--abi",
    "--global-option",
    "-C",
    "--config-settings",
    "--no-binary",
    "--only-binary",
    "--log",
    "--proxy",
    "--retries",
    "--timeout",
    "--cache-dir",
    "--src",
    "--exists-action",
    "--cert",
    "--client-cert",
    "--keyring-provider",
    "--root-user-action",
    "--report",
    // uv pip
    "-p",
    "--python",
    "--index",
    "--default-index",
    "--index-strategy",
    "--resolution",
    "--prerelease",
    "-P",
    "--upgrade-package",
    "--python-platform",
];

/// pip options that take no value.
const PIP_FLAGS: &[&str] = &[
    "--upgrade",
    "--quiet",
    "--verbose",
    "--pre",
    "--no-deps",
    "--force-reinstall",
    "--user",
    "--no-cache-dir",
    "--no-build-isolation",
    "--ignore-installed",
    "--no-index",
    "--break-system-packages",
    "--no-warn-script-location",
    "--no-warn-conflicts",
    "--disable-pip-version-check",
    "--no-color",
    "--isolated",
    "--require-hashes",
    "--compile",
    "--no-compile",
    "--prefer-binary",
    "--no-input",
    "--no-clean",
    "--use-pep517",
    "--no-use-pep517",
    "--ignore-requires-python",
    "--check-build-dependencies",
    // uv pip
    "--system",
    "--reinstall",
];

/// Short pip flags that may be clustered (`-qU`).
const PIP_SHORT_FLAGS: &str = "qvUI";

/// pip options after which nothing installs into the kernel's environment.
const PIP_ELSEWHERE: &[&str] = &["-t", "--target", "--prefix", "--root", "--dry-run"];

/// conda options that take a value.
const CONDA_VALUED: &[&str] = &["-c", "--channel", "--repodata-fn"];

/// conda options that take no value.
const CONDA_FLAGS: &[&str] = &[
    "--yes",
    "--quiet",
    "--force-reinstall",
    "--no-deps",
    "--only-deps",
    "--update-deps",
    "--no-update-deps",
    "--satisfied-skip-solve",
    "--freeze-installed",
    "--update-all",
    "--all",
    "--strict-channel-priority",
    "--override-channels",
];

/// Short conda flags that may be clustered (`-yq`).
const CONDA_SHORT_FLAGS: &str = "yqS";

/// conda options after which nothing installs into the kernel's environment.
const CONDA_ELSEWHERE: &[&str] = &["-n", "--name", "-p", "--prefix", "-d", "--dry-run"];

/// What `%pip <args>` installs.
pub(super) fn pip_magic(args: &str) -> Vec<Install> {
    pip_command(&words(args)).unwrap_or_default()
}

/// What `%conda <args>` (or `%mamba`, `%micromamba`) installs.
pub(super) fn conda_magic(args: &str) -> Vec<Install> {
    conda_command(&words(args)).unwrap_or_default()
}

/// What the shell command of `!<command>` installs: each `pip`, `python -m
/// pip`, `uv pip` or `conda` install of it, split at `;`, `|` and `&`.
pub(super) fn shell(command: &str) -> Vec<Install> {
    segments(command)
        .iter()
        .filter_map(|segment| segment_installs(segment))
        .flatten()
        .collect()
}

/// A shell word: its text without quotes, and whether `IPython` expands it
/// (`$name`, `{expr}`) before the shell sees it.
#[derive(Debug)]
struct Word {
    text: String,
    expanded: bool,
}

/// The words of `command`, without a trailing comment, in one segment.
fn words(command: &str) -> Vec<Word> {
    segments(command).into_iter().next().unwrap_or_default()
}

/// The words of each segment of `command`, as a shell splits them: quotes
/// group, `#` at a word start comments out the rest, and `;`, `|` and `&`
/// outside quotes end a segment. A redirection and its target are dropped.
fn segments(command: &str) -> Vec<Vec<Word>> {
    let mut segments = vec![Vec::new()];
    let mut word: Option<Word> = None;
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut redirect = false;
    while let Some(char) = chars.next() {
        if let Some(open) = quote {
            if char == open {
                quote = None;
            } else {
                let word = word.get_or_insert_with(Word::empty);
                word.expanded |= matches!(char, '$' | '{');
                word.text.push(char);
            }
            continue;
        }
        match char {
            '\'' | '"' => {
                quote = Some(char);
                word.get_or_insert_with(Word::empty);
            },
            '#' if word.is_none() => break,
            ';' | '|' | '&' => {
                finish(&mut word, &mut segments, &mut redirect);
                // `2>&1` and `&>` redirect rather than end the segment.
                if char == '&' && chars.peek() == Some(&'>') {
                    redirect = true;
                    chars.next();
                } else if segments.last().is_some_and(|segment| !segment.is_empty()) {
                    segments.push(Vec::new());
                }
            },
            '<' | '>' => {
                // A word of digits before `>` is the descriptor, not a word.
                if word
                    .as_ref()
                    .is_some_and(|word| word.text.chars().all(|char| char.is_ascii_digit()))
                {
                    word = None;
                } else {
                    finish(&mut word, &mut segments, &mut redirect);
                }
                while chars.peek().is_some_and(|next| matches!(next, '<' | '>')) {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2` names a descriptor, not a file.
                    chars.next();
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                } else {
                    redirect = true;
                }
            },
            _ if char.is_whitespace() => finish(&mut word, &mut segments, &mut redirect),
            _ => {
                let word = word.get_or_insert_with(Word::empty);
                word.expanded |= matches!(char, '$' | '{');
                word.text.push(char);
            },
        }
    }
    finish(&mut word, &mut segments, &mut redirect);
    segments.retain(|segment| !segment.is_empty());
    segments
}

/// End the word being read, dropping it when it is a redirection's target.
fn finish(word: &mut Option<Word>, segments: &mut [Vec<Word>], redirect: &mut bool) {
    if let Some(word) = word.take() {
        let target = std::mem::take(redirect);
        if !target && let Some(segment) = segments.last_mut() {
            segment.push(word);
        }
    }
}

impl Word {
    const fn empty() -> Self {
        Self {
            text: String::new(),
            expanded: false,
        }
    }
}

/// What one shell segment installs, `None` when it runs no install.
fn segment_installs(words: &[Word]) -> Option<Vec<Install>> {
    // `PIP_NO_INPUT=1 pip install x`: leading variable assignments.
    let start = words.iter().position(|word| !is_assignment(&word.text))?;
    let words = &words[start..];
    let program = words.first()?;
    let name = program.text.rsplit(['/', '\\']).next().unwrap_or_default();
    let rest = &words[1..];
    if is_versioned(name, "pip") {
        pip_command(rest)
    } else if program.text == "{sys.executable}"
        || (!program.expanded && is_versioned(name, "python"))
    {
        python_m_pip(rest)
    } else if name == "uv" {
        let pip = rest.iter().position(|word| !word.text.starts_with('-'))?;
        (rest[pip].text == "pip")
            .then(|| pip_command(&rest[pip + 1..]))
            .flatten()
    } else if matches!(name, "conda" | "mamba" | "micromamba") {
        conda_command(rest)
    } else {
        None
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .chars()
                .all(|char| char.is_ascii_alphanumeric() || char == '_')
    })
}

/// `pip`, `pip3`, `pip3.12`; `python`, `python3.11`; `.exe` on Windows.
fn is_versioned(name: &str, program: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name.strip_prefix(program).is_some_and(|version| {
        version
            .chars()
            .all(|char| char.is_ascii_digit() || char == '.')
    })
}

/// `python [options] -m pip …`: the interpreter options before `-m`, which
/// may cluster (`-Im`, `-mpip`). A script or `-c` code runs no pip.
fn python_m_pip(words: &[Word]) -> Option<Vec<Install>> {
    let mut index = 0;
    loop {
        let flags = words.get(index)?.text.strip_prefix('-')?;
        index += 1;
        if flags.starts_with('-') {
            continue;
        }
        for (at, flag) in flags.char_indices() {
            let value = &flags[at + flag.len_utf8()..];
            match flag {
                'm' => {
                    let module = if value.is_empty() {
                        index += 1;
                        words.get(index - 1)?.text.as_str()
                    } else {
                        value
                    };
                    return (module == "pip")
                        .then(|| pip_command(&words[index..]))
                        .flatten();
                },
                'c' => return None,
                // `-W` and `-X` take the rest of the cluster or the next word.
                'W' | 'X' => {
                    if value.is_empty() {
                        index += 1;
                    }
                    break;
                },
                _ => {},
            }
        }
    }
}

/// `pip [global options] install …`; `None` for any other subcommand.
fn pip_command(words: &[Word]) -> Option<Vec<Install>> {
    let mut index = 0;
    while let Some(word) = words.get(index)
        && word.text.starts_with('-')
    {
        index += if !word.text.contains('=') && PIP_VALUED.contains(&word.text.as_str()) {
            2
        } else {
            1
        };
    }
    (words.get(index)?.text == "install")
        .then(|| install_args(&words[index + 1..], &PIP))
        .flatten()
}

/// `conda install …`; `None` for any other subcommand.
fn conda_command(words: &[Word]) -> Option<Vec<Install>> {
    let index = words.iter().position(|word| !word.text.starts_with('-'))?;
    (words[index].text == "install")
        .then(|| install_args(&words[index + 1..], &CONDA))
        .flatten()
}

/// How one installer reads its arguments.
struct Installer {
    valued: &'static [&'static str],
    flags: &'static [&'static str],
    short_flags: &'static str,
    elsewhere: &'static [&'static str],
    /// Options whose value is a file of requirements, kept unread.
    files: &'static [&'static str],
    /// Options whose value is itself an argument (`-e`).
    arguments: &'static [&'static str],
    argument: fn(&str) -> Option<String>,
}

const PIP: Installer = Installer {
    valued: PIP_VALUED,
    flags: PIP_FLAGS,
    short_flags: PIP_SHORT_FLAGS,
    elsewhere: PIP_ELSEWHERE,
    files: &["-r", "--requirement"],
    arguments: &["-e", "--editable"],
    argument: pip_argument,
};

const CONDA: Installer = Installer {
    valued: CONDA_VALUED,
    flags: CONDA_FLAGS,
    short_flags: CONDA_SHORT_FLAGS,
    elsewhere: CONDA_ELSEWHERE,
    files: &["--file"],
    arguments: &[],
    argument: conda_argument,
};

/// The installs an install subcommand's arguments name; `None` when it
/// installs outside the kernel's environment. An option not known ends the
/// reading: its value, if it takes one, could be read as a requirement.
fn install_args(words: &[Word], installer: &Installer) -> Option<Vec<Install>> {
    let mut installs = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let text = word.text.as_str();
        if word.expanded {
            installs.push(unread(text.to_owned()));
            continue;
        }
        if !text.starts_with('-') || text == "-" {
            installs.push(Install {
                argument: text.to_owned(),
                distribution: (installer.argument)(text),
            });
            continue;
        }
        let (option, inline) = split_option(text, installer);
        let mut value = || {
            inline
                .map(str::to_owned)
                .or_else(|| words.next().map(|word| word.text.clone()))
        };
        if installer.elsewhere.contains(&option) {
            return None;
        } else if installer.files.contains(&option) {
            let file = value().unwrap_or_default();
            installs.push(unread(format!("{option} {file}")));
        } else if installer.arguments.contains(&option) {
            let argument = value().unwrap_or_default();
            let distribution = (installer.argument)(&argument);
            installs.push(Install {
                argument,
                distribution,
            });
        } else if installer.valued.contains(&option) {
            value();
        } else if !installer.flags.contains(&option) && !is_flag_cluster(text, installer) {
            installs.push(unread(text.to_owned()));
            break;
        }
    }
    Some(installs)
}

/// An option and its value in the same word: `--opt=value`, or a short
/// option that takes a value followed by it (`-rreq.txt`).
fn split_option<'a>(text: &'a str, installer: &Installer) -> (&'a str, Option<&'a str>) {
    if text.starts_with("--") {
        return text
            .split_once('=')
            .map_or((text, None), |(option, value)| (option, Some(value)));
    }
    if text.len() > 2 && text.is_char_boundary(2) {
        let option = &text[..2];
        let takes_value = [
            installer.valued,
            installer.files,
            installer.arguments,
            installer.elsewhere,
        ]
        .iter()
        .any(|options| options.contains(&option));
        if takes_value {
            return (option, Some(&text[2..]));
        }
    }
    (text, None)
}

fn is_flag_cluster(text: &str, installer: &Installer) -> bool {
    text.strip_prefix('-').is_some_and(|flags| {
        !flags.is_empty()
            && flags
                .chars()
                .all(|flag| installer.short_flags.contains(flag))
    })
}

fn unread(argument: String) -> Install {
    Install {
        argument,
        distribution: None,
    }
}

/// A pip requirement: a PEP 508 requirement, a URL with `#egg=`, or an
/// archive whose file name carries the distribution.
fn pip_argument(argument: &str) -> Option<String> {
    requirement_name(argument).or_else(|| archive_distribution_name(argument))
}

/// A conda match spec: `[channel::]name[version][=build]`.
fn conda_argument(argument: &str) -> Option<String> {
    let spec = argument.rsplit("::").next().unwrap_or(argument);
    let end = spec
        .find(['=', '<', '>', '!', '~', ' ', '['])
        .unwrap_or(spec.len());
    let name = &spec[..end];
    is_project_name(name).then(|| normalize_distribution_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(installs: &[Install]) -> Vec<(&str, Option<&str>)> {
        installs
            .iter()
            .map(|install| (install.argument.as_str(), install.distribution.as_deref()))
            .collect()
    }

    #[test]
    fn pip_magic_reads_requirements_and_skips_options() {
        let installs = pip_magic(
            "install -qU \"llama-index-llms-openai>=0.1\" python_dotenv 'pkg[extra]' --index-url https://x -c c.txt",
        );
        assert_eq!(
            read(&installs),
            [
                (
                    "llama-index-llms-openai>=0.1",
                    Some("llama-index-llms-openai")
                ),
                ("python_dotenv", Some("python-dotenv")),
                ("pkg[extra]", Some("pkg")),
            ]
        );
    }

    #[test]
    fn pip_magic_keeps_what_it_cannot_read_unread() {
        let installs = pip_magic(
            "install -r requirements.txt $pkg {name} ./local git+https://h/r.git#egg=foo-bar --weird x",
        );
        assert_eq!(
            read(&installs),
            [
                ("-r requirements.txt", None),
                ("$pkg", None),
                ("{name}", None),
                ("./local", None),
                ("git+https://h/r.git#egg=foo-bar", Some("foo-bar")),
                ("--weird", None),
            ]
        );
        assert_eq!(
            read(&pip_magic("install -rreq.txt")),
            [("-r req.txt", None)]
        );
        assert_eq!(
            read(&pip_magic("install -e git+https://h/r.git#egg=baz")),
            [("git+https://h/r.git#egg=baz", Some("baz"))]
        );
    }

    #[test]
    fn installs_elsewhere_and_other_subcommands_declare_nothing() {
        for args in [
            "uninstall -y requests",
            "install --target lib requests",
            "install -tlib requests",
            "install --prefix=/opt requests",
            "install --dry-run requests",
            "show requests",
            "list",
            "",
        ] {
            assert!(pip_magic(args).is_empty(), "{args}");
        }
        assert!(conda_magic("install -n other numpy").is_empty());
        assert!(conda_magic("remove numpy").is_empty());
    }

    #[test]
    fn shell_reads_every_pip_spelling() {
        let cases: &[(&str, &[&str])] = &[
            ("pip install requests", &["requests"]),
            ("pip3.11 -q install requests", &["requests"]),
            ("{sys.executable} -m pip install requests", &["requests"]),
            (
                "\"{sys.executable}\" -m pip install requests",
                &["requests"],
            ),
            ("python3 -Im pip install requests", &["requests"]),
            ("/usr/bin/python -mpip install requests", &["requests"]),
            ("python -u -B -m pip install requests", &["requests"]),
            (
                "python -W ignore -X dev -mpip install requests",
                &["requests"],
            ),
            (
                "python -Wignore --isolated -m pip install requests",
                &["requests"],
            ),
            ("python -m pipx install black", &[]),
            ("python -m venv .venv", &[]),
            ("python -Ic 'import x' -m pip install x", &[]),
            // `-c` takes the rest of its cluster as the code: `m` here.
            ("python -cm pip install x", &[]),
            ("python -W", &[]),
            ("pip --log pip.log install requests", &["requests"]),
            ("pip --log=pip.log install requests", &["requests"]),
            ("uv pip install --system requests", &["requests"]),
            ("PIP_NO_INPUT=1 pip install requests", &["requests"]),
            (
                "pip install a > /dev/null 2>&1 && pip install b",
                &["a", "b"],
            ),
            ("pip install a &> log.txt; echo done", &["a"]),
            ("pip install a | tail -n 1", &["a"]),
            ("pip install a # and b", &["a"]),
            (
                "conda install -y -c conda-forge numpy=1.26 conda-forge::scipy",
                &["numpy", "scipy"],
            ),
            ("python script.py install x", &[]),
            ("python -c 'import pip' install x", &[]),
            ("ls -la", &[]),
            ("pipx install black", &[]),
            ("echo pip install x", &[]),
        ];
        for (command, expected) in cases {
            let names: Vec<String> = shell(command)
                .into_iter()
                .filter_map(|install| install.distribution)
                .collect();
            assert_eq!(&names, expected, "{command}");
        }
    }

    /// Redirections and their targets are no arguments, and a descriptor
    /// (`2>&1`) or `&>` keeps the segment going.
    #[test]
    fn shell_redirections_are_not_arguments() {
        for command in [
            "pip install a > /dev/null b",
            "pip install a 2>&1 b",
            "pip install a &> log.txt b",
            "pip install a >>log.txt b <in.txt",
        ] {
            assert_eq!(
                read(&shell(command)),
                [("a", Some("a")), ("b", Some("b"))],
                "{command}"
            );
        }
    }

    /// A word `IPython` expands installs what the expansion names, quoted or
    /// not, even when the text alone reads as a requirement.
    #[test]
    fn expanded_words_stay_unread() {
        assert_eq!(
            read(&shell(
                "pip install pkg@https://h/$v.whl 'other@https://h/{v}.whl'"
            )),
            [
                ("pkg@https://h/$v.whl", None),
                ("other@https://h/{v}.whl", None)
            ]
        );
    }

    /// Only `NAME=value` words lead a command as assignments.
    #[test]
    fn leading_words_that_are_not_assignments_name_the_program() {
        assert!(shell("a.b=1 pip install x").is_empty());
        assert!(shell("=1 pip install x").is_empty());
        assert_eq!(read(&shell("A_1=x pip install y")), [("y", Some("y"))]);
    }

    #[test]
    fn conda_spec_that_is_not_a_name_stays_unread() {
        assert_eq!(
            read(&conda_magic(
                "install --file env.txt 'numpy>=1' ./pkg.tar.bz2"
            )),
            [
                ("--file env.txt", None),
                ("numpy>=1", Some("numpy")),
                ("./pkg.tar.bz2", None)
            ]
        );
    }
}
