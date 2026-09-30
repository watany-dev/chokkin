//! Property-based tests for parser helpers (ignore directives, `LineIndex` via
//! `parse_file`).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fmt::Write as _;

use chokkin::extract_ignores;
use proptest::prelude::*;

fn code() -> impl Strategy<Value = String> {
    (0u32..1000).prop_map(|n| format!("CHK{n:03}"))
}

proptest! {
    /// Arbitrary text never panics and every emitted code is `CHKnnn`.
    #[test]
    fn extract_ignores_total_on_arbitrary_text(source in "(?s).{0,300}") {
        for directive in extract_ignores(&source) {
            prop_assert!(!directive.codes.is_empty());
            for code in &directive.codes {
                prop_assert!(code.len() == 6 && code.starts_with("CHK"));
                prop_assert!(code[3..].chars().all(|c| c.is_ascii_digit()));
            }
        }
    }

    /// Inline directive reports the 1-based line it is written on.
    #[test]
    fn inline_directive_line_matches_position(
        pad in 0usize..6,
        codes in prop::collection::vec(code(), 1..4),
    ) {
        let mut source = String::from("import os\n");
        source.push_str(&"x = 1\n".repeat(pad));
        let _ = writeln!(source, "import sys  # chokkin: ignore[{}]", codes.join(","));
        let found = extract_ignores(&source);
        prop_assert_eq!(found.len(), 1);
        prop_assert!(!found[0].file_level);
        prop_assert_eq!(found[0].line as usize, pad + 2);
        prop_assert_eq!(&found[0].codes, &codes);
    }

    /// Line numbers are independent of the line-ending style.
    #[test]
    fn inline_directive_line_ignores_eol_style(
        pad in 0usize..6,
        eol in prop::sample::select(vec!["\n", "\r\n"]),
    ) {
        let mut source = String::new();
        for _ in 0..pad {
            source.push_str("x = 1");
            source.push_str(eol);
        }
        source.push_str("import sys  # chokkin: ignore[CHK003]");
        source.push_str(eol);
        let found = extract_ignores(&source);
        prop_assert_eq!(found.len(), 1);
        prop_assert_eq!(found[0].line as usize, pad + 1);
    }

    /// A file-level directive in the header comment block is honoured
    /// regardless of the line-ending style and leading blank lines.
    #[test]
    fn file_level_directive_in_header_is_found(
        blanks in 0usize..4,
        comments in 0usize..4,
        eol in prop::sample::select(vec!["\n", "\r\n"]),
        c in code(),
    ) {
        let mut source = String::new();
        for _ in 0..blanks {
            source.push_str(eol);
        }
        for _ in 0..comments {
            source.push_str("# header");
            source.push_str(eol);
        }
        let _ = write!(source, "# chokkin: file-ignore[{c}]");
        source.push_str(eol);
        source.push_str("import os");
        source.push_str(eol);
        let found = extract_ignores(&source);
        prop_assert_eq!(found.len(), 1);
        prop_assert!(found[0].file_level);
        prop_assert_eq!(found[0].line, 0);
    }

    /// A file-level directive after the first statement is never file level.
    #[test]
    fn file_level_directive_after_code_is_dropped(c in code(), pad in 0usize..3) {
        let mut source = String::from("import os\n");
        source.push_str(&"\n".repeat(pad));
        let _ = writeln!(source, "# chokkin: file-ignore[{c}]");
        prop_assert!(extract_ignores(&source).iter().all(|d| !d.file_level));
    }
}

mod parse_file_props {
    use std::fs;

    use chokkin::{
        FileContext, ImportKind, LayoutInfo, ProjectLayout, ProjectRoot, RootMarker, TargetVersion,
        parse_file,
    };
    use proptest::prelude::*;
    use tempfile::TempDir;

    #[derive(Debug, Clone)]
    enum Stmt {
        Import(Vec<String>),
        From {
            module: String,
            names: Vec<String>,
        },
        Relative {
            level: u8,
            module: Option<String>,
            name: String,
        },
    }

    fn segment() -> impl Strategy<Value = String> {
        "zz[a-z]{1,5}"
    }

    fn dotted() -> impl Strategy<Value = String> {
        prop::collection::vec(segment(), 1..4).prop_map(|parts| parts.join("."))
    }

    fn stmt() -> impl Strategy<Value = Stmt> {
        prop_oneof![
            prop::collection::vec(dotted(), 1..3).prop_map(Stmt::Import),
            (dotted(), prop::collection::vec(segment(), 1..3))
                .prop_map(|(module, names)| Stmt::From { module, names }),
            (1u8..4, proptest::option::of(dotted()), segment()).prop_map(
                |(level, module, name)| Stmt::Relative {
                    level,
                    module,
                    name
                }
            ),
        ]
    }

