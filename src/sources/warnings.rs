//! Non-fatal warnings during source file discovery.

use crate::path_util::join_rel;

/// Non-fatal conditions encountered while discovering source files.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourcesWarning {
    /// A configured entry path does not exist.
    #[error("sources: missing entry path `{path}`")]
    MissingEntryPath {
        /// Root-relative entry path.
        path: String,
    },
    /// A configured entry path refers to a directory.
    #[error("sources: entry path is a directory `{path}`")]
    EntryPathIsDirectory {
        /// Root-relative entry path.
        path: String,
    },
    /// Several undeclared package directories; one was chosen.
    #[error("sources: ambiguous package directory ({candidates:?}); chose `{chosen}`")]
    AmbiguousFlatLayout {
        /// Root-relative candidate directories.
        candidates: Vec<String>,
        /// Selected root-relative package directory.
        chosen: String,
    },
    /// `.gitignore` could not be read or parsed.
    #[error("sources: could not read `.gitignore` at `{path}`")]
    GitignoreUnreadable {
        /// Path to the unreadable file.
        path: String,
    },
    /// Project exceeds the large-project file threshold.
    #[error("sources: large project ({file_count} files discovered)")]
    LargeProject {
        /// Number of discovered files.
        file_count: usize,
    },
    /// A path could not be read during directory walking.
    #[error("sources: could not read `{path}`: {reason}")]
    PathUnreadable {
        /// Path that triggered the error.
        path: String,
        /// Human-readable error description.
        reason: String,
    },
    /// The only undeclared package directory is not named after the project.
    #[error(
        "sources: no package directory matches project `{project}`; guessed `{chosen}` \
         (declare it in the build backend config or [tool.chokkin] project)"
    )]
    GuessedPackageDir {
        /// `[project].name`.
        project: String,
        /// Selected root-relative package directory.
        chosen: String,
    },
}

impl SourcesWarning {
    /// Join every path onto `base`, the root-relative directory of the
    /// walk that emitted the warning.
    pub(crate) fn rebase(&mut self, base: &str) {
        match self {
            Self::MissingEntryPath { path }
            | Self::EntryPathIsDirectory { path }
            | Self::GitignoreUnreadable { path } => *path = join_rel(base, path),
            Self::AmbiguousFlatLayout { candidates, chosen } => {
                for candidate in candidates {
                    *candidate = join_rel(base, candidate);
                }
                *chosen = join_rel(base, chosen);
            },
            Self::GuessedPackageDir { chosen, .. } => *chosen = join_rel(base, chosen),
            // Already absolute: the walk reports the directory it opened.
            Self::PathUnreadable { .. } | Self::LargeProject { .. } => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SourcesWarning;

    #[test]
    fn rebase_joins_relative_paths_and_keeps_absolute_ones() {
        let mut ambiguous = SourcesWarning::AmbiguousFlatLayout {
            candidates: vec!["alpha".to_owned(), "beta".to_owned()],
            chosen: "alpha".to_owned(),
        };
        ambiguous.rebase("packages/x");
        assert_eq!(
            ambiguous,
            SourcesWarning::AmbiguousFlatLayout {
                candidates: vec!["packages/x/alpha".to_owned(), "packages/x/beta".to_owned()],
                chosen: "packages/x/alpha".to_owned(),
            }
        );
        let unreadable = SourcesWarning::PathUnreadable {
            path: "/abs/packages/x/data".to_owned(),
            reason: "denied".to_owned(),
        };
        let mut rebased = unreadable.clone();
        rebased.rebase("packages/x");
        assert_eq!(rebased, unreadable);
    }
}
