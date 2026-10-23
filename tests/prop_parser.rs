//! Property-based tests for parser helpers (ignore directives, `LineIndex` via
//! `parse_file`).

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::fmt::Write as _;

use chokkin::internals::extract_ignores;
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
        eol in prop::sample::select(vec!["\n", "\r\n", "\r"]),
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
        comments in 0usize..60,
        eol in prop::sample::select(vec!["\n", "\r\n", "\r"]),
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

    use chokkin::internals::{
        FileContext, ImportKind, LayoutInfo, ProjectRoot, RootMarker, TargetVersion, parse_file,
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
            let layout = LayoutInfo::new("src", vec!["acme".to_owned()]);
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
            let layout = LayoutInfo::new("", Vec::new());
            let parsed = parse_file(&root, "m.py", &layout, FileContext::Runtime, &TargetVersion::default_py311())
                .expect("parse");
            let line_count = u32::try_from(source.lines().count() + 2).unwrap_or(u32::MAX);
            for import in &parsed.imports {
                prop_assert!(import.line >= 1 && import.line <= line_count);
            }
        }
    }
}

/// `parse_file` over raw bytes: line endings, BOM, and PEP 263 decoding.
mod source_bytes_props {
    use std::fmt::Write as _;
    use std::fs;

    use chokkin::internals::{
        Exports, FileContext, LayoutInfo, ParseSeverity, ParsedModule, ProjectRoot, RootMarker,
        TargetVersion, parse_file,
    };
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn parse_bytes(bytes: &[u8]) -> ParsedModule {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("m.py"), bytes).expect("write");
        let root = ProjectRoot {
            path: fs::canonicalize(dir.path()).expect("canon"),
            marker: RootMarker::PyProjectToml,
        };
        let layout = LayoutInfo::new("", Vec::new());
        parse_file(
            &root,
            "m.py",
            &layout,
            FileContext::Runtime,
            &TargetVersion::default_py311(),
        )
        .expect("parse")
    }

    fn eol() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["\n", "\r\n", "\r"])
    }

    fn module_name() -> impl Strategy<Value = String> {
        "zz[a-z]{1,6}"
    }

    type Extracted = (
        Vec<(String, u32)>,
        Vec<(bool, Vec<String>, u32)>,
        Vec<(String, u32)>,
        Exports,
    );

    /// Imports, ignores, symbols and exports: the parts later steps read.
    fn extracted(parsed: &ParsedModule) -> Extracted {
        (
            parsed
                .imports
                .iter()
                .map(|import| (import.module.clone(), import.line))
                .collect(),
            parsed
                .ignores
                .iter()
                .map(|directive| {
                    (
                        directive.file_level,
                        directive.codes.clone(),
                        directive.line,
                    )
                })
                .collect(),
            parsed
                .symbols
                .iter()
                .map(|symbol| (symbol.name.clone(), symbol.line))
                .collect(),
            parsed.exports.clone(),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(128)))]

        /// Any bytes come back `Ok`; valid UTF-8 is skipped only for a syntax
        /// error, and a skipped file yields nothing.
        #[test]
        fn parse_file_is_total_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
            let parsed = parse_bytes(&bytes);
            if std::str::from_utf8(&bytes).is_ok() {
                let syntax_error = parsed
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == ParseSeverity::Error);
                prop_assert_eq!(parsed.skipped, syntax_error);
            }
            if parsed.skipped {
                prop_assert!(parsed.imports.is_empty() && parsed.ignores.is_empty());
            }
        }

        /// Nesting at `parser::depth::MAX_AST_DEPTH` (1,000 levels) is walked
        /// whole, imports included; one level more makes the file unreadable
        /// rather than overflowing the walks' stack (#1105).
        #[test]
        fn nesting_past_the_depth_limit_is_skipped_not_aborted(
            lists in 990usize..1010,
            eol in eol(),
        ) {
            let source = format!(
                "import zzdeep{eol}x = {}{}{eol}",
                "[".repeat(lists),
                "]".repeat(lists),
            );
            let parsed = parse_bytes(source.as_bytes());
            // The assignment is one level, each list another.
            let too_deep = lists + 1 > 1_000;
            prop_assert_eq!(parsed.skipped, too_deep);
            let imports: Vec<_> = parsed.imports.iter().map(|import| import.module.as_str()).collect();
            prop_assert_eq!(imports, if too_deep { vec![] } else { vec!["zzdeep"] });
        }

        /// A header comment block, then imports with inline ignores: the
        /// file-level directive is found however long the header is, and an
        /// inline directive carries the line of the import it sits on, for
        /// every line-ending style and with or without a UTF-8 BOM.
        #[test]
        fn ignores_and_imports_agree_on_lines(
            header in 0usize..60,
            file_ignore in any::<bool>(),
            imports in prop::collection::vec((module_name(), any::<bool>(), 0usize..3), 1..6),
            eol in eol(),
            bom in any::<bool>(),
        ) {
            let mut source = String::new();
            if bom {
                source.push('\u{feff}');
            }
            for index in 0..header {
                let _ = write!(source, "# license header line {index}{eol}");
            }
            if file_ignore {
                let _ = write!(source, "# chokkin: file-ignore[CHK001]{eol}");
            }
            let mut line = header + usize::from(file_ignore);
            let mut want_imports = Vec::new();
            let mut want_inline = Vec::new();
            for (module, ignored, blanks) in &imports {
                for _ in 0..*blanks {
                    source.push_str(eol);
                    line += 1;
                }
                line += 1;
                let line = u32::try_from(line).expect("line");
                let _ = write!(source, "import {module}");
                if *ignored {
                    source.push_str("  # chokkin: ignore[CHK003]");
                    want_inline.push(line);
                }
                source.push_str(eol);
                want_imports.push((module.clone(), line));
            }
            let parsed = parse_bytes(source.as_bytes());
            let got_imports: Vec<_> = parsed
                .imports
                .iter()
                .map(|import| (import.module.clone(), import.line))
                .collect();
            prop_assert_eq!(got_imports, want_imports);
            let file_level = parsed.ignores.iter().filter(|d| d.file_level).count();
            prop_assert_eq!(file_level, usize::from(file_ignore));
            let inline: Vec<u32> = parsed
                .ignores
                .iter()
                .filter(|d| !d.file_level)
                .map(|d| d.line)
                .collect();
            prop_assert_eq!(inline, want_inline);
        }

        /// A latin-1 or cp1252 file parses like its UTF-8 twin: the same
        /// text, re-encoded, under the same coding declaration.
        #[test]
        fn single_byte_source_parses_like_its_utf8_twin(
            cookie in prop::sample::select(vec![
                "# -*- coding: latin-1 -*-",
                "# coding=iso-8859-1",
                "# vim: set fileencoding=cp1252 :",
                "# coding: windows-1252",
            ]),
            shebang in any::<bool>(),
            body in prop::collection::vec(
                prop_oneof![
                    module_name().prop_map(|m| format!("import {m}")),
                    module_name().prop_map(|m| format!("def {m}():\n    return 'caf\u{e9}'")),
                    module_name().prop_map(|m| format!("__all__ = ['{m}', '\u{e9}t\u{e9}']")),
                    Just("# r\u{e9}sum\u{e9} na\u{ef}ve \u{a3}5".to_owned()),
                    Just("x = 1  # chokkin: ignore[CHK006]".to_owned()),
                ],
                1..8,
            ),
            eol in eol(),
        ) {
            let mut text = String::new();
            if shebang {
                text.push_str("#!/usr/bin/env python");
                text.push_str(eol);
            }
            text.push_str(cookie);
            text.push_str(eol);
            // A non-ASCII byte so the re-encoded file is not valid UTF-8.
            text.push_str("# \u{e9}");
            text.push_str(eol);
            for stmt in &body {
                text.push_str(&stmt.replace('\n', eol));
                text.push_str(eol);
            }
            let single_byte: Vec<u8> = text
                .chars()
                .map(|ch| u8::try_from(u32::from(ch)).expect("latin-1 text"))
                .collect();
            let decoded = parse_bytes(&single_byte);
            prop_assert!(!decoded.skipped);
            prop_assert_eq!(extracted(&decoded), extracted(&parse_bytes(text.as_bytes())));
        }

        /// The last literal `__all__` assignment, annotated or not, is the
        /// export list, and later literal `+=` / `.extend` / `.append` add to
        /// it (#567). A non-literal assignment or addition warns and makes it
        /// partial. Before any literal assignment there is no list to add to
        /// (ADR 0005).
        #[test]
        fn exports_follow_last_literal_all(
            assignments in prop::collection::vec(
                (prop::collection::vec("_?[a-z]{1,4}", 0..4), 0usize..11),
                1..5,
            ),
        ) {
            let mut source = String::new();
            let mut want = Exports::Absent;
            let mut warnings = 0;
            for (names, form) in &assignments {
                let quoted: Vec<String> = names.iter().map(|name| format!("{name:?}")).collect();
                let list = quoted.join(", ");
                let statement = match form {
                    0 => format!("__all__ = [{list}]"),
                    1 => format!("__all__ = ({list}{})", if quoted.len() == 1 { "," } else { "" }),
                    2 => format!("__all__: list[str] = [{list}]"),
                    3 => format!("__all__: tuple[str, ...] = ({list}{})", if quoted.len() == 1 { "," } else { "" }),
                    4 => "__all__ = sorted(names)".to_owned(),
                    5 => "__all__: list[str]".to_owned(),
                    6 => format!("__all__ += [{list}]"),
                    7 => format!("__all__.extend([{list}])"),
                    8 => match quoted.first() {
                        Some(name) => format!("__all__.append({name})"),
                        None => "__all__.append(name)".to_owned(),
                    },
                    9 => "__all__ += sub.__all__".to_owned(),
                    _ => "__all__.extend(names)".to_owned(),
                };
                let added = match form {
                    0..=3 => {
                        want = Exports::Known(names.clone());
                        None
                    },
                    6 | 7 => Some(Some(names.clone())),
                    8 => Some(names.first().map(|name| vec![name.clone()])),
                    4 | 9 | 10 => Some(None),
                    _ => None,
                };
                match (added, &mut want) {
                    (None, _) | (Some(Some(_)), Exports::Unknown) => {},
                    (Some(Some(names)), Exports::Known(list) | Exports::Partial(list)) => {
                        list.extend(names);
                    },
                    (Some(None), Exports::Known(list) | Exports::Partial(list)) => {
                        want = Exports::Partial(std::mem::take(list));
                        warnings += 1;
                    },
                    (Some(Some(_)), Exports::Absent)
                    | (Some(None), Exports::Absent | Exports::Unknown) => {
                        want = Exports::Unknown;
                        warnings += 1;
                    },
                }
                let _ = writeln!(source, "{statement}");
            }
            let parsed = parse_bytes(source.as_bytes());
            prop_assert_eq!(&parsed.exports, &want);
            let got_warnings = parsed
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("__all__"))
                .count();
            prop_assert_eq!(got_warnings, warnings);
        }
    }
}
