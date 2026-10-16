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

use super::dynamic::is_dotted_identifier;

/// Line magics that neither run the cell's Python nor import a module, so
/// dropping them hides nothing a rule reads. `%pip` installs a distribution
/// but does not import it; whether its arguments declare dependencies is
/// left to #962's follow-up.
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

/// Cell magics that run their body as Python.
const PYTHON_CELL_MAGICS: &[&str] = &[
    "capture", "prun", "pypy", "python", "python2", "python3", "time", "timeit",
];

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
        CellBody::Python => {
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
    Python,
    Foreign,
}

fn cell_magic(line: &str) -> Result<CellBody, String> {
    let mut words = line.split_whitespace();
    let name = words.next().unwrap_or_default();
    if PYTHON_CELL_MAGICS.contains(&name) {
        return Ok(CellBody::Python);
    }
    if FOREIGN_CELL_MAGICS.contains(&name) {
        return Ok(CellBody::Foreign);
    }
    // `%%script python3` runs Python like `%%python3`.
    if name == "script" {
        let program = words
            .find(|word| !word.starts_with('-'))
            .unwrap_or_default();
        let program = program.rsplit(['/', '\\']).next().unwrap_or_default();
        return Ok(
            if program.starts_with("python") || program.starts_with("pypy") {
                CellBody::Python
            } else {
                CellBody::Foreign
            },
        );
    }
    Err(format!("cell magic `%%{name}` is not recognized"))
}

/// Rewrite each line on its own, the first being the cell's line `start`.
/// A magic or shell line ending in `\` continues onto the next, which goes
/// with it.
fn rewrite_lines(lines: &[&str], start: usize, out: &mut String) -> Result<(), (usize, String)> {
    let mut continued = false;
    for (index, line) in lines.iter().copied().enumerate() {
        let text = line.trim_end_matches(['\n', '\r']);
        if continued {
            blank_line(line, out);
        } else {
            let python = rewrite_line(text).map_err(|message| (start + index, message))?;
            let Some(python) = python else {
                out.push_str(line);
                continue;
            };
            out.push_str(&python);
            out.push_str(&line[text.len()..]);
        }
        continued = text.ends_with('\\');
    }
    Ok(())
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
    if let Some(magic) = value.strip_prefix('%') {
        line_magic(magic.trim_start())?;
    }
    Ok(Some(format!("{indent}{target} = ...")))
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
    let (name, args) = magic
        .split_once(char::is_whitespace)
        .map_or((magic, ""), |(name, args)| (name, args.trim()));
    match name {
        "load_ext" | "reload_ext" => Ok(extension_import(args)),
        "aimport" => Ok(aimport(args)),
        "time" => Ok(statement(args, &[])),
        "timeit" => Ok(statement(args, &["n", "r", "p"])),
        "prun" => Ok(statement(args, &["D", "T", "s", "l"])),
        _ if INERT_LINE_MAGICS.contains(&name) => Ok("pass".to_owned()),
        _ => Err(format!("line magic `%{name}` is not recognized")),
    }
}

fn extension_import(module: &str) -> String {
    if BUILTIN_EXTENSIONS.contains(&module) || !is_dotted_identifier(module) {
        // A name that is not a module path fails to load, importing nothing.
        return "pass".to_owned();
    }
    format!("import {module}")
}

/// `%aimport a, b` imports the modules; `-a` excludes one from reloading.
fn aimport(args: &str) -> String {
    let modules: Vec<&str> = args
        .split([',', ' ', '\t'])
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
            Ok("pass\npass\n\nfiles = ...\nfor x in y:\n    pass\npass\npass\npass\nimport os\n")
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
    }

    #[test]
    fn magics_that_load_a_module_import_it() {
        let cell = "%load_ext ngql\n%load_ext autoreload\n%reload_ext google.colab.data_table\n%aimport -skip a.b, c\n%aimport\n";
        assert_eq!(
            cell_to_python(cell).as_deref(),
            Ok("import ngql\npass\nimport google.colab.data_table\nimport a.b, c\npass\n")
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
            cell_to_python("import os\n%%time\n"),
            unrecognized(1, "cell magic `%%time` is not on the cell's first line")
        );
    }

    #[test]
    fn plain_python_is_left_alone() {
        let cell = "x = 5 % 3\nif x == !y: pass\nprint('why?')\nd = {'a': 1}\n";
        assert_eq!(cell_to_python(cell).as_deref(), Ok(cell));
    }
}
