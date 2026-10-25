//! `chokkin --init` starter configuration generation.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{DependencyGroupsConfig, RuntimeOverrides, apply_overrides, load_config};
use crate::discovery::{DiscoveryError, discover_project_root};
use crate::manifest::{ManifestError, extract_manifest, resolve_target_version};
use crate::path_util::{is_file_named, resolve_under_root};
use crate::sources::{SourcesError, discover_sources};

/// Result of writing a starter `[tool.chokkin]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// File that was updated.
    pub path: PathBuf,
    /// Project globs written to the starter config.
    pub project_globs: Vec<String>,
    /// Entry roots written to the starter config.
    pub entry: Vec<String>,
}

/// Fatal error while generating a starter configuration.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// Project root discovery failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// Configuration loading failed.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    /// Manifest extraction failed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Source discovery failed.
    #[error(transparent)]
    Sources(#[from] SourcesError),
    /// Filesystem I/O failed.
    #[error("failed to update {path}")]
    Io {
        /// Path being updated.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Existing config means init must not append a second table.
    #[error("chokkin config already exists in {path}")]
    ExistingConfig {
        /// Existing config path.
        path: PathBuf,
    },
    /// `pyproject.toml` is a symlink (or under one) that leaves the root.
    #[error("init target `pyproject.toml` resolves outside the project root")]
    OutsideRoot,
    /// Init writes to pyproject.toml only.
    #[error("pyproject.toml not found in discovered project root {root}")]
    MissingPyproject {
        /// Discovered project root.
        root: PathBuf,
    },
}

impl InitError {
    /// Whether this error should map to [`crate::ExitStatus::UsageError`].
    #[must_use]
    pub const fn is_usage_error(&self) -> bool {
        !matches!(self, Self::Io { .. })
    }
}

/// Append a starter `[tool.chokkin]` table to `pyproject.toml`.
pub fn init_project(
    start: &Path,
    project_root: Option<&Path>,
    overrides: &RuntimeOverrides,
) -> Result<InitReport, InitError> {
    let root = discover_project_root(project_root.unwrap_or(start))?;
    let mut config = load_config(&root)?;
    reject_existing_config(&config)?;

    apply_overrides(&mut config.effective, overrides);
    let manifest = extract_manifest(&root, &config)?;
    let sources = discover_sources(&root, &config, &manifest)?;
    let pyproject = root.path.join("pyproject.toml");
    if !is_file_named(&root.path, "pyproject.toml") {
        return Err(InitError::MissingPyproject { root: root.path });
    }
    // Same containment rule as `--fix` (`fix::containment`): never write
    // through a symlink to a file outside the project root.
    let canonical_root = fs::canonicalize(&root.path).unwrap_or_else(|_| root.path.clone());
    if !matches!(resolve_under_root(&canonical_root, &pyproject), Ok(Some(_))) {
        return Err(InitError::OutsideRoot);
    }

    let project_globs = sources.effective_globs;
    let entry = infer_entry_roots(&root.path);
    let block = render_starter_config(
        &project_globs,
        &entry,
        &config.effective.dependencies,
        resolve_target_version(&config.effective, &manifest)
            .clamped_to_bundled()
            .as_str(),
    );
    append_config(&pyproject, &block)?;

    Ok(InitReport {
        path: pyproject,
        project_globs,
        entry,
    })
}

fn reject_existing_config(config: &crate::config::LoadedConfig) -> Result<(), InitError> {
    if let Some(path) = &config.sources.dot_chokkin_toml {
        return Err(InitError::ExistingConfig { path: path.clone() });
    }
    if let Some(path) = &config.sources.chokkin_toml {
        return Err(InitError::ExistingConfig { path: path.clone() });
    }
    if config.sources.pyproject_tool_chokkin {
        return Err(InitError::ExistingConfig {
            path: config.root.path.join("pyproject.toml"),
        });
    }
    Ok(())
}

fn infer_entry_roots(root: &Path) -> Vec<String> {
    ["manage.py", "app.py", "main.py"]
        .into_iter()
        .filter(|candidate| is_file_named(root, candidate))
        .map(str::to_owned)
        .collect()
}

fn render_starter_config(
    project_globs: &[String],
    entry: &[String],
    dependencies: &DependencyGroupsConfig,
    target_version: &str,
) -> String {
    let entry_line = if entry.is_empty() {
        String::new()
    } else {
        format!("entry = {}\n", render_string_array(entry))
    };
    format!(
        "\n[tool.chokkin]\n\
         mode = \"auto\"\n\
         production = false\n\
         target_version = \"{target_version}\"\n\
         respect_gitignore = true\n\
         confidence = \"likely\"\n\
         project = {project}\n\
         {entry_line}\n\
         [tool.chokkin.dependencies]\n\
         dev_groups = {dev_groups}\n\
         runtime_groups = {runtime_groups}\n\
         type_groups = {type_groups}\n",
        target_version = escape_toml_string(target_version),
        project = render_string_array(project_globs),
        dev_groups = render_string_array(&dependencies.dev_groups),
        runtime_groups = render_string_array(&dependencies.runtime_groups),
        type_groups = render_string_array(&dependencies.type_groups),
    )
}

fn render_string_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", escape_toml_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

/// `value` as the body of a TOML basic string, which holds no raw `"`, `\`
/// or control character; a glob takes a directory's name, which may.
fn escape_toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            // Every control character is at most U+009F.
            ch if ch.is_control() => {
                let _ = write!(escaped, "\\u{:04X}", u32::from(ch));
            },
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn append_config(path: &Path, block: &str) -> Result<(), InitError> {
    let io_error = |source: std::io::Error| InitError::Io {
        path: path.to_path_buf(),
        source,
    };
    let existing = fs::read_to_string(path).map_err(io_error)?;
    let separator = if existing.ends_with('\n') { "" } else { "\n" };
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(separator.as_bytes()).map_err(io_error)?;
    file.write_all(block.as_bytes()).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TargetVersion;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn init_appends_starter_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(
            &temp.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\n",
        );
        write(&temp.path().join("src/demo/__init__.py"), "");
        write(&temp.path().join("manage.py"), "");

        let report = init_project(temp.path(), None, &RuntimeOverrides::default()).expect("init");
        assert_eq!(report.entry, vec!["manage.py"]);
        assert!(
            report
                .project_globs
                .contains(&"src/**/*.{py,pyi,ipynb}".to_owned())
        );

        let text = fs::read_to_string(report.path).expect("read pyproject");
        assert!(text.contains("[tool.chokkin]"));
        assert!(text.contains("[tool.chokkin.dependencies]"));
        assert!(text.contains("mode = \"auto\""));
        assert!(text.contains("entry = [\"manage.py\"]"));
        assert!(text.contains("dev_groups = [\"dev\", \"test\", \"tests\", \"lint\", \"docs\"]"));
    }

    #[test]
    fn init_writes_a_target_version_that_loads_back() {
        // #1125: `requires-python` may name minors outside the bundled range.
        for (requires_python, target) in [
            (">=3.6", "py38"),
            (">=3.8", "py308"),
            (">=3.12", "py312"),
            (">=3.16", "py315"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            write(
                &temp.path().join("pyproject.toml"),
                &format!("[project]\nname = \"demo\"\nrequires-python = \"{requires_python}\"\n"),
            );
            let report =
                init_project(temp.path(), None, &RuntimeOverrides::default()).expect("init");
            let text = fs::read_to_string(report.path).expect("read pyproject");
            assert!(
                text.contains(&format!("target_version = \"{target}\"")),
                "{requires_python}: {text}"
            );
            let root = discover_project_root(temp.path()).expect("root");
            let loaded = load_config(&root).expect("config loads back");
            assert_eq!(
                loaded
                    .effective
                    .target_version
                    .as_ref()
                    .map(TargetVersion::as_str),
                Some(target)
            );
        }
    }

    #[test]
    fn init_rejects_existing_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(
            &temp.path().join("pyproject.toml"),
            "[tool.chokkin]\nmode = \"app\"\n",
        );

        let error = init_project(temp.path(), None, &RuntimeOverrides::default())
            .expect_err("existing config");
        assert!(matches!(error, InitError::ExistingConfig { .. }));
        assert!(error.is_usage_error());
    }

    #[test]
    fn render_escapes_strings() {
        let block = render_starter_config(
            &["src/**/weird\"name.py".to_owned()],
            &["main.py".to_owned()],
            &DependencyGroupsConfig {
                dev_groups: vec!["dev".to_owned()],
                runtime_groups: vec!["server".to_owned()],
                type_groups: vec!["types".to_owned()],
            },
            "py311",
        );
        assert!(block.contains("src/**/weird\\\"name.py"));
        assert_eq!(
            escape_toml_string("a\tb\n\u{7f}"),
            "a\\u0009b\\u000A\\u007F"
        );
    }

    #[test]
    fn render_omits_empty_entry() {
        let block = render_starter_config(
            &["src/**/*.py".to_owned()],
            &[],
            &DependencyGroupsConfig {
                dev_groups: vec!["dev".to_owned(), "test".to_owned()],
                runtime_groups: Vec::new(),
                type_groups: Vec::new(),
            },
            "py311",
        );
        assert_eq!(
            block,
            "\n[tool.chokkin]\nmode = \"auto\"\nproduction = false\ntarget_version = \"py311\"\n\
             respect_gitignore = true\nconfidence = \"likely\"\nproject = [\"src/**/*.py\"]\n\n\
             [tool.chokkin.dependencies]\ndev_groups = [\"dev\", \"test\"]\nruntime_groups = []\n\
             type_groups = []\n"
        );
    }

    /// The starter table (#1105): whatever globs, entries and group names it
    /// is rendered from, `load_config` reads back the same values.
    mod props {
        use super::*;
        use crate::config::{EntrySpec, TargetVersion};
        use proptest::prelude::*;

        /// Text that stresses TOML string escaping.
        fn text() -> impl Strategy<Value = String> {
            "[a-z0-9_*{},./\\\\\"' #=\\[\\]\\t\\n\\r\u{7f}\u{1b}é日-]{1,16}"
        }

        /// A relative glob, as `load_config` requires, of segments that start
        /// with a letter (so none is `..`).
        fn glob() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_*{},.\"' #=\\[\\]\\t\\n\\r\u{7f}\u{1b}é日-]{0,8}\
             (/[a-z][a-z0-9_*{},.\"' \\t\\n\u{7f}é日-]{0,8}){0,2}"
        }

        fn names() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec(text(), 0..4)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(64)))]

            #[test]
            fn the_starter_config_loads_back_as_rendered(
                globs in prop::collection::vec(glob(), 1..4),
                entry in prop::sample::subsequence(vec!["manage.py", "app.py", "main.py"], 0..=3),
                dependencies in (names(), names(), names()),
                minor in 10u8..15,
            ) {
                let entry: Vec<String> = entry.into_iter().map(str::to_owned).collect();
                let (dev_groups, runtime_groups, type_groups) = dependencies;
                let dependencies = DependencyGroupsConfig {
                    dev_groups,
                    runtime_groups,
                    type_groups,
                };
                let target = format!("py3{minor}");
                let block = render_starter_config(&globs, &entry, &dependencies, &target);

                let temp = tempfile::tempdir().expect("tempdir");
                let pyproject = temp.path().join("pyproject.toml");
                write(&pyproject, "[project]\nname = \"demo\"\n");
                append_config(&pyproject, &block).expect("append");
                let root = discover_project_root(temp.path()).expect("root");
                let loaded = load_config(&root).map_err(|error| {
                    TestCaseError::fail(format!("{error}\n{block}"))
                })?;

                prop_assert!(loaded.sources.pyproject_tool_chokkin);
                let config = loaded.effective;
                prop_assert_eq!(config.project, globs);
                let entries: Vec<EntrySpec> = entry
                    .iter()
                    .map(|path| EntrySpec::parse(path).expect("entry"))
                    .collect();
                prop_assert_eq!(config.entry, entries);
                prop_assert_eq!(config.dependencies, dependencies);
                prop_assert_eq!(config.target_version, TargetVersion::parse(&target));
            }
        }
    }
}
