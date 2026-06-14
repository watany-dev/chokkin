//! `chokkin --init` starter configuration generation.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{RuntimeOverrides, apply_overrides, load_config};
use crate::discovery::{DiscoveryError, discover_project_root};
use crate::manifest::{ManifestError, extract_manifest, resolve_target_version};
use crate::sources::{SourcesError, discover_sources};

/// Result of writing a starter `[tool.chokkin]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// Discovered project root.
    pub root: PathBuf,
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
        matches!(
            self,
            Self::Discovery(_)
                | Self::Config(_)
                | Self::Manifest(_)
                | Self::Sources(_)
                | Self::ExistingConfig { .. }
                | Self::MissingPyproject { .. }
        )
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
    if !pyproject.is_file() {
        return Err(InitError::MissingPyproject {
            root: root.path.clone(),
        });
    }

    let project_globs = sources.effective_globs.clone();
    let entry = infer_entry_roots(&root.path);
    let block = render_starter_config(
        &project_globs,
        &entry,
        resolve_target_version(&config.effective, &manifest).as_str(),
    );
    append_config(&pyproject, &block)?;

    Ok(InitReport {
        root: root.path,
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
        .filter(|candidate| root.join(candidate).is_file())
        .map(str::to_owned)
        .collect()
}

fn render_starter_config(
    project_globs: &[String],
    entry: &[String],
    target_version: &str,
) -> String {
    let mut block = String::from("\n[tool.chokkin]\n");
    block.push_str("mode = \"auto\"\n");
    block.push_str("production = false\n");
    block.push_str("target_version = \"");
    block.push_str(&escape_toml_string(target_version));
    block.push_str("\"\n");
    block.push_str("respect_gitignore = true\n");
    block.push_str("confidence = \"likely\"\n");
    block.push_str("project = ");
    block.push_str(&render_string_array(project_globs));
    block.push('\n');
    if !entry.is_empty() {
        block.push_str("entry = ");
        block.push_str(&render_string_array(entry));
        block.push('\n');
    }
    block
}

fn render_string_array(values: &[String]) -> String {
    let mut rendered = String::from("[");
    let mut first = true;
    for value in values {
        if first {
            first = false;
        } else {
            rendered.push_str(", ");
        }
        rendered.push('"');
        rendered.push_str(&escape_toml_string(value));
        rendered.push('"');
    }
    rendered.push(']');
    rendered
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn append_config(path: &Path, block: &str) -> Result<(), InitError> {
    let existing = fs::read_to_string(path).map_err(|source| InitError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let separator = if existing.ends_with('\n') { "" } else { "\n" };
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| InitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(separator.as_bytes())
        .map_err(|source| InitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(block.as_bytes())
        .map_err(|source| InitError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(report.project_globs.contains(&"src/**/*.py".to_owned()));

        let text = fs::read_to_string(report.path).expect("read pyproject");
        assert!(text.contains("[tool.chokkin]"));
        assert!(text.contains("mode = \"auto\""));
        assert!(text.contains("entry = [\"manage.py\"]"));
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
            "py311",
        );
        assert!(block.contains("src/**/weird\\\"name.py"));
    }
}
