//! `requirements*.txt` line-based edits.

use crate::manifest::normalize_distribution_name;

use super::error::FixError;
use super::write::{atomic_write, read_manifest};

/// Remove a dependency line from a requirements file by line number or name match.
pub fn remove_dependency_line(
    path: &std::path::Path,
    distribution: &str,
    line: Option<u32>,
) -> Result<String, FixError> {
    let (rel, contents) = read_manifest(path, "requirements.txt")?;

    if contents.lines().any(|line| line.contains("--hash=")) {
        return Err(FixError::Unsupported {
            detail: "hash-pinned requirements files cannot be auto-edited".to_owned(),
        });
    }

    let target = normalize_distribution_name(distribution);
    let mut removed = false;
    let mut output = Vec::new();

    for (index, raw_line) in contents.lines().enumerate() {
        let line_no = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if line.is_some_and(|expected| expected == line_no) || line_name_matches(raw_line, &target)
        {
            removed = true;
            continue;
        }
        output.push(raw_line);
    }

    if !removed {
        return Err(FixError::Unsupported {
            detail: format!("dependency `{distribution}` not found in {rel}"),
        });
    }

    let mut updated = output.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }

    atomic_write(path, updated.as_bytes(), true).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    Ok(format!("removed `{distribution}` from {rel}"))
}

fn line_name_matches(line: &str, distribution: &str) -> bool {
    let trimmed = strip_comment(line).trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return false;
    }
    let name = trimmed
        .split(['[', ';', '#', ' '])
        .next()
        .unwrap_or(trimmed);
    let normalized = normalize_distribution_name(
        name.split(['=', '<', '>', '!', '~', '['])
            .next()
            .unwrap_or(name),
    );
    normalized == distribution
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn removes_matching_requirements_line() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("requirements.txt");
        std::fs::write(&path, "boto3>=1.0\nrequests>=2.0\n").expect("write");
        remove_dependency_line(&path, "boto3", None).expect("remove");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(!updated.contains("boto3"));
        assert!(updated.contains("requests"));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn name() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9]{0,8}"
        }

        fn requirement_line() -> impl Strategy<Value = String> {
            (
                name(),
                prop::sample::select(vec!["", ">=1.0", "==2.1", "~=3.0"]),
                prop::sample::select(vec!["", "  # note", " ; python_version >= \"3.9\""]),
            )
                .prop_map(|(n, spec, tail)| format!("{n}{spec}{tail}"))
        }

        fn run(
            contents: &str,
            target: &str,
            line: Option<u32>,
        ) -> (Result<String, FixError>, String) {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("requirements.txt");
            std::fs::write(&path, contents).expect("write");
            let result = remove_dependency_line(&path, target, line);
            let updated = std::fs::read_to_string(&path).expect("read");
            (result, updated)
        }

        proptest! {
            /// Removing by name keeps every non-matching line, in order.
            #[test]
            fn removal_preserves_other_lines(
                lines in prop::collection::vec(requirement_line(), 1..8),
                pick in any::<prop::sample::Index>(),
                trailing_newline in any::<bool>(),
            ) {
                let target = {
                    let l = &lines[pick.index(lines.len())];
                    l.split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("").to_owned()
                };
                let mut contents = lines.join("\n");
                if trailing_newline {
                    contents.push('\n');
                }
                let (result, updated) = run(&contents, &target, None);
                prop_assert!(result.is_ok());
                let expected: Vec<&str> = lines
                    .iter()
                    .map(String::as_str)
                    .filter(|l| !line_name_matches(l, &target))
                    .collect();
                prop_assume!(!expected.is_empty());
                let got: Vec<&str> = updated.lines().collect();
                prop_assert_eq!(got, expected);
            }

            /// Removal is idempotent: a second run reports "not found" and
            /// leaves the file untouched.
            #[test]
            fn removal_is_idempotent(
                lines in prop::collection::vec(requirement_line(), 1..6),
            ) {
                let target = lines[0].split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("").to_owned();
                let dir = TempDir::new().expect("tempdir");
                let path = dir.path().join("requirements.txt");
                std::fs::write(&path, lines.join("\n") + "\n").expect("write");
                prop_assert!(remove_dependency_line(&path, &target, None).is_ok());
                let once = std::fs::read_to_string(&path).expect("read");
                prop_assert!(remove_dependency_line(&path, &target, None).is_err());
                prop_assert_eq!(std::fs::read_to_string(&path).expect("read"), once);
            }

            /// Whitespace between name and specifier (tab or space) still names
            /// the distribution.
            #[test]
            #[ignore = "bug #438"]
            fn tab_or_space_separated_specifier_is_matched(
                n in name(),
                sep in prop::sample::select(vec![" ", "\t", "  "]),
            ) {
                let (result, _) = run(&format!("{n}{sep}>=1.0\nother\n"), &n, None);
                prop_assert!(result.is_ok(), "{result:?}");
            }

            /// Removing by line number must not also drop other lines that
            /// merely name the same distribution (e.g. marker variants).
            #[test]
            #[ignore = "bug #438"]
            fn removal_by_line_number_touches_only_that_line(n in name()) {
                let contents = format!("{n}>=1 ; python_version < \"3.9\"\n{n}>=2 ; python_version >= \"3.9\"\n");
                let (result, updated) = run(&contents, &n, Some(1));
                prop_assert!(result.is_ok());
                prop_assert!(!updated.trim().is_empty(), "over-removed: {:?}", updated);
            }

            /// CRLF files keep CRLF endings for the untouched lines.
            #[test]
            #[ignore = "bug #437"]
            fn removal_preserves_crlf(
                lines in prop::collection::vec(name(), 2..6),
            ) {
                let mut unique = lines.clone();
                unique.sort();
                unique.dedup();
                prop_assume!(unique.len() == lines.len());
                let contents = lines.join("\r\n") + "\r\n";
                let (result, updated) = run(&contents, &lines[0], None);
                prop_assert!(result.is_ok());
                prop_assert!(!updated.replace("\r\n", "").contains('\n'), "bare LF introduced: {updated:?}");
            }

            /// A removal that empties the file must not leave a stray blank line.
            #[test]
            #[ignore = "bug #437"]
            fn removing_the_only_line_leaves_empty_file(n in name()) {
                let (result, updated) = run(&format!("{n}\n"), &n, None);
                prop_assert!(result.is_ok());
                prop_assert_eq!(updated, "");
            }

            /// Removal by line number removes exactly that line when no other
            /// line names the same distribution.
            #[test]
            fn removal_by_line_number_removes_that_line(
                lines in prop::collection::vec(name(), 2..8),
                pick in any::<prop::sample::Index>(),
            ) {
                let mut unique = lines.clone();
                unique.sort();
                unique.dedup();
                prop_assume!(unique.len() == lines.len());
                let idx = pick.index(lines.len());
                let (result, updated) = run(&(lines.join("\n") + "\n"), "does-not-matter", Some(u32::try_from(idx + 1).unwrap_or(1)));
                prop_assert!(result.is_ok());
                let mut expected = lines;
                expected.remove(idx);
                let got: Vec<String> = updated.lines().map(str::to_owned).collect();
                prop_assert_eq!(got, expected);
            }
        }
    }
}
