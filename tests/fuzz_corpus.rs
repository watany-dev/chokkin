//! Deterministic fuzz corpus: adversarial inputs through the public API.
//!
//! Complements the proptest suites with handpicked nasty inputs (deep
//! nesting, control characters, huge lines, Unicode edge cases) that have
//! historically broken hand-rolled parsers. Every case must complete without
//! panicking; specific cases additionally assert Ok/Err behavior.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::Path;

use yokei::{
    ConfigSources, EntrySpec, LoadedConfig, ProjectRoot, RootMarker, TargetVersion, default_config,
    discover_sources, extract_manifest, load_config,
};

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

fn default_loaded_config(root: &ProjectRoot) -> LoadedConfig {
    LoadedConfig {
        root: root.clone(),
        effective: default_config(),
        sources: ConfigSources {
            used_defaults: true,
            dot_yokei_toml: None,
            yokei_toml: None,
            pyproject_tool_yokei: false,
        },
        uv_workspace: None,
    }
}

/// Adversarial text payloads reused across all file-based corpus runs.
fn corpus() -> Vec<String> {
    let mut cases = vec![
        String::new(),
        "\u{FEFF}".to_owned(),                               // BOM only
        "\u{FEFF}requests>=2.0".to_owned(),                  // BOM prefix
        "\0".to_owned(),                                     // NUL byte
        "pkg\0name".to_owned(),                              // embedded NUL
        "\r\n\r\n\r\n".to_owned(),                           // CRLF storm
        "requests>=2.0\r\nflask\r\n".to_owned(),             // CRLF line endings
        "  \t  \t  ".to_owned(),                             // whitespace only
        "#".to_owned(),                                      // bare comment
        "# comment\n#comment\n #x".to_owned(),               // comment variants
        "-".to_owned(),                                      // bare dash
        "-e".to_owned(),                                     // bare editable flag
        "-e .".to_owned(),                                   // editable current dir
        "-r".to_owned(),                                     // bare include flag (missing path)
        "--requirement=".to_owned(),                         // empty include path
        "-c missing-constraints.txt".to_owned(),             // missing constraint file
        "pkg @ file:///etc/passwd".to_owned(),               // direct file URL
        "git+ssh://git@host/repo.git#egg=".to_owned(),       // empty egg
        "x#egg=&egg=second".to_owned(),                      // duplicated egg params
        "pkg[".to_owned(),                                   // unterminated extras
        "pkg[a,".to_owned(),                                 // dangling extras comma
        "pkg]extra[".to_owned(),                             // reversed brackets
        "pkg==1.0; python_version >= '3.8' and ".to_owned(), // dangling marker
        "ＰＫＧ>=1.0".to_owned(),                            // fullwidth letters
        "пакет>=1.0".to_owned(),                             // Cyrillic name
        "🦀>=1.0".to_owned(),                                // emoji name
        "pkg\u{202E}gnp".to_owned(),                         // RTL override
        "\"'\"'\"'\"'".to_owned(),                           // quote storm
        "=".repeat(64),                                      // assignment storm
        "[".repeat(2_000),                                   // deep open brackets
        "(".repeat(2_000),                                   // deep open parens
        "{".repeat(2_000),                                   // deep open braces
        ")".repeat(2_000),                                   // unbalanced closers
        "a".repeat(100_000),                                 // single huge token
        format!("requests{}", " ".repeat(50_000)),           // trailing space flood
        format!("setup({}", "name=\"x\",".repeat(5_000)),    // huge unterminated call
    ];
    cases.push(format!(
        "setup(install_requires=[{}])",
        "\"a\",".repeat(10_000)
    ));
    cases.push(format!("[{}]]", "section.".repeat(1_000)));
    // Lockfile-shaped adversaries: package array floods and type confusion.
    cases.push("[[package]]\n".repeat(2_000));
    cases.push(format!(
        "[[package]]\nname = \"a\"\ndependencies = [{}]",
        "\"b\",".repeat(5_000)
    ));
    cases.push("package = \"not-an-array\"\nrequires-python = 3".to_owned());
    cases.push("[[package]]\nname = 42\ndependencies = [1, [], {}]\n".to_owned());
    cases
}