    fn render(stmt: &Stmt) -> String {
        match stmt {
            Stmt::Import(modules) => format!("import {}", modules.join(", ")),
            Stmt::From { module, names } => format!("from {module} import {}", names.join(", ")),
            Stmt::Relative {
                level,
                module,
                name,
            } => format!(
                "from {}{} import {name}",
                ".".repeat(usize::from(*level)),
                module.as_deref().unwrap_or("")
            ),
        }
    }

    /// File lives at `src/acme/sub/mod.py` => containing package `acme.sub`.
    fn expected(stmt: &Stmt) -> Vec<(String, ImportKind)> {
        match stmt {
            Stmt::Import(modules) => modules
                .iter()
                .map(|m| (m.clone(), ImportKind::Import))
                .collect(),
            Stmt::From { module, names } => names
                .iter()
                .map(|_| (module.clone(), ImportKind::ImportFrom))
                .collect(),
            Stmt::Relative {
                level,
                module,
                name,
            } => {
                let package = ["acme", "sub"];
                let keep = package.len().checked_sub(usize::from(*level) - 1);
                match keep {
                    Some(keep) if keep >= 1 => {
                        let mut base = package[..keep].join(".");
                        if let Some(module) = module {
                            base = format!("{base}.{module}");
                            vec![(base, ImportKind::ImportFrom)]
                        } else {
                            vec![(format!("{base}.{name}"), ImportKind::ImportFrom)]
                        }
                    },
                    _ => Vec::new(),
                }
            },
        }
    }

    proptest! {
        /// Generated import statements round-trip through `parse_file`:
        /// module names, kinds, and 1-based lines match a reference model,
        /// for LF and CRLF sources alike.
        #[test]
        fn imports_match_reference_model(
            stmts in prop::collection::vec((stmt(), 0usize..3), 1..8),
            eol in prop::sample::select(vec!["\n", "\r\n"]),
        ) {
            let dir = TempDir::new().expect("tempdir");
            fs::create_dir_all(dir.path().join("src/acme/sub")).expect("mkdir");
            let mut source = String::new();
            let mut lines = Vec::new();
            let mut line_no = 0u32;
            for (stmt, blanks) in &stmts {
                for _ in 0..*blanks {
                    source.push_str(eol);
                    line_no += 1;
                }
                source.push_str(&render(stmt));
                source.push_str(eol);
                line_no += 1;
                lines.push(line_no);
            }
            fs::write(dir.path().join("src/acme/sub/mod.py"), &source).expect("write");

            let root = ProjectRoot {
                path: fs::canonicalize(dir.path()).expect("canon"),
                marker: RootMarker::PyProjectToml,
            };
            let layout = LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
            };
            let parsed = parse_file(
                &root,
                "src/acme/sub/mod.py",
                &layout,
                FileContext::Runtime,
                &TargetVersion::default_py311(),
            )
            .expect("parse");

            let mut want = Vec::new();
            for ((stmt, _), line) in stmts.iter().zip(&lines) {
                for (module, kind) in expected(stmt) {
                    want.push((module, kind, *line));
                }
            }
            // Relative imports beyond the top-level package are dropped
            // (recorded as diagnostics), not guessed.
            let got: Vec<_> = parsed
                .imports
                .iter()
                .filter(|i| !i.module.is_empty())
                .map(|i| (i.module.clone(), i.kind, i.line))
                .collect();
            prop_assert_eq!(got, want);
        }

        /// Arbitrary bytes-as-text never panic the parser.
        #[test]
        fn parse_file_is_total(source in "(?s).{0,200}") {
            let dir = TempDir::new().expect("tempdir");
            fs::write(dir.path().join("m.py"), &source).expect("write");
            let root = ProjectRoot {
                path: fs::canonicalize(dir.path()).expect("canon"),
                marker: RootMarker::PyProjectToml,
            };
            let layout = LayoutInfo {
                layout: ProjectLayout::Flat,
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
            };
            let parsed = parse_file(&root, "m.py", &layout, FileContext::Runtime, &TargetVersion::default_py311())
                .expect("parse");
            let line_count = u32::try_from(source.lines().count() + 2).unwrap_or(u32::MAX);
            for import in &parsed.imports {
                prop_assert!(import.line >= 1 && import.line <= line_count);
            }
        }
    }
}
