//! Rewrite a notebook cell's `IPython` syntax into the Python it stands for
//! (#962).
//!
//! `IPython` turns magics (`%pip`, `%%time`), shell escapes (`!ls`,
//! `x = !ls`) and help (`obj?`) into Python before it compiles a cell. The
//! rewrite keeps one output line per input line, so line numbers stay those
//! of the notebook, and drops only what no rule reads: shell commands, help,
//! and magics that neither run Python nor import a module. A magic that runs
//! Python (`%time`, `%%capture`) keeps its code; one that imports a module
//! (`%load_ext`) becomes that import. Any other magic is not recognized and
//! fails the cell, which the caller reports as unread (ADR 0005).

use ruff_python_ast::token::TokenKind;
use ruff_python_parser::Mode;
use ruff_python_parser::lexer::lex;

use super::dynamic::is_dotted_identifier;

/// Line magics that neither run the cell's Python nor import a module, so
/// dropping them hides nothing a rule reads. `%pip` installs a distribution
/// but does not import it; reading its arguments as declared dependencies
/// is #991.
const INERT_LINE_MAGICS: &[&str] = &[
    "alias",
    "alias_magic",
    "autoawait",
    "autocall",
    "automagic",
    "autoreload",
    "autosave",
    "bookmark",
    "cat",
    "cd",
    "clear",
    "colors",
    "conda",
    "config",
    "connect_info",
    "cp",
    "dhist",
    "dirs",
    "doctest_mode",
    "env",
    "gui",
    "hist",
    "history",
    "killbgscripts",
    "ldir",
    "less",
    "lf",
    "lk",
    "ll",
    "ls",
    "lsmagic",
    "lx",
    "magic",
    "mamba",
    "man",
    "matplotlib",
    "micromamba",
    "mkdir",
    "more",
    "mv",
    "page",
    "pdb",
    "pdef",
    "pdoc",
    "pfile",
    "pinfo",
    "pinfo2",
    "pip",
    "popd",
    "pprint",
    "precision",
    "psearch",
    "psource",
    "pushd",
    "pwd",
    "pycat",
    "quickref",
    "rehashx",
    "reset",
    "reset_selective",
    "rm",
    "rmdir",
    "sc",
    "set_env",
    "store",
    "sx",
    "system",
    "tb",
    "unalias",
    "unload_ext",
    "who",
    "who_ls",
    "whos",
    "xdel",
    "xmode",
];

/// Cell magics whose body is another language, so the cell holds no Python.
const FOREIGN_CELL_MAGICS: &[&str] = &[
    "!",
    "HTML",
    "bash",
    "html",
    "javascript",
    "js",
    "latex",
    "markdown",
    "perl",
    "ruby",
    "sh",
    "svg",
    "sx",
    "system",
];

/// Cell magics that run their body as Python and take no statement on their
/// own line.
const PYTHON_CELL_MAGICS: &[&str] = &["capture", "pypy", "python", "python2", "python3", "time"];

/// Options of `%timeit` and `%prun` that take a value.
const TIMEIT_VALUED: &[&str] = &["n", "r", "p"];
const PRUN_VALUED: &[&str] = &["D", "T", "s", "l"];

/// Extensions `IPython` ships, which `%load_ext` finds under
/// `IPython.extensions` rather than as a top-level module.
const BUILTIN_EXTENSIONS: &[&str] = &["autoreload", "storemagic"];

/// Rewrite `cell` into Python with the same number of lines, or name the
/// `IPython` form that was not recognized and its 0-based line in the cell.
pub(super) fn cell_to_python(cell: &str) -> Result<String, (usize, String)> {
    let lines: Vec<&str> = cell.split_inclusive('\n').collect();
    let mut out = String::with_capacity(cell.len());
    // IPython drops leading blank lines before it looks for a cell magic.
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Ok(cell.to_owned());
    };
    for line in &lines[..first] {
        out.push_str(line);
    }
    let Some(magic) = lines[first].trim_start().strip_prefix("%%") else {
        rewrite_lines(&lines[first..], first, &mut out)?;
        return Ok(out);
    };
    match cell_magic(magic).map_err(|message| (first, message))? {
        CellBody::Python(python) => {
            out.push_str(&python);
            blank_line(lines[first], &mut out);
            rewrite_lines(&lines[first + 1..], first + 1, &mut out)?;
        },
        CellBody::Foreign => {
            for line in &lines[first..] {
                blank_line(line, &mut out);
            }
        },
    }
    Ok(out)
}

