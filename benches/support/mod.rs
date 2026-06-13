//! Synthetic project fixtures shared by the benchmark targets.
//!
//! Fixtures are generated once per benchmark setup (outside the measured
//! loop) inside a [`tempfile::TempDir`], so benchmark iterations only pay
//! for the pipeline step under test.

// Each bench target compiles this module independently, so helpers used by
// only one target look dead in the other.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// Write `content` to `root/rel`, creating parent directories as needed.
pub fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture dir");
    }
    fs::write(path, content).expect("write fixture file");
}

fn module_body(index: u64) -> String {
    format!("\"\"\"Module {index}.\"\"\"\n\n\ndef func_{index}() -> int:\n    return {index}\n")
}

/// A src-layout project with `n_files` Python modules spread over
/// subpackages, plus tests, scripts, docs, and a `.gitignore`.
pub fn synth_src_project(n_files: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    write(
        root,
        "pyproject.toml",
        concat!(
            "[project]\n",
            "name = \"bench-acme\"\n",
            "version = \"0.1.0\"\n",
            "dependencies = [\"requests>=2\"]\n",
        ),
    );
    write(root, ".gitignore", "ignored/\n*.tmp\n");
    write(root, "src/bench_acme/__init__.py", "");
    for index in 0..n_files {
        let sub = index / 50;
        write(
            root,
            &format!("src/bench_acme/sub_{sub}/mod_{index}.py"),
            &module_body(index),
        );
    }
    for index in 0..(n_files / 20).max(1) {
        write(
            root,
            &format!("tests/test_mod_{index}.py"),
            &module_body(index),
        );
    }
    write(root, "scripts/run.py", "print('run')\n");
    write(root, "docs/conf.py", "project = 'bench'\n");
    write(root, "ignored/skipme.py", "x = 1\n");
    temp
}

/// A flat-layout project: one package directory with `n_files` modules.
/// Exercises the per-file flat-package prefix check in context assignment.
pub fn synth_flat_project(n_files: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    write(
        root,
        "pyproject.toml",
        concat!(
            "[project]\n",
            "name = \"bench-flat\"\n",
            "version = \"0.1.0\"\n",
        ),
    );
    write(root, "bench_flat/__init__.py", "");
    for index in 0..n_files {
        let sub = index / 50;
        write(
            root,
            &format!("bench_flat/sub_{sub}/mod_{index}.py"),
            &module_body(index),
        );
    }
    temp
}

/// A project declaring dependencies only through `requirements.txt`
/// (`n_lines` requirement lines, comments, markers, extras, and one
/// recursive `-r` include).
pub fn synth_requirements_project(n_lines: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    let mut main = String::from("# generated benchmark fixture\n-r requirements-extra.txt\n");
    for index in 0..n_lines {
        match index % 4 {
            0 => writeln!(main, "pkg-{index}>=1.0"),
            1 => writeln!(main, "pkg-{index}[extra1,extra2]==2.{index}"),
            2 => writeln!(main, "pkg-{index}>=1.{index} ; python_version >= \"3.10\""),
            _ => writeln!(main, "# comment line {index}"),
        }
        .expect("write requirement line");
    }
    write(root, "requirements.txt", &main);

    let mut extra = String::new();
    for index in 0..(n_lines / 4) {
        writeln!(extra, "extra-pkg-{index}>=0.{index}").expect("write extra line");
    }
    write(root, "requirements-extra.txt", &extra);
    temp
}

/// A project whose dependencies come from a statically parseable
/// `setup.py` with `n_deps` entries in `install_requires`.
pub fn synth_setup_py_project(n_deps: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    let mut body = String::from(
        "from setuptools import setup\n\nsetup(\n    name=\"bench-acme\",\n    version=\"1.0.0\",\n    install_requires=[\n",
    );
    for index in 0..n_deps {
        writeln!(body, "        \"pkg-{index}>=1.{index}\",").expect("write setup.py dep");
    }
    body.push_str("    ],\n)\n");
    write(root, "setup.py", &body);
    temp
}

/// A project whose dependencies come from a `setup.cfg` with `n_deps`
/// `install_requires` lines.
pub fn synth_setup_cfg_project(n_deps: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    let mut body = String::from(
        "[metadata]\nname = bench-acme\nversion = 1.0.0\n\n[options]\ninstall_requires =\n",
    );
    for index in 0..n_deps {
        writeln!(body, "    pkg-{index}>=1.{index}").expect("write setup.cfg dep");
    }
    body.push_str("\n[options.extras_require]\ndocs =\n    sphinx>=7\n");
    write(root, "setup.cfg", &body);
    temp
}

/// A pyproject-based project with `n_deps` runtime dependencies plus
/// optional-dependency groups and console entry points.
pub fn synth_pyproject_project(n_deps: u64) -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    let mut body =
        String::from("[project]\nname = \"bench-acme\"\nversion = \"0.1.0\"\ndependencies = [\n");
    for index in 0..n_deps {
        writeln!(body, "    \"pkg-{index}>=1.{index}\",").expect("write pyproject dep");
    }
    body.push_str("]\n\n[project.optional-dependencies]\n");
    for group in ["docs", "test", "lint"] {
        writeln!(body, "{group} = [").expect("write group header");
        for index in 0..(n_deps / 10).max(1) {
            writeln!(body, "    \"{group}-pkg-{index}>=0.{index}\",").expect("write group dep");
        }
        body.push_str("]\n");
    }
    body.push_str("\n[project.scripts]\n");
    for index in 0..20u64 {
        writeln!(body, "cli-{index} = \"bench_acme.mod_{index}:main\"").expect("write script");
    }
    write(root, "pyproject.toml", &body);
    temp
}
