//! Property-based test that `--fix --allow-remove-files` keeps the program
//! running (#1114): `CPython` runs the generated entry before and after the
//! fix, and it must import the same project files both times, with no
//! `ImportError` after.
//!
//! Only the code this test writes is run, never an analyzed project's.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

const MODULES: usize = 4;

/// How one file imports `pkg.m<i>`.
#[derive(Debug, Clone, Copy)]
enum Form {
    FromPackage,
    Dotted,
    /// `from . import m<i>`, inside the package only.
    Relative,
    /// `importlib.import_module("pkg.m<i>")`.
    Dynamic,
}

#[derive(Debug, Clone)]
struct Spec {
    /// Whether `pkg/__init__.py` exists; without it `pkg` is a namespace
    /// package.
    package_init: bool,
    /// What the root `main.py` imports.
    main: Vec<(usize, Form)>,
    /// What each `pkg/m<i>.py` imports.
    modules: Vec<Vec<(usize, Form)>>,
}

fn edges() -> impl Strategy<Value = Vec<(usize, Form)>> {
    prop::collection::vec(
        (
            0..MODULES,
            prop::sample::select(vec![
                Form::FromPackage,
                Form::Dotted,
                Form::Relative,
                Form::Dynamic,
            ]),
        ),
        0..3,
    )
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (
        any::<bool>(),
        edges(),
        prop::collection::vec(edges(), MODULES),
    )
        .prop_map(|(package_init, main, modules)| Spec {
            package_init,
            main,
            modules,
        })
}

fn import_lines(edges: &[(usize, Form)], in_package: bool) -> String {
    let mut text = String::new();
    for (index, form) in edges {
        match form {
            Form::Relative if in_package => writeln!(text, "from . import m{index}"),
            Form::FromPackage | Form::Relative => writeln!(text, "from pkg import m{index}"),
            Form::Dotted => writeln!(text, "import pkg.m{index}"),
            Form::Dynamic => writeln!(
                text,
                "import importlib\nimportlib.import_module(\"pkg.m{index}\")"
            ),
        }
        .expect("fmt");
    }
    text
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

/// A flat-layout app whose root `main.py` runs as a script (#1137).
fn project(spec: &Spec) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write(
        root,
        "pyproject.toml",
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"app\"\n",
    );
    write(
        root,
        "main.py",
        &format!(
            "{}\nif __name__ == \"__main__\":\n    pass\n",
            import_lines(&spec.main, false)
        ),
    );
    if spec.package_init {
        write(root, "pkg/__init__.py", "");
    }
    for (index, edges) in spec.modules.iter().enumerate() {
        write(
            root,
            &format!("pkg/m{index}.py"),
            &import_lines(edges, true),
        );
    }
    dir
}

/// Runs `main.py` as `__main__` in `CPython` and returns the project files it
/// imported, root-relative, or the error output when it failed.
fn loaded_files(root: &Path) -> Result<BTreeSet<String>, String> {
    const DRIVER: &str = r#"
import os, runpy, sys
root = os.path.realpath(sys.argv[1])
sys.path.insert(0, root)
runpy.run_path(os.path.join(root, "main.py"), run_name="__main__")
for module in list(sys.modules.values()):
    path = getattr(module, "__file__", None)
    if path and os.path.realpath(path).startswith(root + os.sep):
        print(os.path.relpath(os.path.realpath(path), root).replace(os.sep, "/"))
"#;
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = Command::new(python)
        .args(["-I", "-B", "-c", DRIVER])
        .arg(root)
        .output()
        .expect("run CPython");
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The files in `pkg/`, root-relative; `main.py` is the entry.
fn package_files(root: &Path) -> BTreeSet<String> {
    fs::read_dir(root.join("pkg"))
        .expect("read pkg")
        .map(|entry| {
            let name = entry.expect("read_dir").file_name();
            format!("pkg/{}", name.to_string_lossy())
        })
        .collect()
}

/// Runs `--fix --allow-remove-files` and returns the files it reports
/// removing, read from the `applied CHK001` lines on stderr.
fn remove_files(root: &Path) -> BTreeSet<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--fix", "--allow-remove-files", "--no-cache"])
        .current_dir(root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {stderr}"
    );
    stderr
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("applied CHK001 "))
        .filter_map(|rest| rest.split_whitespace().next().map(str::to_owned))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// The entry imports the same files after the removal as before it.
    #[test]
    fn file_removal_keeps_what_the_entry_imports(spec in spec_strategy()) {
        let dir = project(&spec);
        let root = dir.path();
        let before = loaded_files(root).expect("the generated program runs");
        let files = package_files(root);
        let removed = remove_files(root);
        let gone: BTreeSet<String> = files.difference(&package_files(root)).cloned().collect();
        prop_assert_eq!(&removed, &gone, "the reported removals are the files gone");
        let after = loaded_files(root);
        prop_assert_eq!(
            after.as_ref(),
            Ok(&before),
            "removed {:?} from {:#?}",
            removed,
            spec
        );
    }
}

/// The property holds trivially when nothing is removed: `pkg/m1.py`, which
/// nothing imports, must go.
#[test]
fn file_removal_removes_an_unreached_module() {
    let spec = Spec {
        package_init: true,
        main: vec![(0, Form::FromPackage)],
        modules: vec![Vec::new(); MODULES],
    };
    let dir = project(&spec);
    let removed = remove_files(dir.path());
    assert!(removed.contains("pkg/m1.py"), "removed {removed:?}");
}