enum CellBody {
    /// The body is Python, run after the Python on the magic's own line.
    Python(String),
    Foreign,
}

fn cell_magic(line: &str) -> Result<CellBody, String> {
    let (name, args) = split_magic(line);
    match name {
        // `%%timeit` runs its own line as setup; `%%prun` runs it first.
        "timeit" => Ok(CellBody::Python(statement(args, TIMEIT_VALUED))),
        "prun" => Ok(CellBody::Python(statement(args, PRUN_VALUED))),
        "script" => script_body(args),
        _ if PYTHON_CELL_MAGICS.contains(&name) => Ok(CellBody::Python(String::new())),
        _ if FOREIGN_CELL_MAGICS.contains(&name) => Ok(CellBody::Foreign),
        _ => Err(format!("cell magic `%%{name}` is not recognized")),
    }
}

/// `%%script python3` runs Python like `%%python3`; any other program runs
/// another language.
fn script_body(args: &str) -> Result<CellBody, String> {
    let mut words = args.split_whitespace();
    let program = loop {
        match words.next() {
            Some("--out" | "--err" | "--proc") => {
                words.next();
            },
            Some(word) if word.starts_with('-') => {},
            Some(word) => break word,
            None => return Err("cell magic `%%script` names no program".to_owned()),
        }
    };
    let program = program.rsplit(['/', '\\']).next().unwrap_or_default();
    Ok(
        if program.starts_with("python") || program.starts_with("pypy") {
            CellBody::Python(String::new())
        } else {
            CellBody::Foreign
        },
    )
}

/// Rewrite each line that starts a statement on its own, the first being the
/// cell's line `start`. A line ending in `\` continues onto the next:
/// `IPython` joins them with a space before it reads the magic or shell
/// command they make up.
fn rewrite_lines(lines: &[&str], start: usize, out: &mut String) -> Result<(), (usize, String)> {
    let starts = statement_starts(lines);
    let mut next = 0;
    for (index, line) in lines.iter().enumerate() {
        if index < next {
            continue;
        }
        if !starts[index] {
            out.push_str(line);
            continue;
        }
        let text = line.trim_end_matches(['\n', '\r']);
        let mut joined = text.to_owned();
        let mut end = index + 1;
        while let (Some(head), Some(next), Some(false)) =
            (joined.strip_suffix('\\'), lines.get(end), starts.get(end))
        {
            joined = format!("{head} {}", next.trim_end_matches(['\n', '\r']));
            end += 1;
        }
        match rewrite_line(&joined).map_err(|message| (start + index, message))? {
            Some(python) => {
                out.push_str(&python);
                out.push_str(&line[text.len()..]);
                for line in &lines[index + 1..end] {
                    blank_line(line, out);
                }
            },
            None => lines[index..end].iter().for_each(|line| out.push_str(line)),
        }
        next = end;
    }
    Ok(())
}

/// Whether each line starts a statement, the only place `IPython` reads an
/// escape (#994). A line inside brackets or a string that spans lines, or
/// after a `\`, continues the statement before it. The lexer's `IPython`
/// mode reads an escape to the end of its line, so the brackets and quotes
/// of a shell command open nothing.
fn statement_starts(lines: &[&str]) -> Vec<bool> {
    let mut ends = Vec::with_capacity(lines.len());
    let mut end = 0;
    for line in lines {
        end += line.len();
        ends.push(end);
    }
    let mut starts = vec![false; lines.len()];
    if let Some(first) = starts.first_mut() {
        *first = true;
    }
    let source = lines.concat();
    // The lexer alone: the parser panics on some escapes it does not expect,
    // such as help inside unbalanced brackets, and only the tokens matter.
    let mut lexer = lex(&source, Mode::Ipython);
    let mut depth = 0_usize;
    loop {
        match lexer.next_token() {
            TokenKind::EndOfFile => break,
            TokenKind::Lpar | TokenKind::Lsqb | TokenKind::Lbrace => depth += 1,
            TokenKind::Rpar | TokenKind::Rsqb | TokenKind::Rbrace => {
                depth = depth.saturating_sub(1);
            },
            TokenKind::Newline | TokenKind::NonLogicalNewline if depth == 0 => {
                let end = lexer.current_range().end().to_usize();
                if let Ok(line) = ends.binary_search(&end)
                    && let Some(next) = starts.get_mut(line + 1)
                {
                    *next = true;
                }
            },
            _ => {},
        }
    }
    starts
}

