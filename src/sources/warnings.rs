//! Non-fatal warnings during source file discovery.

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
    AmbiguousPackageDir {
        /// Root-relative candidate directories.
        candidates: Vec<String>,
        /// Selected root-relative package directory.
        chosen: String,
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
}
