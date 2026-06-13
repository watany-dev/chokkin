//! Non-fatal warnings during source file discovery.

/// Non-fatal conditions encountered while discovering source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourcesWarning {
    /// A configured entry path does not exist.
    MissingEntryPath {
        /// Root-relative entry path.
        path: String,
    },
    /// A configured entry path refers to a directory.
    EntryPathIsDirectory {
        /// Root-relative entry path.
        path: String,
    },
    /// Multiple flat-layout package candidates; one was chosen.
    AmbiguousFlatLayout {
        /// All detected candidates.
        candidates: Vec<String>,
        /// Selected package directory name.
        chosen: String,
    },
    /// `.gitignore` could not be read or parsed.
    GitignoreUnreadable {
        /// Path to the unreadable file.
        path: String,
    },
    /// A path could not be read during directory walking.
    PathUnreadable {
        /// Path that triggered the error.
        path: String,
        /// Human-readable error description.
        reason: String,
    },
    /// Project exceeds the large-project file threshold.
    LargeProject {
        /// Number of discovered files.
        file_count: usize,
    },
}