/// The Python for one `IPython` line, or `None` when it is plain Python.
fn rewrite_line(text: &str) -> Result<Option<String>, String> {
    let body = text.trim_start();
    let indent = &text[..text.len() - body.len()];
    if body.starts_with('!') || is_help(body) {
        return Ok(Some(format!("{indent}pass")));
    }
    if let Some(magic) = body.strip_prefix('%') {
        if let Some(cell_magic) = magic.strip_prefix('%') {
            let name = cell_magic.split_whitespace().next().unwrap_or_default();
            return Err(format!(
                "cell magic `%%{name}` is not on the cell's first line"
            ));
        }
        // `% pip install x` names `%pip`: whitespace may follow the `%`.
        return line_magic(magic.trim_start()).map(|python| Some(format!("{indent}{python}")));
    }
    let Some((target, value)) = assigned_escape(body) else {
        return Ok(None);
    };
    let python = match value.strip_prefix('%') {
        Some(magic) => line_magic(magic.trim_start())?,
        None => "pass".to_owned(),
    };
    Ok(Some(format!("{indent}{target} = ...; {python}")))
}

/// `x = !cmd` or `x = %magic`: the assigned names and the escaped value.
/// `IPython` accepts only names and attributes on the left.
fn assigned_escape(body: &str) -> Option<(&str, &str)> {
    let (target, value) = body.split_once('=')?;
    let value = value.trim_start();
    let target = target.trim_end();
    let names = !target.is_empty()
        && target
            .chars()
            .all(|char| char.is_alphanumeric() || matches!(char, '_' | '.' | ',' | ' ' | '\t'));
    (names && (value.starts_with('!') || value.starts_with('%'))).then_some((target, value))
}

/// `obj?`, `obj??`, `?obj`: help on a name, which runs nothing.
fn is_help(body: &str) -> bool {
    let body = body.trim_end();
    if !body.starts_with('?') && !body.ends_with('?') {
        return false;
    }
    let name = body.trim_matches('?').trim_start_matches('%');
    !name.is_empty()
        && name
            .chars()
            .all(|char| char.is_alphanumeric() || matches!(char, '_' | '.' | '*'))
}

/// The Python a line magic (without its `%`) stands for.
fn line_magic(magic: &str) -> Result<String, String> {
    let (name, args) = split_magic(magic);
    match name {
        "load_ext" | "reload_ext" => Ok(extension_import(args)),
        "aimport" => Ok(aimport(args)),
        "time" => Ok(statement(args, &[])),
        "timeit" => Ok(statement(args, TIMEIT_VALUED)),
        "prun" => Ok(statement(args, PRUN_VALUED)),
        _ if INERT_LINE_MAGICS.contains(&name) => Ok("pass".to_owned()),
        _ => Err(format!("line magic `%{name}` is not recognized")),
    }
}

/// A magic's name and its arguments.
fn split_magic(magic: &str) -> (&str, &str) {
    magic
        .split_once(char::is_whitespace)
        .map_or((magic, ""), |(name, args)| (name, args.trim()))
}

fn extension_import(module: &str) -> String {
    if BUILTIN_EXTENSIONS.contains(&module) || !is_dotted_identifier(module) {
        // A name that is not a module path fails to load, importing nothing.
        return "pass".to_owned();
    }
    format!("import {module}")
}

/// `%aimport a, b` imports the modules; `%aimport -a` only excludes `a`
/// from reloading, importing nothing.
fn aimport(args: &str) -> String {
    if args.starts_with('-') {
        return "pass".to_owned();
    }
    let modules: Vec<&str> = args
        .split(',')
        .map(str::trim)
        .filter(|module| is_dotted_identifier(module))
        .collect();
    if modules.is_empty() {
        return "pass".to_owned();
    }
    format!("import {}", modules.join(", "))
}

