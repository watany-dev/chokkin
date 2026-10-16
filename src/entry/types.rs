//! Entry root construction types (pipeline step 8).

use std::collections::BTreeSet;
use std::fmt;

use crate::config::{EntrySpec, PluginId, ProjectMode};
use crate::sources::{FileContext, PublicSurface};

/// Where an entry root was discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOrigin {
    /// `[tool.chokkin].entry` or workspace override.
    Config,
    /// `[project.scripts]` / `[project.gui-scripts]` / `[project.entry-points]`,
    /// or a hatch build hook's `path`.
    Manifest {
        /// Entry-point name within the group.
        name: String,
        /// `console` | `gui` | other group name.
        group: String,
    },
    /// Plugin extractor (`PluginEntry`).
    Plugin {
        /// Contributing plugin.
        plugin: PluginId,
        /// Human-readable origin label.
        label: String,
    },
    /// §8 automatic path rules (`manage.py`, `__main__.py`, …).
    Auto {
        /// Rule identifier, e.g. `manage.py` or `scripts/**`.
        rule: String,
    },
    /// `module:symbol` reference resolved to a file (uvicorn, gunicorn, …).
    SymbolRef {
        /// Dotted module name.
        module: String,
        /// Symbol within the module.
        symbol: String,
        /// Human-readable origin label.
        label: String,
    },
    /// PEP 723 inline script metadata (`# /// script`).
    Script,
    /// A command that runs the file by path (#797).
    Command {
        /// Human-readable origin label.
        label: String,
    },
    /// A module providing the PEP 517 build hooks (#782).
    BuildBackend,
}

/// One merged entry root after deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRoot {
    /// Root-relative file path.
    pub path: String,
    /// Every object an entry names in this file; candidates for one path
    /// merge their symbols here, so no entry's target is lost (#884).
    pub symbols: BTreeSet<String>,
    /// Assigned file context.
    pub context: FileContext,
    /// Discovery sources merged for this path.
    pub origins: Vec<EntryOrigin>,
}

impl EntryRoot {
    /// Whether only a library subpackage's public surface makes this a root,
    /// so its imports run when a caller imports it, not when the package
    /// loads (#802).
    pub(crate) fn only_public(&self) -> bool {
        !self.origins.is_empty()
            && self.origins.iter().all(|origin| {
                matches!(origin, EntryOrigin::Auto { rule } if rule == super::build::LIBRARY_SUBPACKAGE_RULE)
            })
    }
}

/// Outcome of entry root construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPlan {
    /// Project mode after `mode = auto` inference (§8); never `Auto`.
    pub mode: ProjectMode,
    /// Merged entry roots sorted by path.
    pub roots: Vec<EntryRoot>,
    /// Non-fatal conditions.
    pub warnings: Vec<EntryWarning>,
    /// Root-relative directories of workspace members that ship as their own
    /// libraries, whatever mode the project itself resolved to (#488).
    pub library_members: Vec<String>,
}

impl EntryPlan {
    /// Mode that scores an orphan at root-relative `path`.
    #[must_use]
    pub fn mode_for(&self, path: &str) -> ProjectMode {
        if self.in_library_member(path) {
            ProjectMode::Library
        } else {
            self.mode
        }
    }

    /// Mode `path` is judged under once the root wheel's `surface` is known:
    /// a file outside it has no outside caller, so a root-library file it does
    /// not ship is judged as in app mode (R-05). A library member ships its own
    /// wheel, so the root's surface does not apply to it (#515).
    #[must_use]
    pub fn surface_mode(&self, path: &str, surface: Option<&PublicSurface>) -> ProjectMode {
        if !self.in_library_member(path) && surface.is_some_and(|surface| !surface.contains(path)) {
            ProjectMode::App
        } else {
            self.mode_for(path)
        }
    }

    #[must_use]
    pub fn in_library_member(&self, path: &str) -> bool {
        self.library_members.iter().any(|member| {
            path.strip_prefix(member.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

/// Non-fatal entry construction warnings.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryWarning {
    /// Configured or resolved entry path is not in discovered sources.
    MissingEntryPath {
        /// Root-relative path.
        path: String,
    },
    /// A manifest or plugin `module:symbol` target could not be mapped to a file.
    UnresolvedModuleTarget {
        /// Dotted module name.
        module: String,
        /// Root-relative file that declares the target.
        file: String,
        /// Where in `file` the target is declared.
        label: String,
    },
}

impl fmt::Display for EntryWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntryPath { path } => write!(
                formatter,
                "entry: skipped entry `{path}`: not a discovered source file; unused files and dependencies are reported below certain"
            ),
            Self::UnresolvedModuleTarget {
                module,
                file,
                label,
            } => write!(
                formatter,
                "entry: could not map `{module}` to a source file (`{label}` in `{file}`)"
            ),
        }
    }
}

/// Candidate entry before merge and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EntryCandidate {
    pub spec: EntrySpec,
    pub context: FileContext,
    pub origin: EntryOrigin,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(mode: ProjectMode) -> EntryPlan {
        EntryPlan {
            mode,
            roots: Vec::new(),
            warnings: Vec::new(),
            library_members: vec!["libs/core".to_owned()],
        }
    }

    #[test]
    fn surface_mode_demotes_only_root_files_the_wheel_does_not_ship() {
        let surface = PublicSurface {
            files: ["src/acme/api.py".to_owned()].into(),
        };
        let library = plan(ProjectMode::Library);
        let mode = |path| library.surface_mode(path, Some(&surface));
        assert_eq!(mode("src/acme/api.py"), ProjectMode::Library);
        assert_eq!(mode("tools/release.py"), ProjectMode::App);
        assert_eq!(mode("libs/core/core/x.py"), ProjectMode::Library);
        assert_eq!(
            library.surface_mode("tools/release.py", None),
            ProjectMode::Library
        );
        let app = plan(ProjectMode::App);
        assert_eq!(
            app.surface_mode("src/acme/api.py", Some(&surface)),
            ProjectMode::App
        );
        assert_eq!(
            app.surface_mode("libs/core/core/x.py", Some(&surface)),
            ProjectMode::Library
        );
    }
}