#[test]
fn requirements_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("requirements.txt"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let _ = extract_manifest(&root, &config);
    }
}

#[test]
fn pyproject_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("pyproject.toml"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let _ = extract_manifest(&root, &config);
        let _ = load_config(&root);
    }
}

#[test]
fn setup_py_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("setup.py"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let _ = extract_manifest(&root, &config);
    }
}

#[test]
fn setup_cfg_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("setup.cfg"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let _ = extract_manifest(&root, &config);
    }
}

#[test]
fn uv_lock_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\n",
        )
        .expect("write");
        fs::write(temp.path().join("uv.lock"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let _ = extract_manifest(&root, &config);
    }
}

#[test]
fn standalone_config_corpus_never_panics() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join(".yokei.toml"), &payload).expect("write");
        let root = project_root_at(temp.path());
        let _ = load_config(&root);
    }
}

#[test]
fn gitignore_corpus_never_breaks_discovery() {
    for payload in corpus() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"acme\"\n",
        )
        .expect("write pyproject");
        fs::create_dir_all(temp.path().join("src/acme")).expect("mkdir");
        fs::write(temp.path().join("src/acme/__init__.py"), "").expect("write init");
        fs::write(temp.path().join(".gitignore"), &payload).expect("write gitignore");

        let root = project_root_at(temp.path());
        let config = default_loaded_config(&root);
        let manifest = extract_manifest(&root, &config).expect("manifest");
        let _ = discover_sources(&root, &config, &manifest);
    }
}

#[test]
fn entry_spec_corpus_never_panics() {
    let cases = [
        "",
        ":",
        "::",
        ":::",
        "a:",
        ":a",
        ".",
        "..",
        ".:.",
        "..:..",
        "C:\\windows\\path.py",
        "C:/windows/path.py",
        "a:b:c:d:e",
        "🦀.py:app",
        "src/app.py:アプリ",
        "\0:\0",
        "py:",
        ":py",
    ];
    for case in cases {
        if let Ok(spec) = EntrySpec::parse(case) {
            assert!(!spec.path.is_empty(), "case {case:?} produced empty path");
        }
    }

    let long = "a".repeat(100_000);
    let _ = EntrySpec::parse(&long);
    let _ = EntrySpec::parse(&format!("{long}:{long}"));
}

#[test]
fn target_version_corpus_never_panics() {
    let cases = [
        "",
        "py",
        "py3",
        "py3.",
        "py3x",
        "py311",
        "py3111",
        "py31111",
        "PY311",
        "ｐｙ３１１",
        "py3\u{0660}\u{0660}", // Arabic-Indic digits are not ASCII digits
        "py3-1",
        " py311",
        "py311 ",
    ];
    for case in cases {
        let _ = TargetVersion::parse(case);
    }
    let _ = TargetVersion::parse(&"py3".repeat(10_000));
}

#[test]
fn deep_include_chain_does_not_overflow_stack() {
    let temp = tempfile::tempdir().expect("tempdir");
    let depth = 200;
    for index in 0..depth {
        let next = index + 1;
        let contents = if next < depth {
            format!("-r req-{next}.txt\n")
        } else {
            "requests\n".to_owned()
        };
        let filename = if index == 0 {
            "requirements.txt".to_owned()
        } else {
            format!("req-{index}.txt")
        };
        fs::write(temp.path().join(filename), contents).expect("write");
    }

    let root = project_root_at(temp.path());
    let config = default_loaded_config(&root);
    let manifest = extract_manifest(&root, &config).expect("deep chain resolves");
    assert_eq!(manifest.dependencies.len(), 1);
    assert_eq!(manifest.sources.requirements_files.len(), depth);
}