/// The statement a timing magic runs, after its options. `valued` names the
/// options that take a value, which may follow as its own word.
fn statement(args: &str, valued: &[&str]) -> String {
    let mut rest = args;
    while let Some(option) = rest.strip_prefix('-') {
        let (word, after) = option
            .split_once(char::is_whitespace)
            .map_or((option, ""), |(word, after)| (word, after.trim_start()));
        rest = after;
        if valued.contains(&word) {
            rest = rest
                .split_once(char::is_whitespace)
                .map_or("", |(_, after)| after.trim_start());
        }
    }
    if rest.is_empty() { "pass" } else { rest }.to_owned()
}

fn blank_line(line: &str, out: &mut String) {
    let text = line.trim_end_matches(['\n', '\r']);
    out.push_str(&line[text.len()..]);
}

#[cfg(test)]
mod tests {
    use super::cell_to_python;

    fn unrecognized(line: usize, message: &str) -> Result<String, (usize, String)> {
        Err((line, message.to_owned()))
    }

    #[test]
    fn shell_help_and_inert_magics_become_pass() {
        let cell = "% pip install requests\n!ls -la \\\n  /tmp\nfiles = !ls\nfor x in y:\n    !echo $x\nobj?\n?obj.attr\n%matplotlib inline\nimport os\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok(
                "pass\npass\n\nfiles = ...; pass\nfor x in y:\n    pass\npass\npass\npass\nimport os\n"
            )
        );
    }

    #[test]
    fn magics_that_run_python_keep_it() {
        let cell = "%time import numpy\n%timeit -n 10 -r3 -o pandas.read_csv(path)\n%prun -l 5 run()\n%timeit -q\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok("import numpy\npandas.read_csv(path)\nrun()\npass\n")
        );
        assert_eq!(
            cell_to_python("\n%%time\nimport torch\n!nvidia-smi\n").as_deref(),
            Ok("\n\nimport torch\npass\n")
        );
        assert_eq!(
            cell_to_python("%%script python3\nimport rich\n").as_deref(),
            Ok("\nimport rich\n")
        );
        assert_eq!(
            cell_to_python("%%script --out res python3\nimport rich\n").as_deref(),
            Ok("\nimport rich\n")
        );
        assert_eq!(
            cell_to_python("%%timeit -n 1 import pandas as pd\npd.read_csv(path)\n").as_deref(),
            Ok("import pandas as pd\npd.read_csv(path)\n")
        );
        assert_eq!(
            cell_to_python("t = %time import foo\n").as_deref(),
            Ok("t = ...; import foo\n")
        );
        assert_eq!(
            cell_to_python("%%prun -l 5 run()\nfinish()\n").as_deref(),
            Ok("run()\nfinish()\n")
        );
        assert_eq!(
            cell_to_python("%%script --bg python3\nimport rich\n").as_deref(),
            Ok("\nimport rich\n")
        );
    }

    #[test]
    fn escapes_assign_only_to_names() {
        for cell in ["x[0] = !ls\n", "= !ls\n"] {
            assert_eq!(cell_to_python(cell).as_deref(), Ok(cell));
        }
    }

    #[test]
    fn help_is_only_on_names() {
        for cell in ["f(x)?\n", "??\n"] {
            assert_eq!(cell_to_python(cell).as_deref(), Ok(cell));
        }
    }

    #[test]
    fn continued_lines_join_before_the_rewrite() {
        assert_eq!(
            cell_to_python("%load_ext \\\n  foo\n").as_deref(),
            Ok("import foo\n\n")
        );
        let cell = "x = 1 + \\\n  2\n!ls\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok("x = 1 + \\\n  2\npass\n")
        );
        let cell = "# C:\\data\\\n%matplotlib inline\n";
        assert_eq!(cell_to_python(cell).as_deref(), Ok("# C:\\data\\\npass\n"));
    }

    #[test]
    fn lines_inside_brackets_and_strings_continue_their_statement() {
        let cell = "%matplotlib inline\ntotal = (a\n         % b)\ndoc = \"\"\"\n!important\n\"\"\"\ny = (a\n     # note\n     % b)\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok(
                "pass\ntotal = (a\n         % b)\ndoc = \"\"\"\n!important\n\"\"\"\ny = (a\n     # note\n     % b)\n"
            )
        );
        assert_eq!(
            cell_to_python("%%time\nx = [1,\n     !ls]\n!ls\n").as_deref(),
            Ok("\nx = [1,\n     !ls]\npass\n")
        );
    }

    #[test]
    fn escapes_do_not_open_brackets_or_strings() {
        let cell = "!echo ( \"\"\"\n# done\n\n%pip install x\nx = !echo [\n%ls\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok("pass\n# done\n\npass\nx = ...; pass\npass\n")
        );
    }

    #[test]
    fn aimport_imports_named_modules() {
        assert_eq!(
            cell_to_python("%aimport a.b, c\n").as_deref(),
            Ok("import a.b, c\n")
        );
    }

    #[test]
    fn magics_that_load_a_module_import_it() {
        let cell = "%load_ext ngql\n%load_ext autoreload\n%reload_ext google.colab.data_table\n%aimport -skip a.b\n%aimport\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok("import ngql\npass\nimport google.colab.data_table\npass\npass\n")
        );
    }

    #[test]
    fn foreign_cell_magics_drop_the_whole_cell() {
        assert_eq!(
            cell_to_python("%%bash\nimport os\npip install x\n").as_deref(),
            Ok("\n\n\n")
        );
        assert_eq!(
            cell_to_python("%%script bash\necho $HOME").as_deref(),
            Ok("\n")
        );
    }

    #[test]
    fn unrecognized_forms_name_the_magic() {
        assert_eq!(
            cell_to_python("\n%%writefile app.py\nimport streamlit\n"),
            unrecognized(1, "cell magic `%%writefile` is not recognized")
        );
        assert_eq!(
            cell_to_python("import os\n%run helper.py\n"),
            unrecognized(1, "line magic `%run` is not recognized")
        );
        assert_eq!(
            cell_to_python("x = %wget url\n"),
            unrecognized(0, "line magic `%wget` is not recognized")
        );
        assert_eq!(
            cell_to_python("%%script --out res\nimport os\n"),
            unrecognized(0, "cell magic `%%script` names no program")
        );
        assert_eq!(
            cell_to_python("%%time\nimport os\n%run helper.py\n"),
            unrecognized(2, "line magic `%run` is not recognized")
        );
        assert_eq!(
            cell_to_python("import os\n%%time\n"),
            unrecognized(1, "cell magic `%%time` is not on the cell's first line")
        );
    }

    #[test]
    fn help_after_unbalanced_brackets_does_not_panic() {
        assert_eq!(
            cell_to_python("y = (1,\nb]\n?obj.attr\n").as_deref(),
            Ok("y = (1,\nb]\npass\n")
        );
    }

    #[test]
    fn plain_python_is_left_alone() {
        let cell = "x = 5 % 3\nif x == !y: pass\nprint('why?')\nd = {'a': 1}\n";
        assert_eq!(cell_to_python(cell).as_deref(), Ok(cell));
    }

    mod props {
        use super::cell_to_python;
        use super::super::{FOREIGN_CELL_MAGICS, INERT_LINE_MAGICS, PYTHON_CELL_MAGICS};
        use proptest::prelude::*;

        /// Lines with no `%`, `!` or `?`, so nothing in them is an escape.
        /// Some open a bracket, a string or a `\` continuation that the next
        /// line closes or not.
        const PLAIN: &[&str] = &[
            "",
            "   ",
            "import os",
            "x = 1",
            "def f(a):",
            "    return a",
            "if x:",
            "    pass",
            "y = (1,",
            "     2)",
            "s = \"\"\"",
            "text",
            "\"\"\"",
            "# comment",
            "z = [a,",
            "b]",
            "w = 1 + \\",
            "  2",
            "print('hi')",
            "d = {'a': 1}",
            "x = 5 * 3",
        ];

        /// Complete statements: each line starts one, so an escape on the
        /// line after any of them is read.
        const STATEMENTS: &[&str] = &["", "import os", "x = 1", "# comment", "pass", "print(1)"];

        /// `IPython` forms the rewrite recognizes, mixed with plain lines.
        const IPYTHON: &[&str] = &[
            "%time import numpy",
            "%timeit -n 10 -r3 f(x)",
            "%prun -l 5 run()",
            "% pip install requests",
            "%matplotlib inline",
            "%load_ext foo.bar",
            "%aimport a.b, c",
            "!ls -la",
            "!echo ( \"\"\"",
            "files = !ls",
            "t = %time import foo",
            "obj?",
            "?obj.attr",
            "    !echo $x",
            "%load_ext \\",
            "!ls \\",
            "%%time",
            "%%bash",
            "%%script python3",
        ];

        fn cell(pool: &'static [&'static str], ending: bool) -> impl Strategy<Value = String> {
            prop::collection::vec(prop::sample::select(pool), 0..10).prop_map(move |lines| {
                let mut cell = lines.join("\n");
                if ending && !lines.is_empty() {
                    cell.push('\n');
                }
                cell
            })
        }

        fn mixed_cell() -> impl Strategy<Value = String> {
            let pool: Vec<&'static str> = PLAIN.iter().chain(IPYTHON).copied().collect();
            (prop::collection::vec(prop::sample::select(pool), 0..12), any::<bool>()).prop_map(
                |(lines, ending)| {
                    let mut cell = lines.join("\n");
                    if ending {
                        cell.push('\n');
                    }
                    cell
                },
            )
        }

        /// A magic name none of the tables or rules knows.
        fn unknown_magic() -> impl Strategy<Value = String> {
            "zz[a-z_]{0,8}".prop_filter("a known magic", |name| {
                !INERT_LINE_MAGICS.contains(&name.as_str())
                    && !FOREIGN_CELL_MAGICS.contains(&name.as_str())
                    && !PYTHON_CELL_MAGICS.contains(&name.as_str())
            })
        }

        proptest! {
            /// The rewrite keeps every line break where it was, so the
            /// notebook's line numbers hold for whatever it reads.
            #[test]
            fn rewrite_keeps_the_line_count(cell in mixed_cell()) {
                if let Ok(python) = cell_to_python(&cell) {
                    prop_assert_eq!(
                        python.matches('\n').count(),
                        cell.matches('\n').count(),
                        "{:?} -> {:?}", cell, python
                    );
                }
            }

            /// A cell without magics, shell escapes or help is left as is.
            #[test]
            fn plain_cells_are_unchanged(cell in cell(PLAIN, true)) {
                prop_assert_eq!(cell_to_python(&cell), Ok(cell));
            }

            /// A line magic nothing recognizes fails the cell at its line,
            /// whatever plain statements surround it (ADR 0005).
            #[test]
            fn unknown_line_magics_fail_at_their_line(
                before in prop::collection::vec(prop::sample::select(STATEMENTS), 0..5),
                after in prop::collection::vec(prop::sample::select(STATEMENTS), 0..5),
                name in unknown_magic(),
                assigned in any::<bool>(),
            ) {
                let magic = if assigned { format!("x = %{name} arg") } else { format!("%{name} arg") };
                let lines: Vec<&str> = before
                    .iter()
                    .copied()
                    .chain([magic.as_str()])
                    .chain(after.iter().copied())
                    .collect();
                let cell = format!("{}\n", lines.join("\n"));
                prop_assert_eq!(
                    cell_to_python(&cell),
                    Err((before.len(), format!("line magic `%{name}` is not recognized")))
                );
            }

            /// So does a cell magic nothing recognizes, on the cell's first
            /// line after any blank ones.
            #[test]
            fn unknown_cell_magics_fail_the_cell(
                blank in 0usize..3,
                body in cell(PLAIN, true),
                name in unknown_magic(),
            ) {
                let cell = format!("{}%%{name} arg\n{body}", "\n".repeat(blank));
                prop_assert_eq!(
                    cell_to_python(&cell),
                    Err((blank, format!("cell magic `%%{name}` is not recognized")))
                );
            }

            /// No input panics the rewrite.
            #[test]
            fn rewrite_never_panics(cell in "\\PC{0,200}") {
                let _ = cell_to_python(&cell);
            }

            #[test]
            fn rewrite_never_panics_on_ipython_lines(
                cell in mixed_cell(),
                cr in any::<bool>(),
            ) {
                let cell = if cr { cell.replace('\n', "\r\n") } else { cell };
                let _ = cell_to_python(&cell);
            }
        }
    }
}
