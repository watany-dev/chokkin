//! Input the analysis could not read, and the one stage that lowers the
//! findings it may hide (ADR 0005).
//!
//! A gap is recorded where it is found; each kind declares which findings it
//! weakens, so no rule has to know about unread input itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::config::{Confidence, DependencyGroupsConfig, PluginId, TargetVersion};
use crate::entry::{ModuleIndex, beside_origin, resolve_module_to_path};
use crate::graph::ProjectGraph;
use crate::manifest::{
    ExtrasState, InlineScript, LoadedManifest, ManifestWarning, archive_distribution_name,
    invalid_requirement_name, normalize_distribution_name,
};
use crate::parser::{ImportContext, NOT_IN_PACKAGE, ParseSummary, ParsedModule};
use crate::path_util::join_rel;
use crate::plugins::{CommandTarget, CommandTargetRef, PluginHints, UnreadSettings};
use crate::reachability::ReachabilityReport;
use crate::resolver::{
    RUNTIME_PEERS, ResolveWarning, ResolvedImport, StdlibRange, import_root, is_first_party_import,
};
use crate::rules::RuleContext;
use crate::rules::deps::{
    DeclarationBucket, declaration_bucket, declaration_buckets, file_context,
};
use crate::rules::types::{IssueCandidate, IssueSubject, Origin, RuleId, Severity, import_site};
use crate::sources::{FileContext, is_dockerfile};

/// One piece of input the analysis could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// Root-relative file the gap is in.
    pub file: String,
    /// What could not be read.
    pub kind: GapKind,
    /// The workspace member whose findings alone the gap lowers; `None`
    /// lowers every finding in its kind's scope (#910).
    pub member: Option<String>,
    /// What in the file could not be read and its line, which `--explain`
    /// shows in place of the file: an unresolved import's module (#911), a
    /// command's target (#913).
    pub site: Option<(String, Option<u32>)>,
    /// The root-relative project file the site names, whose findings alone
    /// the gap lowers: a module a string literal names (#1026); for an
    /// unresolved relative import, the directory whose files it lowers
    /// (#1113).
    pub target: Option<String>,
}

/// A workspace member's manifest: what it could not read lowers only the
/// findings in that member.
#[derive(Debug, Clone, Copy)]
pub struct MemberManifest<'a> {
    /// The member id findings carry in `workspace_member`.
    pub id: &'a str,
    /// Root-relative member directory, which the manifest's files are under.
    pub path: &'a str,
    /// The member's own manifest.
    pub manifest: &'a LoadedManifest,
}

/// What could not be read, which decides the findings a gap lowers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapKind {
    /// A reachable module that was skipped: none of its imports are known.
    UnreadModule,
    /// An entry point that is not a discovered source, so it was skipped
    /// (#923): the code it runs is not followed.
    SkippedEntry,
    /// An app-mode project has no entry root (#1138): no file is known to
    /// run, so any file or distribution may be used by code that does.
    NoEntryRoot,
    /// A skipped public module of a library that no root reaches: a caller
    /// may import it, so it may use any declared distribution (#550). It
    /// leaves orphans alone, since what it imports must ship in the wheel
    /// and so is public too.
    UnreadPublicModule,
    /// A reachable notebook was read, but some cells that may be code were
    /// not (#902): what those cells import is unknown. Every notebook is an
    /// entry (#514), so none is a public orphan (#942).
    UnreadNotebookCells,
    /// A reachable module has a relative import that could not be resolved
    /// (#1113): it may import any file below the directory its dots climb
    /// to, which `target` holds.
    UnresolvedRelativeImport,
    /// A reachable module loads a module whose name is computed at runtime.
    /// One in a public orphan is no gap: it lowers no distribution, and what
    /// a public module loads ships in the wheel and so is public too (#942).
    OpaqueDynamicImport,
    /// A manifest declares its runtime dependencies in a form that could not
    /// be read statically (#491): the root's for every finding, a member's
    /// for that member's (#910).
    RuntimeDependenciesUnknown,
    /// A manifest's extras could not all be read (#975): an unread extra may
    /// declare any import as optional. Partial extras lower the same way as
    /// unknown ones, since the lost entries may name anything (§6 rejects
    /// narrowing by what did parse).
    UnknownExtras,
    /// A manifest line that may declare the distribution its leading name
    /// names could not be parsed (#928): an import of that distribution may
    /// be declared.
    InvalidRequirement,
    /// The same on the runtime side (a runtime, extra or runtime-group line):
    /// it may also be the runtime declaration a misplaced finding misses (#935).
    InvalidRuntimeRequirement,
    /// A requirements-file line installs an archive whose file name names the
    /// distribution (#988), which is not read: an import of that distribution
    /// may be declared.
    ArchiveRequirement,
    /// The same on the runtime side, as [`Self::InvalidRuntimeRequirement`].
    ArchiveRuntimeRequirement,
    /// A reachable import no source resolved to a module (§7): any declared
    /// distribution may provide it (#911).
    UnresolvedImport,
    /// The same in a public module of a library that no root reaches, whose
    /// imports CHK002 counts (#942).
    UnresolvedPublicImport,
    /// The same in a PEP 723 script, whose own block installs what it
    /// imports: it lowers that script's findings alone (#911).
    UnresolvedScriptImport,
    /// A task file or tool config runs Python code no entry reaches: a
    /// computed or unread target, or a file or module it names that nothing
    /// else reaches (#913). What that code imports is unknown.
    UnreadCommandTarget,
    /// A root `[tool.uv.sources]` directory without a `[project]` table: its
    /// code is analyzed as the root's, but its own entries, which may reach
    /// any file under it, and declarations, which may declare any import
    /// there, are not read (#1000).
    UnreadPathSource,
    /// A distribution a file under such a directory imports, at its first
    /// import: an entry of the source may run that file (#1000).
    UnreadPathSourceImport,
    /// A Dockerfile below the root runs a server app found only beside it
    /// (#1061): the module runs in the image's own tree, whose installs and
    /// layout (`RUN pip install`, `COPY`, `WORKDIR`) are not read, so an
    /// import under the Dockerfile's directory may be installed or importable
    /// there.
    UnreadImageTree,
    /// A Django project's settings could not be found or read, or star-import
    /// a module that is not a project file (#914): the apps, middleware and
    /// URL configuration they name are unknown.
    UnreadDjangoSettings,
    /// A Django backend setting (`DATABASES`, `CACHES`) has a value not read
    /// (#1240): it may select any driver the setting's backends load, which
    /// `target` names by setting, and loads no project file.
    UnreadBackendSetting,
    /// A Sphinx `conf.py`, or a module it star-imports, sets `extensions`
    /// in a form not read, or star-imports a module not followed where
    /// neither assigns `extensions` (#1062): the extensions it loads are
    /// unknown, and an extension may be any file or distribution.
    UnreadSphinxExtensions,
    /// The root `requires-python` could not be read, so the assumed py311
    /// decided that a module only other minors ship is not stdlib (#924):
    /// the import's findings may be wrong.
    AssumedTargetVersion,
    /// A PEP 723 script's `dependencies` line that may declare the
    /// distribution it names could not be parsed (#936): that script's
    /// import of it may be declared.
    InvalidScriptRequirement,
    /// A PEP 723 script's `requires-python` could not be read, so the
    /// project's target decided that a module only other minors ship is not
    /// stdlib (#936): that script's import of it may be wrong.
    AssumedScriptTargetVersion,
    /// A string literal in any file is the dotted name of a module no entry
    /// reaches (#1026): code may load it by that name (airflow's
    /// `lazy_load_command("pkg.cli.x.cmd")`, gunicorn's `"python:pkg.config"`),
    /// which the graph does not follow. It lowers that file's CHK001 alone,
    /// to an info note at likely: a name is weaker evidence than a command
    /// that runs the file.
    StringModuleRef,
    /// A string literal in a reachable runtime module is the dotted name of a
    /// module a distribution provides (#1266): code may load it by that name
    /// (a list of extensions handed to `importlib.import_module`), which no
    /// import shows. `target` holds the distribution, whose CHK002 alone it
    /// lowers to an info note at likely, as [`Self::StringModuleRef`] does.
    StringDistributionRef,
}

/// How a gap changes a finding in its scope.
enum Lowering {
    /// Confidence falls to at most this level.
    Cap(Confidence),
    /// The finding becomes an info hint, with confidence at most this level:
    /// `Maybe` where the gap may hold what the finding says is missing,
    /// `Likely` where a name may merely load the file it reports (#1026).
    Hint(Confidence),
}

impl GapKind {
    /// The scope each kind declares: what it lowers and how, if at all.
    ///
    /// Unread code may import any file or distribution, so it lowers the
    /// absence findings; an unknown declaration list may declare any import,
    /// so it lowers the missing-declaration findings to hints; an unread extra
    /// may declare one only as optional, beside a runtime list that was read,
    /// so it caps them and the misplaced ones (#975); an unresolved import may
    /// name what an unused declaration provides. A uv path source without
    /// `[project]` is both under its directory: its entries and its
    /// declaration list are unread (#1000). An image's own tree is the
    /// latter, and its layout is unread too, so it also lowers what does not
    /// resolve there (#1061). A script's own block is read
    /// separately, so no project manifest gap reaches its findings; what the
    /// block itself could not read lowers that script's alone (#936). A
    /// string naming a module may load it, so it lowers that file's absence
    /// finding to a note (#1026).
    #[expect(
        clippy::too_many_lines,
        reason = "one table, so each gap's lowering is read in one place"
    )]
    const fn lowering(self, candidate: &IssueCandidate) -> Option<Lowering> {
        match (self, candidate.rule, &candidate.subject) {
            (
                Self::UnreadModule
                | Self::SkippedEntry
                | Self::NoEntryRoot
                | Self::UnreadNotebookCells
                | Self::UnresolvedRelativeImport
                | Self::OpaqueDynamicImport
                | Self::UnreadCommandTarget
                | Self::UnreadDjangoSettings
                | Self::UnreadSphinxExtensions,
                RuleId::Chk001,
                _,
            )
            | (
                Self::UnreadModule
                | Self::SkippedEntry
                | Self::NoEntryRoot
                | Self::UnreadPublicModule
                | Self::UnreadNotebookCells
                | Self::UnreadDjangoSettings
                | Self::UnreadSphinxExtensions,
                RuleId::Chk002,
                IssueSubject::Distribution { .. },
            )
            | (
                Self::UnresolvedImport | Self::UnresolvedPublicImport,
                RuleId::Chk002,
                IssueSubject::Distribution { .. },
            )
            | (
                Self::UnresolvedScriptImport,
                RuleId::Chk002,
                IssueSubject::ScriptDistribution { .. },
            )
            | (Self::UnknownExtras, RuleId::Chk003 | RuleId::Chk004, IssueSubject::Import { .. })
            | (Self::UnknownExtras, RuleId::Chk005, IssueSubject::Distribution { .. })
            | (Self::UnreadPathSource, RuleId::Chk001, IssueSubject::File { .. })
            | (
                Self::UnreadPathSourceImport | Self::UnreadBackendSetting,
                RuleId::Chk002,
                IssueSubject::Distribution { .. },
            ) => Some(Lowering::Cap(Confidence::Likely)),
            (
                Self::RuntimeDependenciesUnknown
                | Self::InvalidRequirement
                | Self::InvalidRuntimeRequirement,
                RuleId::Chk003 | RuleId::Chk004,
                IssueSubject::Import { .. },
            )
            | (
                Self::ArchiveRequirement | Self::ArchiveRuntimeRequirement | Self::UnreadImageTree,
                RuleId::Chk003 | RuleId::Chk004 | RuleId::Chk010,
                IssueSubject::Import { .. },
            )
            | (
                Self::RuntimeDependenciesUnknown
                | Self::InvalidRuntimeRequirement
                | Self::ArchiveRuntimeRequirement
                | Self::UnreadPathSource
                | Self::UnreadImageTree,
                RuleId::Chk005,
                IssueSubject::Distribution { .. },
            )
            | (
                Self::UnreadPathSource,
                RuleId::Chk003 | RuleId::Chk004,
                IssueSubject::Import { .. },
            )
            | (
                Self::AssumedTargetVersion | Self::AssumedScriptTargetVersion,
                RuleId::Chk003 | RuleId::Chk004 | RuleId::Chk010,
                IssueSubject::Import { .. },
            )
            | (
                Self::AssumedTargetVersion
                | Self::AssumedScriptTargetVersion
                | Self::InvalidScriptRequirement,
                RuleId::Chk003,
                IssueSubject::ScriptDistribution { .. },
            ) => Some(Lowering::Hint(Confidence::Maybe)),
            (Self::StringModuleRef, RuleId::Chk001, IssueSubject::File { .. })
            | (Self::StringDistributionRef, RuleId::Chk002, IssueSubject::Distribution { .. }) => {
                Some(Lowering::Hint(Confidence::Likely))
            },
            _ => None,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::UnreadModule => "a reachable module could not be read",
            Self::SkippedEntry => "an entry point is not a discovered source file",
            Self::NoEntryRoot => "no entry root was found, so no file is known to run",
            Self::UnreadPublicModule => "a public module of the library could not be read",
            Self::UnreadNotebookCells => {
                "some code cells of a reachable notebook could not be read"
            },
            Self::UnresolvedRelativeImport => {
                "a reachable module has a relative import that could not be resolved"
            },
            Self::OpaqueDynamicImport => {
                "a reachable module imports a module by a name computed at runtime"
            },
            Self::RuntimeDependenciesUnknown => "runtime dependencies could not be read statically",
            Self::UnknownExtras => "extras could not be read statically",
            Self::InvalidRequirement | Self::InvalidRuntimeRequirement => {
                "a requirement line naming the distribution could not be read"
            },
            Self::ArchiveRequirement | Self::ArchiveRuntimeRequirement => {
                "a requirement line installs an archive named for the distribution, which is \
                 not read"
            },
            Self::UnresolvedImport => "a reachable import could not be resolved to a distribution",
            Self::UnresolvedPublicImport => {
                "an import in a public module of the library could not be resolved to a \
                 distribution"
            },
            Self::UnresolvedScriptImport => {
                "an import in the script could not be resolved to a distribution"
            },
            Self::UnreadCommandTarget => "a command runs Python code no entry reaches",
            Self::UnreadPathSource => {
                "a uv path source has no `[project]` table, so its entries and dependencies were \
                 not read"
            },
            Self::UnreadPathSourceImport => {
                "a file under a uv path source with no `[project]` table imports the \
                 distribution, and the source's entries were not read"
            },
            Self::UnreadImageTree => {
                "a Dockerfile runs the module in its image's own tree, whose installs and layout \
                 were not read"
            },
            Self::UnreadDjangoSettings => "the Django settings could not be found or read",
            Self::UnreadBackendSetting => {
                "a Django backend setting that may select the distribution could not be read"
            },
            Self::UnreadSphinxExtensions => "the Sphinx `extensions` could not all be read",
            Self::AssumedTargetVersion => {
                "`requires-python` could not be read, so py311 decided the module is not stdlib"
            },
            Self::InvalidScriptRequirement => {
                "a script dependency line naming the distribution could not be read"
            },
            Self::AssumedScriptTargetVersion => {
                "the script's `requires-python` could not be read, so the project's target \
                 decided the module is not stdlib"
            },
            Self::StringModuleRef => {
                "a string literal names the module, which may be loaded by name"
            },
            Self::StringDistributionRef => {
                "a string literal in a reachable module names a module of the distribution, which \
                 may be loaded by name"
            },
        }
    }
}

impl Gap {
    /// Whether the gap is in `candidate`'s scope: a member's gap reaches that
    /// member's findings, a script's import or block that script's alone, an
    /// assumed stdlib status the imports of that root in that file (and the
    /// script declarations they miss), an invalid requirement the findings
    /// on the distribution it names, an archive requirement those and the
    /// unresolved imports whose root is that name, and a string naming a
    /// module that file alone.
    fn reaches(&self, candidate: &IssueCandidate) -> bool {
        let in_member = self.member.is_none() || self.member == candidate.workspace_member;
        let in_site = match (self.kind, &candidate.subject) {
            (GapKind::UnresolvedScriptImport, IssueSubject::ScriptDistribution { script, .. }) => {
                *script == self.file
            },
            (
                GapKind::AssumedTargetVersion | GapKind::AssumedScriptTargetVersion,
                IssueSubject::Import { module, file, .. },
            ) => *file == self.file && self.names_root(module),
            (
                GapKind::AssumedTargetVersion | GapKind::AssumedScriptTargetVersion,
                IssueSubject::ScriptDistribution { script, .. },
            ) => {
                *script == self.file
                    && candidate.origins.iter().any(|origin| {
                        matches!(origin, Origin::Import { module, .. } if self.names_root(module))
                    })
            },
            (GapKind::InvalidScriptRequirement, IssueSubject::ScriptDistribution { script, name }) => {
                *script == self.file && self.site_is(name)
            },
            (
                GapKind::ArchiveRequirement | GapKind::ArchiveRuntimeRequirement,
                IssueSubject::Import {
                    distribution: None,
                    module,
                    ..
                },
            ) => self.site_is(&normalize_distribution_name(import_root(module))),
            (
                GapKind::InvalidRequirement
                | GapKind::InvalidRuntimeRequirement
                | GapKind::ArchiveRequirement
                | GapKind::ArchiveRuntimeRequirement,
                IssueSubject::Import { distribution, .. },
            ) => distribution.as_deref().is_some_and(|name| self.site_is(name)),
            (
                GapKind::InvalidRuntimeRequirement
                | GapKind::ArchiveRuntimeRequirement
                | GapKind::UnreadPathSourceImport,
                IssueSubject::Distribution { name },
            ) => self.site_is(name),
            (
                GapKind::UnreadPathSource,
                IssueSubject::File { path: file } | IssueSubject::Import { file, .. },
            ) => is_under(file, &self.file),
            (GapKind::UnreadPathSource, IssueSubject::Distribution { .. }) => {
                candidate.origins.iter().any(
                    |origin| matches!(origin, Origin::Import { file, .. } if is_under(file, &self.file)),
                )
            },
            (GapKind::UnreadImageTree, IssueSubject::Import { file, .. }) => {
                is_under(file, self.image_dir())
            },
            (GapKind::UnreadImageTree, IssueSubject::Distribution { .. }) => {
                candidate.origins.iter().any(
                    |origin| matches!(origin, Origin::Import { file, .. } if is_under(file, self.image_dir())),
                )
            },
            (GapKind::UnreadBackendSetting, IssueSubject::Distribution { name }) => self
                .target
                .as_deref()
                .is_some_and(|setting| setting_may_load(setting, name)),
            (GapKind::UnresolvedRelativeImport, IssueSubject::File { path }) => {
                self.target.as_deref().is_some_and(|dir| is_under(path, dir))
            },
            (GapKind::StringModuleRef, subject) => {
                matches!(subject, IssueSubject::File { path } if self.target.as_deref() == Some(path))
            },
            (GapKind::StringDistributionRef, subject) => {
                matches!(subject, IssueSubject::Distribution { name } if self.target.as_deref() == Some(name))
            },
            _ => true,
        };
        in_member && in_site
    }

    /// The directory of an image's Dockerfile, its usual build context.
    fn image_dir(&self) -> &str {
        self.file.rsplit_once('/').map_or("", |(dir, _)| dir)
    }

    /// Whether the site is the root of `module`.
    fn names_root(&self, module: &str) -> bool {
        self.site_is(import_root(module))
    }

    fn site_is(&self, name: &str) -> bool {
        self.site.as_ref().is_some_and(|(site, _)| site == name)
    }

    /// What `--explain` names: the import or target and its line, or else
    /// the file; nothing for a gap of the whole project.
    fn label(&self) -> String {
        match &self.site {
            Some((what, line)) => format!("`{what}` in `{}`", import_site(&self.file, *line)),
            None if self.file.is_empty() => String::new(),
            None => format!("`{}`", self.file),
        }
    }
}

/// Whether a backend `setting` may select `peer`, or a driver that provides
/// it (`hiredis` for the cache backend's `redis`).
fn setting_may_load(setting: &str, peer: &str) -> bool {
    let selects = |peer: &str| {
        RUNTIME_PEERS
            .iter()
            .any(|entry| entry.peer == peer && entry.settings.contains(&setting))
    };
    selects(peer)
        || RUNTIME_PEERS.iter().any(|entry| {
            entry.peer == peer && entry.providers.iter().any(|provider| selects(provider))
        })
}

/// Gaps that can affect a finding.
///
/// One in a module that never runs hides nothing, so module and import gaps
/// count only where the module is reached or, for an unread module, where a
/// caller of the library may import it. `skipped_entries` are the entry
/// paths skipped as not discovered, less those that do not exist, which run
/// nothing; `no_entry_root` says an app has no root at all (#1138).
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
pub fn collect_gaps(
    context: &RuleContext<'_>,
    manifest: &LoadedManifest,
    members: &[MemberManifest<'_>],
    scripts: &[InlineScript],
    plugins: &PluginHints,
    skipped_entries: &[String],
    no_entry_root: bool,
    unread_path_sources: &[String],
    groups: &DependencyGroupsConfig,
    project_range: StdlibRange,
) -> Vec<Gap> {
    let mut gaps = module_gaps(context.parse, context.graph, context.reachability);
    gaps.extend(skipped_entries.iter().map(|path| Gap {
        file: path.clone(),
        kind: GapKind::SkippedEntry,
        member: None,
        target: None,
        site: None,
    }));
    // The whole project: no file to name.
    gaps.extend(no_entry_root.then(|| Gap {
        file: String::new(),
        kind: GapKind::NoEntryRoot,
        member: None,
        target: None,
        site: None,
    }));
    gaps.extend(unread_public_module_gaps(context.reachability));
    gaps.extend(unresolved_import_gaps(context, manifest, scripts));
    gaps.extend(command_target_gaps(
        context,
        &plugins.config_command_targets,
    ));
    gaps.extend(image_tree_gaps(context, &plugins.config_command_targets));
    gaps.extend(string_module_ref_gaps(context));
    gaps.extend(string_distribution_ref_gaps(context));
    gaps.extend(
        plugins
            .unread_settings()
            .map(|(plugin, unread)| settings_gap(plugin, unread)),
    );
    gaps.extend(path_source_gaps(
        &context.resolution.imports,
        unread_path_sources,
    ));
    gaps.extend(manifest_gap(manifest));
    gaps.extend(extras_gap(manifest));
    gaps.extend(invalid_requirement_gaps(manifest, groups));
    gaps.extend(archive_requirement_gaps(manifest, groups));
    gaps.extend(assumed_target_gaps(context.parse, manifest, scripts));
    gaps.extend(script_gaps(context.parse, project_range, scripts));
    gaps.extend(members.iter().flat_map(|member| {
        let manifest = member.manifest;
        manifest_gap(manifest)
            .into_iter()
            .chain(extras_gap(manifest))
            .chain(invalid_requirement_gaps(manifest, groups))
            .chain(archive_requirement_gaps(manifest, groups))
            .map(|mut gap| {
                gap.file = join_rel(member.path, &gap.file);
                gap.member = Some(member.id.to_owned());
                gap
            })
    }));
    // The root's gap sorts before its members', so it is the one `--explain` names first.
    gaps.sort_by(|left, right| {
        (left.kind, left.member.is_some(), &left.file, &left.site).cmp(&(
            right.kind,
            right.member.is_some(),
            &right.file,
            &right.site,
        ))
    });
    gaps
}

fn is_reached(file: &str, graph: &ProjectGraph, reachability: &ReachabilityReport) -> bool {
    graph
        .file_id(file)
        .is_some_and(|file_id| reachability.reachable.contains(&file_id))
}

/// Whether a caller of the library may import `file`, which no root reaches:
/// CHK002 counts its imports (#501), so an import it could not resolve lowers
/// CHK002 (#942).
fn is_public_orphan(file: &str, reachability: &ReachabilityReport) -> bool {
    reachability
        .unreachable
        .iter()
        .any(|orphan| orphan.path == file && orphan.callers_may_import())
}

fn module_gaps(
    parse: &ParseSummary,
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for module in &parse.modules {
        if !is_reached(&module.path, graph, reachability) {
            continue;
        }
        let mut push = |kind| {
            gaps.push(Gap {
                file: module.path.clone(),
                kind,
                member: None,
                target: None,
                site: None,
            });
        };
        if module.skipped {
            push(GapKind::UnreadModule);
        }
        if module.has_unread_cells {
            push(GapKind::UnreadNotebookCells);
        }
        if module.has_opaque_dynamic_import {
            push(GapKind::OpaqueDynamicImport);
        }
        gaps.extend(unresolved_relative_gaps(module));
    }
    gaps
}

/// One gap per directory the module's unresolved relative imports climb to,
/// at the first such import (#1113). The parser records an import it could
/// not resolve with an empty module and its dots; `level` dots name the
/// file's directory and the `level - 1` above it, as `__package__` would
/// for a module run from there. Only a file the layout names no module for
/// may still run the import: a top-level module's, or one climbing past its
/// package or the directories above it, raises `ImportError` and loads
/// nothing.
fn unresolved_relative_gaps(module: &ParsedModule) -> Vec<Gap> {
    let unplaced: BTreeSet<u32> = module
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message == NOT_IN_PACKAGE)
        .map(|diagnostic| diagnostic.line)
        .collect();
    let mut sites: BTreeMap<&str, (u8, u32)> = BTreeMap::new();
    for import in &module.imports {
        if !import.module.is_empty() || !unplaced.contains(&import.line) {
            continue;
        }
        let Some(dir) = package_dir(&module.path, import.relative_level) else {
            continue;
        };
        // One file reaches a directory with one dot count only.
        let site = sites
            .entry(dir)
            .or_insert((import.relative_level, import.line));
        site.1 = site.1.min(import.line);
    }
    sites
        .into_iter()
        .map(|(dir, (level, line))| Gap {
            file: module.path.clone(),
            kind: GapKind::UnresolvedRelativeImport,
            member: None,
            site: Some((".".repeat(usize::from(level)), Some(line))),
            target: Some(dir.to_owned()),
        })
        .collect()
}

/// The directory `level` dots name from the file at `path`: its own
/// directory, then one up per further dot. `None` past the file's top-level
/// directory, which is the outermost package the file can be imported in.
fn package_dir(path: &str, level: u8) -> Option<&str> {
    let (mut dir, _) = path.rsplit_once('/')?;
    for _ in 1..level {
        (dir, _) = dir.rsplit_once('/')?;
    }
    (level > 0).then_some(dir)
}

/// Skipped modules no root reaches that a caller of the library may import:
/// CHK002 counts the imports of the public orphans it could read (#501), so
/// it must allow for the ones it could not.
fn unread_public_module_gaps(reachability: &ReachabilityReport) -> Vec<Gap> {
    reachability
        .unread
        .iter()
        .filter(|file| file.callers_may_import())
        .map(|file| Gap {
            file: file.path.clone(),
            kind: GapKind::UnreadPublicModule,
            member: None,
            target: None,
            site: None,
        })
        .collect()
}

/// One gap per file and module, in a reachable file or a public orphan whose
/// imports CHK002 counts (#942), at the import sites CHK010 warns about: a
/// `TYPE_CHECKING` or optional import never fails, a docs or dev file runs
/// outside the installed package, and a first-party miss is a typo rather
/// than a distribution (§12). A script's block installs what the script runs
/// with, so its imports count in any context.
fn unresolved_import_gaps(
    context: &RuleContext<'_>,
    manifest: &LoadedManifest,
    scripts: &[InlineScript],
) -> Vec<Gap> {
    let is_script = |file: &str| scripts.iter().any(|script| script.path == file);
    let mut sites: BTreeMap<(&str, &str), Option<u32>> = BTreeMap::new();
    for warning in &context.resolution.warnings {
        let ResolveWarning::UnresolvedImport {
            import,
            file,
            line,
            context: import_context,
            optional,
            ..
        } = warning
        else {
            continue;
        };
        let counts = *import_context != ImportContext::Type
            && !*optional
            && (is_reached(file, context.graph, context.reachability)
                || is_public_orphan(file, context.reachability))
            && (is_script(file)
                || !matches!(
                    file_context(file, context.sources),
                    FileContext::Docs | FileContext::Dev
                ))
            && !is_first_party_import(import, &context.sources.layout, &manifest.metadata);
        if !counts {
            continue;
        }
        keep_first_line(&mut sites, (file.as_str(), import.as_str()), *line);
    }
    sites
        .into_iter()
        .map(|((file, import), line)| Gap {
            file: file.to_owned(),
            kind: if is_script(file) {
                GapKind::UnresolvedScriptImport
            } else if is_reached(file, context.graph, context.reachability) {
                GapKind::UnresolvedImport
            } else {
                GapKind::UnresolvedPublicImport
            },
            member: None,
            target: None,
            site: Some((import.to_owned(), line)),
        })
        .collect()
}

/// Keep the first line of a site, as CHK010 anchors at it; a site without
/// one sorts last.
fn keep_first_line<K: Ord>(sites: &mut BTreeMap<K, Option<u32>>, key: K, line: Option<u32>) {
    let kept = sites.entry(key).or_insert(line);
    *kept = match (*kept, line) {
        (Some(kept), Some(line)) => Some(kept.min(line)),
        (kept, line) => kept.or(line),
    };
}

/// One gap per command file and target. A computed or unread target may run
/// any file; a file or module it names that no entry reaches runs code whose
/// imports are not followed. Scripts, `-m` modules (#912) and server apps
/// (#1004) are entries, so that is a module a task runner loads, `-c` code
/// imports, or a relocated `-m` names, and under `--production` a dev
/// command's script or module, whose entry is held back. A target that names
/// nothing discovered (a typo, a deleted file, a third-party module such as
/// `pytest`) hides no project file, so it is no gap.
fn command_target_gaps(context: &RuleContext<'_>, commands: &[CommandTargetRef]) -> Vec<Gap> {
    let known_paths: BTreeSet<String> = context
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let unreached = |path: &String| !is_reached(path, context.graph, context.reachability);
    let mut sites: BTreeMap<(&str, String), Option<u32>> = BTreeMap::new();
    for command in commands {
        let text = match &command.target {
            CommandTarget::Opaque { text } => text.clone(),
            CommandTarget::Script { paths } => {
                match paths.iter().find(|path| known_paths.contains(*path)) {
                    Some(path) if unreached(path) => path.clone(),
                    _ => continue,
                }
            },
            CommandTarget::Run { module } | CommandTarget::Module { module } => {
                let layout = &context.sources.layout;
                let path =
                    resolve_module_to_path(&format!("{module}.__main__"), layout, &known_paths)
                        .or_else(|| resolve_module_to_path(module, layout, &known_paths));
                match path {
                    Some(path) if unreached(&path) => module.clone(),
                    _ => continue,
                }
            },
            // An app that resolves to an analysed file is an entry, so it is
            // reached.
            CommandTarget::App { .. } => continue,
        };
        keep_first_line(
            &mut sites,
            (command.origin.file.as_str(), text),
            command.origin.line,
        );
    }
    sites
        .into_iter()
        .map(|((file, text), line)| Gap {
            file: file.to_owned(),
            kind: GapKind::UnreadCommandTarget,
            member: None,
            target: None,
            site: Some((text, line)),
        })
        .collect()
}

/// One gap per Dockerfile below the root and server app it runs that the
/// root's import roots do not resolve but the Dockerfile's directory does
/// (#1061), at its first line: that tree is the image's, not the root's. An
/// app the root resolves is the root's code, so its imports keep their
/// findings.
fn image_tree_gaps(context: &RuleContext<'_>, commands: &[CommandTargetRef]) -> Vec<Gap> {
    let known_paths: BTreeSet<String> = context
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let mut sites: BTreeMap<(&str, String), Option<u32>> = BTreeMap::new();
    for command in commands {
        let CommandTarget::App { module, .. } = &command.target else {
            continue;
        };
        let file = command.origin.file.as_str();
        let own_tree = is_dockerfile(Path::new(file))
            && resolve_module_to_path(module, &context.sources.layout, &known_paths).is_none()
            && beside_origin(module, file, &known_paths).is_some();
        if own_tree {
            keep_first_line(&mut sites, (file, module.clone()), command.origin.line);
        }
    }
    sites
        .into_iter()
        .map(|((file, module), line)| Gap {
            file: file.to_owned(),
            kind: GapKind::UnreadImageTree,
            member: None,
            target: None,
            site: Some((module, line)),
        })
        .collect()
}

/// A string literal that is a whole dotted module name may load the module
/// it names (airflow's `lazy_load_command("airflow.cli.commands.x.cmd")`,
/// gunicorn's `"python:pkg.config"`), which the graph does not follow: one
/// gap per such literal, at its first line in each file, whose name or a
/// shorter prefix of at least two segments (`pkg.mod.attr` names `pkg.mod`,
/// never bare `pkg`) resolves to a project file no entry reaches, scoped to
/// that file (#1026). A literal in any file counts, reached or not, since
/// the lowering only says the file may be loaded; a file's own name lowers
/// nothing. A name that resolves to nothing (a third-party module, a
/// sentence of identifiers) hides no project file, so it is no gap.
fn string_module_ref_gaps(context: &RuleContext<'_>) -> Vec<Gap> {
    let known_paths: BTreeSet<String> = context
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let index = ModuleIndex::new(&known_paths, &context.sources.layout);
    // Every dotted string literal is a lookup: spread the files over workers,
    // keeping their order (#1087).
    let per_module = crate::parallel::map_in_order(&context.parse.modules, |module| {
        let mut gaps = Vec::new();
        for reference in &module.module_refs {
            let name = crate::parser::module_ref_name(&reference.text);
            let Some(target) = resolve_dotted_prefix(name, &index) else {
                continue;
            };
            if target == module.path || is_reached(&target, context.graph, context.reachability) {
                continue;
            }
            gaps.push(Gap {
                file: module.path.clone(),
                kind: GapKind::StringModuleRef,
                member: None,
                site: Some((reference.text.clone(), Some(reference.line))),
                target: Some(target),
            });
        }
        gaps
    });
    per_module.into_iter().flatten().collect()
}

/// One gap per distribution a dotted-name string literal in a reachable
/// runtime module names, at its first literal by file and line (#1266). The
/// resolver keeps only the literals that resolve to a distribution, so a
/// first-party, stdlib or unknown name is none; a literal in a public module
/// of a library counts as its imports do for CHK002 (#501). A name that no
/// declaration provides lowers nothing, as no CHK002 names it.
fn string_distribution_ref_gaps(context: &RuleContext<'_>) -> Vec<Gap> {
    let public_orphans: BTreeSet<&str> = context
        .reachability
        .unreachable
        .iter()
        .filter(|orphan| orphan.callers_may_import())
        .map(|orphan| orphan.path.as_str())
        .collect();
    let mut first: BTreeMap<&str, (&str, Option<u32>, &str)> = BTreeMap::new();
    for reference in &context.resolution.string_refs {
        let Some(distribution) = reference.distribution.as_deref() else {
            continue;
        };
        let file = reference.file.as_str();
        if !is_reached(file, context.graph, context.reachability) && !public_orphans.contains(file)
        {
            continue;
        }
        let site = (file, reference.line, reference.full_module.as_str());
        first
            .entry(distribution)
            .and_modify(|kept| *kept = std::cmp::min(*kept, site))
            .or_insert(site);
    }
    first
        .into_iter()
        .map(|(distribution, (file, line, module))| Gap {
            file: file.to_owned(),
            kind: GapKind::StringDistributionRef,
            member: None,
            site: Some((module.to_owned(), line)),
            target: Some(distribution.to_owned()),
        })
        .collect()
}

/// The project file `module` names, or else the one its longest prefix of
/// at least two segments names. A package's `__init__.py` is never a CHK001
/// candidate, so a name that stops at a package lowers nothing.
fn resolve_dotted_prefix(module: &str, index: &ModuleIndex<'_>) -> Option<String> {
    let mut candidate = module;
    loop {
        if let Some(path) = index.resolve(candidate) {
            return (!path.ends_with("__init__.py")).then_some(path);
        }
        let (prefix, _) = candidate.rsplit_once('.')?;
        if !prefix.contains('.') {
            return None;
        }
        candidate = prefix;
    }
}

/// Whether root-relative `path` is below directory `dir`.
fn is_under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// One gap per unread path source directory, and one per distribution a file
/// under one imports, at its first import: an entry of the source may run
/// any file under it, reached from the root or not.
fn path_source_gaps(imports: &[Arc<ResolvedImport>], dirs: &[String]) -> Vec<Gap> {
    let mut first: BTreeMap<&str, (&str, Option<u32>)> = BTreeMap::new();
    for import in imports {
        let Some(distribution) = import.distribution.as_deref() else {
            continue;
        };
        if !dirs.iter().any(|dir| is_under(&import.file, dir)) {
            continue;
        }
        let site = (import.file.as_str(), import.line);
        // A plugin reference without a line sorts last in its file.
        let order = |a: &(&str, Option<u32>), b: &(&str, Option<u32>)| {
            (a.0, a.1.is_none(), a.1).cmp(&(b.0, b.1.is_none(), b.1))
        };
        first
            .entry(distribution)
            .and_modify(|kept| *kept = std::cmp::min_by(*kept, site, order))
            .or_insert(site);
    }
    let dir_gaps = dirs.iter().map(|dir| Gap {
        file: dir.clone(),
        kind: GapKind::UnreadPathSource,
        member: None,
        target: None,
        site: None,
    });
    let import_gaps = first.into_iter().map(|(distribution, (file, line))| Gap {
        file: file.to_owned(),
        kind: GapKind::UnreadPathSourceImport,
        member: None,
        target: None,
        site: Some((distribution.to_owned(), line)),
    });
    dir_gaps.chain(import_gaps).collect()
}

/// Settings decide what Django runs in every member, and an extension
/// `sphinx-build` loads may be any module on its path, so the gap lowers the
/// whole project's findings.
fn settings_gap(plugin: PluginId, unread: &UnreadSettings) -> Gap {
    let kind = match (plugin, &unread.backend) {
        (PluginId::Sphinx, _) => GapKind::UnreadSphinxExtensions,
        (_, Some(_)) => GapKind::UnreadBackendSetting,
        (_, None) => GapKind::UnreadDjangoSettings,
    };
    Gap {
        file: unread.file.clone(),
        kind,
        member: None,
        target: unread.backend.clone(),
        site: unread.site.clone(),
    }
}

/// Each file's imports of a root that only minors py311 does not reach ship
/// as stdlib, when the root `requires-python` could not be read (#924): one
/// gap per root at its first line. A PEP 723 script with its own target, and
/// a member, whose `requires-python` the resolver does not use, assume nothing.
fn assumed_target_gaps(
    parse: &ParseSummary,
    manifest: &LoadedManifest,
    scripts: &[InlineScript],
) -> Vec<Gap> {
    let unreadable = manifest
        .warnings
        .iter()
        .any(|warning| matches!(warning, ManifestWarning::RequiresPythonUnreadable { .. }));
    if !unreadable {
        return Vec::new();
    }
    let assumed = StdlibRange::new(
        &TargetVersion::default_py311(),
        manifest.metadata.requires_python.as_deref(),
    );
    let mut gaps = Vec::new();
    for module in &parse.modules {
        let own_target = scripts
            .iter()
            .any(|script| script.path == module.path && script.target_version.is_some());
        if own_target {
            continue;
        }
        gaps.extend(
            version_dependent_roots(module, assumed)
                .into_iter()
                .map(|(root, line)| Gap {
                    file: module.path.clone(),
                    kind: GapKind::AssumedTargetVersion,
                    member: None,
                    target: None,
                    site: Some((root.to_owned(), Some(line))),
                }),
        );
    }
    gaps
}

/// Each absolute import root of `module` that `range` does not ship as stdlib
/// although another minor does, at its first line.
fn version_dependent_roots(module: &ParsedModule, range: StdlibRange) -> BTreeMap<&str, u32> {
    let mut first: BTreeMap<&str, u32> = BTreeMap::new();
    for import in &module.imports {
        let root = import_root(&import.module);
        if import.relative_level == 0 && !root.is_empty() && range.ships_only_outside(root) {
            let line = first.entry(root).or_insert(import.line);
            *line = (*line).min(import.line);
        }
    }
    first
}

/// What a PEP 723 block itself could not read, scoped to that script (#936):
/// each invalid `dependencies` line that names a distribution, and, when
/// `requires-python` is unreadable, each root of the script's imports that
/// the project's range, which the resolver used instead, does not ship as
/// stdlib although another minor does (one gap per root at its first line).
fn script_gaps(
    parse: &ParseSummary,
    project_range: StdlibRange,
    scripts: &[InlineScript],
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for script in scripts {
        gaps.extend(
            script
                .invalid_requirements
                .iter()
                .filter_map(|(raw, line)| {
                    Some(Gap {
                        file: script.path.clone(),
                        kind: GapKind::InvalidScriptRequirement,
                        member: None,
                        target: None,
                        site: Some((invalid_requirement_name(raw)?, *line)),
                    })
                }),
        );
        if !script.requires_python_unreadable() {
            continue;
        }
        let Some(module) = parse
            .modules
            .iter()
            .find(|module| module.path == script.path)
        else {
            continue;
        };
        let roots = version_dependent_roots(module, project_range);
        gaps.extend(roots.into_iter().map(|(root, line)| Gap {
            file: script.path.clone(),
            kind: GapKind::AssumedScriptTargetVersion,
            member: None,
            target: None,
            site: Some((root.to_owned(), Some(line))),
        }));
    }
    gaps
}

/// The manifest's warning names the file (`pyproject.toml` for
/// `dynamic = ["dependencies"]`); the fallback keeps the gap if one is missing.
fn manifest_gap(manifest: &LoadedManifest) -> Option<Gap> {
    if !manifest.sources.runtime_dependencies_unknown {
        return None;
    }
    let file = manifest
        .warnings
        .iter()
        .find_map(|warning| match warning {
            ManifestWarning::RuntimeDependenciesUnknown { file } => Some(file.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "pyproject.toml".to_owned());
    Some(Gap {
        file,
        kind: GapKind::RuntimeDependenciesUnknown,
        member: None,
        target: None,
        site: None,
    })
}

/// The warning that reported the unread extras names the file and, for
/// setup.py, the argument; the fallback keeps the gap if none does.
fn extras_gap(manifest: &LoadedManifest) -> Option<Gap> {
    if manifest.sources.extras == ExtrasState::Known {
        return None;
    }
    let reported = manifest.warnings.iter().find_map(|warning| match warning {
        ManifestWarning::SetupPyPartiallyStatic { file, argument }
            if argument.starts_with("extras_require") =>
        {
            Some((file.clone(), Some((argument.clone(), None))))
        },
        ManifestWarning::DynamicExtrasUnread { file, .. }
        | ManifestWarning::SetupPyNotStatic { file } => Some((file.clone(), None)),
        _ => None,
    });
    let (file, site) = reported.unwrap_or_else(|| {
        let file = if manifest.sources.setup_cfg && !manifest.sources.setup_py {
            "setup.cfg"
        } else {
            "setup.py"
        };
        (file.to_owned(), None)
    });
    Some(Gap {
        file,
        kind: GapKind::UnknownExtras,
        member: None,
        target: None,
        site,
    })
}

/// One gap per invalid requirement line that names a distribution (#928).
/// A line with no name (`>=1.0`, `{{ name }}>=1`) lowers nothing: it names
/// no distribution, and reading it as an unread manifest would turn every
/// missing declaration into a hint for one stray line. Constraint lines are
/// not told apart from declarations, so they lower the same way. A line in
/// a runtime bucket may also be the runtime declaration CHK005 misses; a dev
/// or type line beside a valid dev one cannot be (#935).
fn invalid_requirement_gaps(
    manifest: &LoadedManifest,
    groups: &DependencyGroupsConfig,
) -> Vec<Gap> {
    manifest
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ManifestWarning::InvalidRequirementLine {
                file,
                line,
                raw,
                context,
                ..
            } => Some(Gap {
                file: file.clone(),
                kind: match declaration_bucket(context, groups) {
                    DeclarationBucket::Runtime | DeclarationBucket::Optional(_) => {
                        GapKind::InvalidRuntimeRequirement
                    },
                    DeclarationBucket::Dev | DeclarationBucket::Type => GapKind::InvalidRequirement,
                },
                member: None,
                target: None,
                site: Some((invalid_requirement_name(raw)?, *line)),
            }),
            _ => None,
        })
        .collect()
}

/// One gap per opaque requirements-file line that installs an archive whose
/// file name names the distribution (#988): `wheels/bar-1.0-py3-none-any.whl`
/// installs `bar`, so an import of `bar` may be declared. A directory or URL
/// line with no such name (`.`, `./vendor/foo`, `git+https://...`) lowers
/// nothing, for the reason [`invalid_requirement_gaps`] gives. The archive is
/// never read, so the name stays a gap rather than a declaration: unused,
/// duplicate and `--fix` keep skipping the line. Its bucket decides the kind
/// as for an invalid line, a runtime one if any bucket it is in is runtime.
fn archive_requirement_gaps(
    manifest: &LoadedManifest,
    groups: &DependencyGroupsConfig,
) -> Vec<Gap> {
    manifest
        .dependencies
        .iter()
        .filter(|dep| dep.opaque && dep.name.is_empty())
        .filter_map(|dep| {
            let name = archive_distribution_name(dep.specifier.as_deref()?)?;
            let runtime = declaration_buckets(dep, groups).iter().any(|bucket| {
                matches!(
                    bucket,
                    DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
                )
            });
            Some(Gap {
                file: dep.origin.file.clone(),
                kind: if runtime {
                    GapKind::ArchiveRuntimeRequirement
                } else {
                    GapKind::ArchiveRequirement
                },
                member: None,
                target: None,
                site: Some((name, dep.origin.line)),
            })
        })
        .collect()
}

/// Lower each finding a gap's scope covers and name the gap in `--explain`
/// (ADR 0005 §4). Exit codes still follow severity alone (§7). A cap never
/// reaches `Maybe`, so a severity a rule derived from confidence (a library
/// orphan's) still matches the lowered confidence.
pub(super) fn apply_gaps(candidates: &mut [IssueCandidate], gaps: &[Gap]) {
    for candidate in candidates {
        let in_scope: Vec<&Gap> = gaps.iter().filter(|gap| gap.reaches(candidate)).collect();
        let kinds: BTreeSet<GapKind> = in_scope.iter().map(|gap| gap.kind).collect();
        for &kind in &kinds {
            let Some(lowering) = kind.lowering(candidate) else {
                continue;
            };
            match lowering {
                Lowering::Cap(cap) => candidate.confidence = candidate.confidence.min(cap),
                Lowering::Hint(cap) => {
                    candidate.severity = Severity::Info;
                    candidate.confidence = candidate.confidence.min(cap);
                },
            }
            candidate.explain.details.push(gap_detail(kind, &in_scope));
        }
    }
}

/// The gap reason, the first gap's label, and how many more gaps share the kind.
fn gap_detail(kind: GapKind, gaps: &[&Gap]) -> String {
    let mut labels = gaps
        .iter()
        .filter(|gap| gap.kind == kind)
        .map(|gap| gap.label());
    let first = labels.next().unwrap_or_default();
    match labels.count() {
        0 if first.is_empty() => kind.reason().to_owned(),
        0 => format!("{}: {first}", kind.reason()),
        more => format!("{}: {first} (and {more} more)", kind.reason()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::types::ExplainData;

    fn candidate(rule: RuleId, subject: IssueSubject) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: String::new(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData::default(),
        }
    }

    fn file(path: &str) -> IssueSubject {
        IssueSubject::File {
            path: path.to_owned(),
        }
    }

    fn distribution(name: &str) -> IssueSubject {
        IssueSubject::Distribution {
            name: name.to_owned(),
        }
    }

    fn script_distribution(name: &str) -> IssueSubject {
        IssueSubject::ScriptDistribution {
            script: "tool.py".to_owned(),
            name: name.to_owned(),
        }
    }

    fn import(module: &str) -> IssueSubject {
        IssueSubject::Import {
            module: module.to_owned(),
            file: "acme/main.py".to_owned(),
            line: Some(1),
            distribution: None,
        }
    }

    fn gap(file: &str, kind: GapKind) -> Gap {
        Gap {
            file: file.to_owned(),
            kind,
            member: None,
            target: None,
            site: None,
        }
    }

    fn in_member(mut candidate: IssueCandidate, member: &str) -> IssueCandidate {
        candidate.workspace_member = Some(member.to_owned());
        candidate
    }

    fn lowered(gaps: &[Gap], mut candidates: Vec<IssueCandidate>) -> Vec<(Severity, Confidence)> {
        apply_gaps(&mut candidates, gaps);
        candidates
            .iter()
            .map(|candidate| (candidate.severity, candidate.confidence))
            .collect()
    }

    /// An unresolved relative import lowers the CHK001 below the directory
    /// its dots climb to, and nothing else (#1113).
    #[test]
    fn unresolved_relative_import_caps_the_files_below_its_directory() {
        let relative = Gap {
            target: Some("ns".to_owned()),
            site: Some(("..".to_owned(), Some(1))),
            ..gap("ns/sub/a.py", GapKind::UnresolvedRelativeImport)
        };
        let certain = (Severity::Error, Confidence::Certain);
        let likely = (Severity::Error, Confidence::Likely);
        let found = lowered(
            &[relative],
            vec![
                candidate(RuleId::Chk001, file("ns/b.py")),
                candidate(RuleId::Chk001, file("ns/sub/c.py")),
                candidate(RuleId::Chk001, file("nsx/d.py")),
                candidate(RuleId::Chk001, file("other/e.py")),
                candidate(RuleId::Chk002, distribution("requests")),
            ],
        );
        assert_eq!(found, [likely, likely, certain, certain, certain]);
    }

    #[test]
    fn package_dir_climbs_one_directory_per_further_dot() {
        assert_eq!(package_dir("ns/sub/a.py", 1), Some("ns/sub"));
        assert_eq!(package_dir("ns/sub/a.py", 2), Some("ns"));
        assert_eq!(package_dir("ns/sub/a.py", 3), None);
        assert_eq!(package_dir("ns/sub/a.py", 0), None);
        assert_eq!(package_dir("main.py", 1), None);
    }

    /// Only an import the parser left unresolved, on a line it said is
    /// outside a package, opens a gap, at the first such line per directory.
    #[test]
    fn unresolved_relative_gaps_take_the_first_unplaced_import_per_directory() {
        use crate::parser::{ImportKind, ImportRef, ParseDiagnostic, ParseSeverity};
        let import = |module: &str, level: u8, line: u32| ImportRef {
            module: module.to_owned(),
            name: None,
            alias: None,
            line,
            kind: ImportKind::ImportFrom,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            deferred: false,
            relative_level: level,
            fallback_for: Vec::new(),
            version_minors: None,
        };
        let unplaced = |line: u32| ParseDiagnostic {
            line,
            message: NOT_IN_PACKAGE.to_owned(),
            severity: ParseSeverity::Warning,
        };
        let module = ParsedModule {
            path: "ns/sub/a.py".to_owned(),
            imports: vec![
                // Resolved, though its line also holds an unplaced one.
                import("ns.sub.x", 1, 2),
                // Unresolved, but not for being outside a package.
                import("", 2, 3),
                import("", 1, 7),
                import("", 1, 5),
                import("", 1, 9),
            ],
            diagnostics: vec![unplaced(2), unplaced(5), unplaced(7), unplaced(9)],
            ..ParsedModule::default()
        };
        let sites: Vec<_> = unresolved_relative_gaps(&module)
            .into_iter()
            .map(|gap| (gap.target, gap.site))
            .collect();
        assert_eq!(
            sites,
            [(Some("ns/sub".to_owned()), Some((".".to_owned(), Some(5))))]
        );
    }

    #[test]
    fn unread_module_skipped_entry_and_framework_config_cap_absence_findings_but_not_script_ones() {
        for gap in [
            gap("acme/latin.py", GapKind::UnreadModule),
            gap("manage.py", GapKind::SkippedEntry),
            gap("", GapKind::NoEntryRoot),
            gap("manage.py", GapKind::UnreadDjangoSettings),
            gap("docs/conf.py", GapKind::UnreadSphinxExtensions),
        ] {
            let found = lowered(
                &[gap],
                vec![
                    candidate(RuleId::Chk001, file("acme/orphan.py")),
                    candidate(RuleId::Chk002, distribution("requests")),
                    candidate(RuleId::Chk002, script_distribution("rich")),
                    candidate(RuleId::Chk003, import("yaml")),
                ],
            );
            assert_eq!(
                found,
                [
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Certain),
                    (Severity::Error, Confidence::Certain),
                ]
            );
        }
    }

    /// A gap of the whole project names no file in `--explain` (#1138).
    #[test]
    fn no_entry_root_explains_without_a_file() {
        let mut candidates = vec![candidate(RuleId::Chk002, distribution("requests"))];
        apply_gaps(&mut candidates, &[gap("", GapKind::NoEntryRoot)]);
        assert_eq!(
            candidates[0].explain.details,
            ["no entry root was found, so no file is known to run"]
        );
    }

    #[test]
    fn opaque_dynamic_import_and_command_targets_cap_only_unused_files() {
        for gap in [
            gap("acme/main.py", GapKind::OpaqueDynamicImport),
            gap("Makefile", GapKind::UnreadCommandTarget),
        ] {
            let found = lowered(
                &[gap],
                vec![
                    candidate(RuleId::Chk001, file("acme/orphan.py")),
                    candidate(RuleId::Chk002, distribution("requests")),
                ],
            );
            assert_eq!(
                found,
                [
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Certain),
                ]
            );
        }
    }

    /// A string naming a module turns only that file's CHK001 into a likely
    /// hint; other files and distributions keep their level (#1026).
    #[test]
    fn string_module_ref_lowers_only_the_named_file() {
        let mut gap = gap("acme/cli.py", GapKind::StringModuleRef);
        gap.target = Some("acme/commands/cheat_sheet.py".to_owned());
        gap.site = Some(("acme.commands.cheat_sheet.run".to_owned(), Some(2)));
        let found = lowered(
            &[gap],
            vec![
                candidate(RuleId::Chk001, file("acme/commands/cheat_sheet.py")),
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk002, distribution("requests")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// A string naming a distribution's module turns only that
    /// distribution's CHK002 into a likely hint; files and other
    /// distributions keep their level (#1266).
    #[test]
    fn string_distribution_ref_lowers_only_the_named_distribution() {
        let mut gap = gap("acme/app.py", GapKind::StringDistributionRef);
        gap.target = Some("acme-ext-help".to_owned());
        gap.site = Some(("acme_ext.help".to_owned(), Some(3)));
        let found = lowered(
            &[gap],
            vec![
                candidate(RuleId::Chk002, distribution("acme-ext-help")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk001, file("acme/app.py")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// `pkg.mod.attr` resolves through its longest prefix that is a project
    /// file; a bare package name never does, and a name that stops at a
    /// package `__init__.py` resolves to nothing, since CHK001 never reports
    /// one.
    #[test]
    fn dotted_prefixes_resolve_down_to_two_segments() {
        let layout = crate::sources::LayoutInfo {
            packages: vec!["acme".to_owned()],
            ..Default::default()
        };
        let known: BTreeSet<String> = [
            "acme/__init__.py",
            "acme/commands/__init__.py",
            "acme/commands/cheat_sheet.py",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let index = ModuleIndex::new(&known, &layout);
        let resolve = |module: &str| resolve_dotted_prefix(module, &index);
        assert_eq!(
            resolve("acme.commands.cheat_sheet.run").as_deref(),
            Some("acme/commands/cheat_sheet.py")
        );
        assert_eq!(
            resolve("acme.commands.cheat_sheet").as_deref(),
            Some("acme/commands/cheat_sheet.py")
        );
        assert_eq!(resolve("acme.commands.missing.run"), None);
        assert_eq!(resolve("acme.commands"), None);
        assert_eq!(resolve("acme.missing.run"), None);
        assert_eq!(resolve("other.mod"), None);
    }

    /// A path source without `[project]` lowers findings only under its
    /// directory, and its imports cap only the distribution they name (#1000).
    #[test]
    fn unread_path_source_lowers_findings_under_its_directory() {
        let under = |module: &str, file: &str| IssueSubject::Import {
            module: module.to_owned(),
            file: file.to_owned(),
            line: Some(1),
            distribution: None,
        };
        let imported_from = |mut candidate: IssueCandidate, file: &str| {
            candidate.origins.push(Origin::Import {
                file: file.to_owned(),
                line: Some(1),
                module: "httpx".to_owned(),
            });
            candidate
        };
        let found = lowered(
            &[gap("sub", GapKind::UnreadPathSource)],
            vec![
                candidate(RuleId::Chk001, file("sub/a.py")),
                candidate(RuleId::Chk001, file("sub2/a.py")),
                candidate(RuleId::Chk003, under("httpx", "sub/x.py")),
                candidate(RuleId::Chk003, under("httpx", "sub2/x.py")),
                imported_from(candidate(RuleId::Chk005, distribution("httpx")), "sub/x.py"),
                imported_from(
                    candidate(RuleId::Chk005, distribution("httpx")),
                    "sub2/x.py",
                ),
                candidate(RuleId::Chk002, distribution("click")),
            ],
        );
        let likely = (Severity::Error, Confidence::Likely);
        let hint = (Severity::Info, Confidence::Maybe);
        let kept = (Severity::Error, Confidence::Certain);
        assert_eq!(found, [likely, kept, hint, kept, hint, kept, kept]);

        let import = Gap {
            file: "sub/tools/gen.py".to_owned(),
            kind: GapKind::UnreadPathSourceImport,
            member: None,
            target: None,
            site: Some(("click".to_owned(), Some(1))),
        };
        let found = lowered(
            &[import],
            vec![
                candidate(RuleId::Chk002, distribution("click")),
                candidate(RuleId::Chk002, distribution("rich")),
                candidate(RuleId::Chk001, file("sub/tools/other.py")),
            ],
        );
        assert_eq!(found, [likely, kept, kept]);
    }

    /// An image's tree is the directory of its Dockerfile: the imports there
    /// become hints, the files and everything outside keep their findings (#1061).
    #[test]
    fn unread_image_tree_lowers_imports_under_the_dockerfile_directory() {
        let under = |module: &str, file: &str| IssueSubject::Import {
            module: module.to_owned(),
            file: file.to_owned(),
            line: Some(1),
            distribution: None,
        };
        let mut image = gap("dev/sim/Dockerfile", GapKind::UnreadImageTree);
        image.site = Some(("app.main".to_owned(), Some(4)));
        let mut misplaced = candidate(RuleId::Chk005, distribution("httpx"));
        misplaced.origins.push(Origin::Import {
            file: "dev/sim/app/main.py".to_owned(),
            line: Some(1),
            module: "httpx".to_owned(),
        });
        let found = lowered(
            &[image],
            vec![
                candidate(RuleId::Chk004, under("fastapi", "dev/sim/app/main.py")),
                candidate(RuleId::Chk010, under("app", "dev/sim/app/main.py")),
                misplaced,
                candidate(RuleId::Chk005, distribution("rich")),
                candidate(RuleId::Chk003, under("httpx", "dev/simulator/main.py")),
                candidate(RuleId::Chk001, file("dev/sim/app/models.py")),
                candidate(RuleId::Chk002, distribution("httpx")),
            ],
        );
        let hint = (Severity::Info, Confidence::Maybe);
        let kept = (Severity::Error, Confidence::Certain);
        assert_eq!(found, [hint, hint, hint, kept, kept, kept, kept]);
    }

    /// Plugin references follow the statements, so the first import site is
    /// the least by file and line, not the first seen.
    #[test]
    fn path_source_import_gap_names_the_first_site_under_the_directory() {
        let import = |distribution: Option<&str>, file: &str, line| ResolvedImport {
            import_root: "click".to_owned(),
            full_module: "click".to_owned(),
            file: file.to_owned(),
            workspace_member: None,
            line,
            reference: None,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            never_runs: false,
            origin: crate::graph::ModuleOrigin::ThirdParty,
            distribution: distribution.map(str::to_owned),
            confidence: crate::resolver::ResolveConfidence::Certain,
        };
        let imports = [
            import(Some("click"), "sub/tools/gen.py", Some(5)),
            import(Some("click"), "sub/a.py", None),
            import(Some("click"), "sub/a.py", Some(3)),
            import(Some("click"), "sub/a.py", Some(7)),
            import(Some("click"), "sub2/a.py", Some(1)),
            import(Some("rich"), "sub2/a.py", Some(1)),
            import(None, "sub/a.py", Some(1)),
        ]
        .map(Arc::new);
        let sites: Vec<_> = path_source_gaps(&imports, &["sub".to_owned()])
            .into_iter()
            .map(|gap| (gap.kind, gap.file, gap.site))
            .collect();
        assert_eq!(
            sites,
            [
                (GapKind::UnreadPathSource, "sub".to_owned(), None),
                (
                    GapKind::UnreadPathSourceImport,
                    "sub/a.py".to_owned(),
                    Some(("click".to_owned(), Some(3)))
                ),
            ]
        );
    }

    #[test]
    fn unknown_runtime_dependencies_turn_missing_declarations_into_hints() {
        let found = lowered(
            &[gap("setup.py", GapKind::RuntimeDependenciesUnknown)],
            vec![
                candidate(RuleId::Chk003, import("yaml")),
                candidate(RuleId::Chk004, import("idna")),
                candidate(RuleId::Chk003, script_distribution("rich")),
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk005, distribution("pytest")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe),
                (Severity::Info, Confidence::Maybe),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
                (Severity::Info, Confidence::Maybe),
            ]
        );
    }

    /// A runtime line may be the declaration a misplaced finding misses, so
    /// it hints CHK005 on the distribution it names; a dev line cannot be
    /// (#935).
    #[test]
    fn invalid_runtime_requirement_also_hints_misplaced_findings_on_its_name() {
        let invalid = |kind| Gap {
            file: "pyproject.toml".to_owned(),
            kind,
            member: None,
            target: None,
            site: Some(("rich".to_owned(), None)),
        };
        let candidates = || {
            vec![
                candidate(RuleId::Chk005, distribution("rich")),
                candidate(RuleId::Chk005, distribution("pytest")),
                candidate(RuleId::Chk002, distribution("rich")),
            ]
        };
        assert_eq!(
            lowered(&[invalid(GapKind::InvalidRuntimeRequirement)], candidates()),
            [
                (Severity::Info, Confidence::Maybe),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
        assert_eq!(
            lowered(&[invalid(GapKind::InvalidRequirement)], candidates()),
            [
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// An archive line names the distribution it installs by its file name,
    /// so it hints the imports of that name, resolved or not, and a runtime
    /// one the misplaced finding on it too; an invalid line's name reaches no
    /// unresolved import (#988).
    #[test]
    fn archive_requirement_hints_imports_of_its_name() {
        let archive = |kind| Gap {
            file: "requirements.txt".to_owned(),
            kind,
            member: None,
            target: None,
            site: Some(("foo-bar".to_owned(), Some(2))),
        };
        let resolved = |module: &str, distribution: &str| IssueSubject::Import {
            module: module.to_owned(),
            file: "acme/main.py".to_owned(),
            line: Some(1),
            distribution: Some(distribution.to_owned()),
        };
        let candidates = || {
            vec![
                candidate(RuleId::Chk003, resolved("foo_bar", "foo-bar")),
                candidate(RuleId::Chk010, import("foo_bar.core")),
                candidate(RuleId::Chk010, import("foo")),
                candidate(RuleId::Chk003, resolved("yaml", "pyyaml")),
                candidate(RuleId::Chk005, distribution("foo-bar")),
            ]
        };
        let hint = (Severity::Info, Confidence::Maybe);
        let kept = (Severity::Error, Confidence::Certain);
        assert_eq!(
            lowered(&[archive(GapKind::ArchiveRuntimeRequirement)], candidates()),
            [hint, hint, kept, kept, hint]
        );
        assert_eq!(
            lowered(&[archive(GapKind::ArchiveRequirement)], candidates()),
            [hint, hint, kept, kept, kept]
        );
        assert_eq!(
            lowered(&[archive(GapKind::InvalidRequirement)], candidates()),
            [hint, kept, kept, kept, kept]
        );
    }

    /// An unread extra may declare any import as optional, so it caps the
    /// missing and misplaced declarations of its manifest's scope (#975).
    #[test]
    fn unknown_extras_cap_missing_and_misplaced_declarations() {
        let mut member_gap = gap("packages/x/setup.py", GapKind::UnknownExtras);
        member_gap.member = Some("x".to_owned());
        let found = lowered(
            &[gap("setup.py", GapKind::UnknownExtras)],
            vec![
                candidate(RuleId::Chk003, import("yaml")),
                candidate(RuleId::Chk004, import("idna")),
                candidate(RuleId::Chk005, distribution("pytest")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk003, script_distribution("rich")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
        let found = lowered(
            &[member_gap],
            vec![
                in_member(candidate(RuleId::Chk005, distribution("pytest")), "x"),
                in_member(candidate(RuleId::Chk005, distribution("pytest")), "y"),
                candidate(RuleId::Chk003, import("yaml")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// A member's unread declarations may declare its own imports only, so
    /// they lower that member's findings and leave the root's and other
    /// members' alone (#910).
    #[test]
    fn a_member_gap_lowers_only_that_members_missing_declarations() {
        let mut member_gap = gap(
            "packages/x/pyproject.toml",
            GapKind::RuntimeDependenciesUnknown,
        );
        member_gap.member = Some("x".to_owned());
        let mut candidates = vec![
            in_member(candidate(RuleId::Chk003, import("yaml")), "x"),
            in_member(candidate(RuleId::Chk004, import("idna")), "x"),
            in_member(candidate(RuleId::Chk003, import("yaml")), "y"),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(&mut candidates, &[member_gap]);
        let found: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.severity,
                    candidate.confidence,
                    candidate.explain.details.clone(),
                )
            })
            .collect();
        let detail = vec![
            "runtime dependencies could not be read statically: `packages/x/pyproject.toml`"
                .to_owned(),
        ];
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe, detail.clone()),
                (Severity::Info, Confidence::Maybe, detail),
                (Severity::Error, Confidence::Certain, Vec::new()),
                (Severity::Error, Confidence::Certain, Vec::new()),
            ]
        );
    }

    /// An invalid line may declare the distribution it names, so it lowers
    /// the missing-declaration findings for that one: the root's line in
    /// every member, a member's in that member alone (#928).
    #[test]
    fn invalid_requirement_hints_only_imports_of_the_distribution_it_names() {
        let resolved = |module: &str, name: &str| IssueSubject::Import {
            module: module.to_owned(),
            file: "acme/main.py".to_owned(),
            line: Some(1),
            distribution: Some(name.to_owned()),
        };
        let invalid = |file: &str, name: &str, member: Option<&str>| Gap {
            file: file.to_owned(),
            kind: GapKind::InvalidRequirement,
            member: member.map(str::to_owned),
            site: Some((name.to_owned(), Some(2))),
            target: None,
        };
        let mut candidates = vec![
            candidate(RuleId::Chk003, resolved("yaml", "pyyaml")),
            in_member(candidate(RuleId::Chk004, resolved("yaml", "pyyaml")), "y"),
            candidate(RuleId::Chk003, resolved("idna", "idna")),
            in_member(candidate(RuleId::Chk003, resolved("rich", "rich")), "x"),
            in_member(candidate(RuleId::Chk003, resolved("rich", "rich")), "y"),
            candidate(RuleId::Chk003, import("pyyaml")),
            candidate(RuleId::Chk002, distribution("pyyaml")),
        ];
        apply_gaps(
            &mut candidates,
            &[
                invalid("requirements.txt", "pyyaml", None),
                invalid("packages/x/pyproject.toml", "rich", Some("x")),
            ],
        );
        let found: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.severity,
                    candidate.confidence,
                    candidate.explain.details.clone(),
                )
            })
            .collect();
        let detail = |site: &str| {
            vec![format!(
                "a requirement line naming the distribution could not be read: {site}"
            )]
        };
        let hint = |site| (Severity::Info, Confidence::Maybe, detail(site));
        let kept = (Severity::Error, Confidence::Certain, Vec::new());
        assert_eq!(
            found,
            [
                hint("`pyyaml` in `requirements.txt:2`"),
                hint("`pyyaml` in `requirements.txt:2`"),
                kept.clone(),
                hint("`rich` in `packages/x/pyproject.toml:2`"),
                kept.clone(),
                kept.clone(),
                kept,
            ]
        );
    }

    /// The root's unread declarations still lower every finding, a member's
    /// included, and `--explain` counts only the gaps in a finding's scope.
    #[test]
    fn the_root_gap_reaches_members_and_explain_counts_gaps_in_scope() {
        let mut member_x = gap("packages/x/setup.py", GapKind::RuntimeDependenciesUnknown);
        member_x.member = Some("x".to_owned());
        let mut member_y = gap("packages/y/setup.py", GapKind::RuntimeDependenciesUnknown);
        member_y.member = Some("y".to_owned());
        let mut candidates = vec![
            in_member(candidate(RuleId::Chk003, import("yaml")), "x"),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(
            &mut candidates,
            &[
                gap("pyproject.toml", GapKind::RuntimeDependenciesUnknown),
                member_x,
                member_y,
            ],
        );
        assert_eq!(
            candidates[0].explain.details,
            ["runtime dependencies could not be read statically: `pyproject.toml` (and 1 more)"]
        );
        assert_eq!(
            candidates[1].explain.details,
            ["runtime dependencies could not be read statically: `pyproject.toml`"]
        );
        assert_eq!(candidates[1].severity, Severity::Info);
    }

    fn unresolved(file: &str, kind: GapKind, module: &str, line: u32) -> Gap {
        let mut gap = gap(file, kind);
        gap.site = Some((module.to_owned(), Some(line)));
        gap
    }

    /// An unresolved import may name what an unused distribution provides;
    /// files and missing declarations are read from imports that did resolve.
    #[test]
    fn unresolved_import_caps_only_unused_project_distributions() {
        let mut candidates = vec![
            candidate(RuleId::Chk002, distribution("acme-widgets-sdk")),
            candidate(RuleId::Chk002, script_distribution("rich")),
            candidate(RuleId::Chk001, file("acme/orphan.py")),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(
            &mut candidates,
            &[
                unresolved(
                    "acme/main.py",
                    GapKind::UnresolvedImport,
                    "zz_widgets_core",
                    1,
                ),
                unresolved("acme/cli.py", GapKind::UnresolvedImport, "zz_cli", 3),
            ],
        );
        let found: Vec<_> = candidates
            .iter()
            .map(|candidate| (candidate.confidence, candidate.explain.details.len()))
            .collect();
        assert_eq!(
            found,
            [
                (Confidence::Likely, 1),
                (Confidence::Certain, 0),
                (Confidence::Certain, 0),
                (Confidence::Certain, 0),
            ]
        );
        assert_eq!(
            candidates[0].explain.details,
            [
                "a reachable import could not be resolved to a distribution: `zz_widgets_core` in `acme/main.py:1` (and 1 more)"
            ]
        );
    }

    /// A script's block installs what the script imports, so its unresolved
    /// import lowers that block's entries and no other script's or the project's.
    #[test]
    fn unresolved_script_import_caps_only_its_own_block() {
        let mut other = script_distribution("httpx");
        if let IssueSubject::ScriptDistribution { script, .. } = &mut other {
            "other.py".clone_into(script);
        }
        let found = lowered(
            &[unresolved(
                "tool.py",
                GapKind::UnresolvedScriptImport,
                "zz_render",
                4,
            )],
            vec![
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk002, other),
                candidate(RuleId::Chk002, distribution("requests")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// Only the assumed py311 decided that `binhex` is not stdlib, so that
    /// import in that file is a hint; other imports and files keep their level.
    #[test]
    fn assumed_target_version_hints_only_the_version_dependent_import() {
        let mut elsewhere = candidate(RuleId::Chk010, import("binhex"));
        if let IssueSubject::Import { file, .. } = &mut elsewhere.subject {
            "acme/cli.py".clone_into(file);
        }
        let found = lowered(
            &[unresolved(
                "acme/main.py",
                GapKind::AssumedTargetVersion,
                "binhex",
                1,
            )],
            vec![
                candidate(RuleId::Chk010, import("binhex")),
                candidate(RuleId::Chk003, import("binhex.sub")),
                candidate(RuleId::Chk010, import("yaml")),
                elsewhere,
                candidate(RuleId::Chk002, distribution("binhex")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe),
                (Severity::Info, Confidence::Maybe),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// What a script block could not read hints that script's findings alone:
    /// its import of a version-dependent root, by module or by the origin of
    /// a missing declaration, and its import of the name an invalid line names.
    #[test]
    fn script_block_gaps_hint_only_that_scripts_findings() {
        let in_script = |rule, subject| {
            let mut found = candidate(rule, subject);
            if let IssueSubject::Import { file, .. } = &mut found.subject {
                "tool.py".clone_into(file);
            }
            found
        };
        let mut via_origin = candidate(RuleId::Chk003, script_distribution("setuptools"));
        via_origin.origins.push(Origin::Import {
            file: "tool.py".to_owned(),
            line: Some(1),
            module: "distutils.core".to_owned(),
        });
        let mut other_script = script_distribution("rich");
        if let IssueSubject::ScriptDistribution { script, .. } = &mut other_script {
            "other.py".clone_into(script);
        }
        let found = lowered(
            &[
                unresolved(
                    "tool.py",
                    GapKind::AssumedScriptTargetVersion,
                    "distutils",
                    1,
                ),
                Gap {
                    file: "tool.py".to_owned(),
                    kind: GapKind::InvalidScriptRequirement,
                    member: None,
                    target: None,
                    site: Some(("rich".to_owned(), Some(2))),
                },
            ],
            vec![
                via_origin,
                in_script(RuleId::Chk010, import("distutils")),
                candidate(RuleId::Chk003, script_distribution("rich")),
                candidate(RuleId::Chk003, script_distribution("httpx")),
                candidate(RuleId::Chk003, other_script),
                candidate(RuleId::Chk010, import("distutils")),
                candidate(RuleId::Chk002, script_distribution("rich")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe),
                (Severity::Info, Confidence::Maybe),
                (Severity::Info, Confidence::Maybe),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    #[test]
    fn a_cap_never_raises_confidence() {
        let mut maybe = candidate(RuleId::Chk001, file("acme/orphan.py"));
        maybe.confidence = Confidence::Maybe;
        let found = lowered(&[gap("acme/latin.py", GapKind::UnreadModule)], vec![maybe]);
        assert_eq!(found, [(Severity::Error, Confidence::Maybe)]);
    }

    /// The cells that were read still count, so only absence findings fall;
    /// a missing declaration seen in a read cell is still certain (#902).
    #[test]
    fn unread_notebook_cells_cap_absence_findings_like_an_unread_module() {
        let found = lowered(
            &[gap("acme/demo.ipynb", GapKind::UnreadNotebookCells)],
            vec![
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk003, import("yaml")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// Module gaps of `modules` (path, skipped, opaque) when `reached` run.
    fn reached_gaps(modules: &[(&str, bool, bool)], reached: &[&str]) -> Vec<(String, GapKind)> {
        use crate::discovery::{ProjectRoot, RootMarker};
        use crate::graph::FileNode;
        use crate::parser::ParsedModule;
        use crate::sources::{FileContext, FileKind};

        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let mut reachability = ReachabilityReport::default();
        for (path, ..) in modules {
            let file_id = graph
                .intern_file(FileNode {
                    path: (*path).to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file");
            if reached.contains(path) {
                reachability.reachable.insert(file_id);
            }
        }
        let parse = ParseSummary {
            modules: modules
                .iter()
                .map(|&(path, skipped, opaque)| ParsedModule {
                    path: path.to_owned(),
                    skipped,
                    has_opaque_dynamic_import: opaque,
                    ..ParsedModule::default()
                })
                .collect(),
        };
        module_gaps(&parse, &graph, &reachability)
            .into_iter()
            .map(|gap| (gap.file, gap.kind))
            .collect()
    }

    #[test]
    fn only_reached_modules_leave_gaps() {
        let found = reached_gaps(
            &[
                ("acme/main.py", false, true),
                ("acme/latin.py", true, false),
                ("acme/orphan.py", false, true),
                ("acme/stray.py", true, false),
            ],
            &["acme/main.py", "acme/latin.py"],
        );
        assert_eq!(
            found,
            [
                ("acme/main.py".to_owned(), GapKind::OpaqueDynamicImport),
                ("acme/latin.py".to_owned(), GapKind::UnreadModule),
            ]
        );
    }

    /// A library caller may import a public module no root reaches, so an
    /// unread one may use any declared distribution; what it imports ships in
    /// the wheel and is public too, so orphans stay as they are (#550).
    #[test]
    fn an_unread_public_module_leaves_a_gap_that_caps_only_unused_distributions() {
        use crate::config::ProjectMode;
        use crate::graph::FileId;
        use crate::reachability::UnreachableFile;

        let unread = |path: &str, mode, max_confidence| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence,
            mode,
            confidence_reason: None,
        };
        let mut reachability = ReachabilityReport::default();
        reachability.unread = vec![
            unread("acme/client.py", ProjectMode::Library, Confidence::Maybe),
            unread("tools/release.py", ProjectMode::App, Confidence::Certain),
        ];
        let gaps = unread_public_module_gaps(&reachability);
        assert_eq!(gaps, [gap("acme/client.py", GapKind::UnreadPublicModule)]);

        let found = lowered(
            &gaps,
            vec![
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk003, import("yaml")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    #[test]
    fn explain_names_each_gap_kind_once() {
        let mut candidates = vec![candidate(RuleId::Chk001, file("acme/orphan.py"))];
        apply_gaps(
            &mut candidates,
            &[
                gap("acme/a.py", GapKind::UnreadModule),
                gap("acme/b.py", GapKind::UnreadModule),
                gap("acme/main.py", GapKind::OpaqueDynamicImport),
            ],
        );
        assert_eq!(
            candidates[0].explain.details,
            [
                "a reachable module could not be read: `acme/a.py` (and 1 more)",
                "a reachable module imports a module by a name computed at runtime: `acme/main.py`",
            ]
        );
    }

    #[test]
    fn keep_first_line_keeps_the_earliest_line_and_prefers_one_to_none() {
        let mut sites = BTreeMap::new();
        for (key, line) in [
            ("later", Some(5)),
            ("later", Some(3)),
            ("earlier", Some(3)),
            ("earlier", Some(5)),
            ("none_first", None),
            ("none_first", Some(4)),
            ("none_last", Some(4)),
            ("none_last", None),
            ("only_none", None),
        ] {
            keep_first_line(&mut sites, key, line);
        }
        assert_eq!(
            sites.into_iter().collect::<Vec<_>>(),
            [
                ("earlier", Some(3)),
                ("later", Some(3)),
                ("none_first", Some(4)),
                ("none_last", Some(4)),
                ("only_none", None),
            ]
        );
    }

    /// The unread extras' gap names the warning's file and setup.py argument,
    /// else the manifest the extras came from.
    #[test]
    fn extras_gap_names_where_the_extras_went_unread() {
        use crate::discovery::{ProjectRoot, RootMarker};
        use crate::manifest::{LockfileGraph, ManifestSources, ProjectMetadata, UvToolSettings};

        let manifest =
            |warnings: Vec<ManifestWarning>, setup_cfg: bool, setup_py: bool| LoadedManifest {
                root: ProjectRoot {
                    path: std::env::temp_dir(),
                    marker: RootMarker::PyProjectToml,
                },
                metadata: ProjectMetadata::default(),
                dependencies: Vec::new(),
                constraints: Vec::new(),
                uv: UvToolSettings::default(),
                uv_workspace: None,
                entry_points: Vec::new(),
                lockfile: LockfileGraph::default(),
                sources: ManifestSources {
                    setup_cfg,
                    setup_py,
                    extras: ExtrasState::Unknown,
                    ..ManifestSources::default()
                },
                warnings,
            };
        let found = |manifest: LoadedManifest| {
            extras_gap(&manifest).map(|gap| {
                assert_eq!(gap.kind, GapKind::UnknownExtras);
                (gap.file, gap.site.map(|(argument, _)| argument))
            })
        };
        let partly = |argument: &str| ManifestWarning::SetupPyPartiallyStatic {
            file: "setup.py".to_owned(),
            argument: argument.to_owned(),
        };
        assert_eq!(
            found(manifest(
                vec![
                    partly("install_requires"),
                    partly("extras_require[\"dev\"]")
                ],
                false,
                true
            )),
            Some((
                "setup.py".to_owned(),
                Some("extras_require[\"dev\"]".to_owned())
            ))
        );
        assert_eq!(
            found(manifest(
                vec![ManifestWarning::DynamicExtrasUnread {
                    file: "pyproject.toml".to_owned(),
                    extras: vec!["dev".to_owned()],
                }],
                false,
                false
            )),
            Some(("pyproject.toml".to_owned(), None))
        );
        assert_eq!(
            found(manifest(vec![partly("install_requires")], true, false)),
            Some(("setup.cfg".to_owned(), None))
        );
        assert_eq!(
            found(manifest(Vec::new(), true, true)),
            Some(("setup.py".to_owned(), None))
        );
        let mut known = manifest(Vec::new(), true, true);
        known.sources.extras = ExtrasState::Known;
        assert_eq!(found(known), None);
    }

    mod props {
        use proptest::prelude::*;

        use super::*;
        use crate::config::{RuntimeOverrides, default_config};
        use crate::parser::ParseSummary;
        use crate::reachability::ReachabilityReport;
        use crate::resolver::ResolutionIndex;
        use crate::rules::emit::emit_issues;
        use crate::rules::types::{DependencyReport, ExplainData};

        const FILES: [&str; 6] = [
            "acme/main.py",
            "acme/sub/mod.py",
            "tool.py",
            "img/Dockerfile",
            "img/app.py",
            "other/x.py",
        ];
        /// Gap files: the files, and the directories a path source names.
        const GAP_FILES: [&str; 9] = [
            "acme/main.py",
            "acme/sub/mod.py",
            "tool.py",
            "img/Dockerfile",
            "img/app.py",
            "other/x.py",
            "acme",
            "img",
            "other",
        ];
        const MODULES: [&str; 4] = ["requests", "rich.console", "yaml", "acme.sub"];
        const NAMES: [&str; 4] = ["requests", "rich", "yaml", "acme"];
        const MEMBERS: [Option<&str>; 3] = [None, Some("a"), Some("b")];

        const KINDS: [GapKind; 28] = [
            GapKind::UnreadModule,
            GapKind::SkippedEntry,
            GapKind::NoEntryRoot,
            GapKind::UnreadPublicModule,
            GapKind::UnreadNotebookCells,
            GapKind::UnresolvedRelativeImport,
            GapKind::OpaqueDynamicImport,
            GapKind::RuntimeDependenciesUnknown,
            GapKind::UnknownExtras,
            GapKind::InvalidRequirement,
            GapKind::InvalidRuntimeRequirement,
            GapKind::ArchiveRequirement,
            GapKind::ArchiveRuntimeRequirement,
            GapKind::UnresolvedImport,
            GapKind::UnresolvedPublicImport,
            GapKind::UnresolvedScriptImport,
            GapKind::UnreadCommandTarget,
            GapKind::UnreadPathSource,
            GapKind::UnreadPathSourceImport,
            GapKind::UnreadImageTree,
            GapKind::UnreadDjangoSettings,
            GapKind::UnreadBackendSetting,
            GapKind::UnreadSphinxExtensions,
            GapKind::AssumedTargetVersion,
            GapKind::InvalidScriptRequirement,
            GapKind::AssumedScriptTargetVersion,
            GapKind::StringModuleRef,
            GapKind::StringDistributionRef,
        ];

        /// The rules each kind's documentation says it may lower, written
        /// from the `GapKind` docs rather than from `lowering`. The match is
        /// exhaustive, so a new kind has to state its rules here.
        const fn declared_rules(kind: GapKind) -> &'static [RuleId] {
            match kind {
                GapKind::UnreadModule
                | GapKind::SkippedEntry
                | GapKind::NoEntryRoot
                | GapKind::UnreadNotebookCells
                | GapKind::UnreadDjangoSettings
                | GapKind::UnreadSphinxExtensions => &[RuleId::Chk001, RuleId::Chk002],
                GapKind::UnresolvedRelativeImport
                | GapKind::OpaqueDynamicImport
                | GapKind::UnreadCommandTarget
                | GapKind::StringModuleRef => &[RuleId::Chk001],
                GapKind::UnreadPublicModule
                | GapKind::UnresolvedImport
                | GapKind::UnresolvedPublicImport
                | GapKind::UnresolvedScriptImport
                | GapKind::UnreadPathSourceImport
                | GapKind::UnreadBackendSetting
                | GapKind::StringDistributionRef => &[RuleId::Chk002],
                GapKind::UnknownExtras
                | GapKind::RuntimeDependenciesUnknown
                | GapKind::InvalidRuntimeRequirement => {
                    &[RuleId::Chk003, RuleId::Chk004, RuleId::Chk005]
                },
                GapKind::InvalidRequirement => &[RuleId::Chk003, RuleId::Chk004],
                GapKind::InvalidScriptRequirement => &[RuleId::Chk003],
                GapKind::ArchiveRequirement
                | GapKind::AssumedTargetVersion
                | GapKind::AssumedScriptTargetVersion => {
                    &[RuleId::Chk003, RuleId::Chk004, RuleId::Chk010]
                },
                GapKind::ArchiveRuntimeRequirement | GapKind::UnreadImageTree => &[
                    RuleId::Chk003,
                    RuleId::Chk004,
                    RuleId::Chk005,
                    RuleId::Chk010,
                ],
                GapKind::UnreadPathSource => &[
                    RuleId::Chk001,
                    RuleId::Chk003,
                    RuleId::Chk004,
                    RuleId::Chk005,
                ],
            }
        }

        fn owned(names: &'static [&'static str]) -> impl Strategy<Value = String> {
            prop::sample::select(names).prop_map(str::to_owned)
        }

        fn line() -> impl Strategy<Value = Option<u32>> {
            prop::option::of(1u32..4)
        }

        fn subject() -> impl Strategy<Value = IssueSubject> {
            prop_oneof![
                owned(&FILES).prop_map(|path| IssueSubject::File { path }),
                owned(&NAMES).prop_map(|name| IssueSubject::Distribution { name }),
                (owned(&MODULES), owned(&NAMES))
                    .prop_map(|(module, name)| IssueSubject::Symbol { module, name }),
                owned(&NAMES).prop_map(|name| IssueSubject::Binary { name }),
                (
                    owned(&MODULES),
                    owned(&FILES),
                    line(),
                    prop::option::of(owned(&NAMES))
                )
                    .prop_map(|(module, file, line, distribution)| {
                        IssueSubject::Import {
                            module,
                            file,
                            line,
                            distribution,
                        }
                    }),
                (owned(&FILES), owned(&NAMES))
                    .prop_map(|(script, name)| IssueSubject::ScriptDistribution { script, name }),
            ]
        }

        fn member() -> impl Strategy<Value = Option<String>> {
            prop::sample::select(MEMBERS.as_slice()).prop_map(|member| member.map(str::to_owned))
        }

        fn candidate() -> impl Strategy<Value = IssueCandidate> {
            (
                prop::sample::select(RuleId::ALL.as_slice()),
                subject(),
                prop::sample::select(
                    [Severity::Error, Severity::Warning, Severity::Info].as_slice(),
                ),
                prop::sample::select(
                    [Confidence::Maybe, Confidence::Likely, Confidence::Certain].as_slice(),
                ),
                member(),
                prop::collection::vec(
                    (owned(&FILES), line(), owned(&MODULES))
                        .prop_map(|(file, line, module)| Origin::Import { file, line, module }),
                    0..3,
                ),
            )
                .prop_map(
                    |(rule, subject, severity, confidence, workspace_member, origins)| {
                        IssueCandidate {
                            rule,
                            subject,
                            severity,
                            confidence,
                            message: "finding".to_owned(),
                            workspace_member,
                            origins,
                            explain: ExplainData::default(),
                        }
                    },
                )
        }

        fn gap() -> impl Strategy<Value = Gap> {
            (
                prop::sample::select(KINDS.as_slice()),
                owned(&GAP_FILES),
                member(),
                prop::option::of((owned(&NAMES), line())),
                prop::option::of(owned(&FILES)),
            )
                .prop_map(|(kind, file, member, site, target)| Gap {
                    file,
                    kind,
                    member,
                    site,
                    target,
                })
        }

        fn gaps() -> impl Strategy<Value = Vec<Gap>> {
            prop::collection::vec(gap(), 0..5)
        }

        fn applied(candidates: &[IssueCandidate], gaps: &[Gap]) -> Vec<IssueCandidate> {
            let mut candidates = candidates.to_vec();
            apply_gaps(&mut candidates, gaps);
            candidates
        }

        /// Everything a gap must leave alone: the finding itself and where
        /// it was seen.
        fn identity(candidate: &IssueCandidate) -> impl PartialEq + std::fmt::Debug + '_ {
            (
                candidate.rule,
                &candidate.subject,
                &candidate.message,
                &candidate.workspace_member,
                &candidate.origins,
            )
        }

        /// Every gap the vocabulary can write in `member`, of `kinds`.
        fn every_gap(kinds: &[GapKind], member: Option<&String>) -> Vec<Gap> {
            let sites = NAMES.iter().map(|&name| Some((name.to_owned(), Some(1))));
            let sites: Vec<_> = sites.chain([None]).collect();
            let targets = FILES.iter().map(|&path| Some(path.to_owned()));
            let targets: Vec<_> = targets.chain([None]).collect();
            let mut placed = Vec::new();
            for file in GAP_FILES {
                for site in &sites {
                    for target in &targets {
                        placed.push((file, site, target));
                    }
                }
            }
            kinds
                .iter()
                .flat_map(|&kind| {
                    placed.iter().map(move |(file, site, target)| Gap {
                        file: (*file).to_owned(),
                        kind,
                        member: member.cloned(),
                        site: (*site).clone(),
                        target: (*target).clone(),
                    })
                })
                .collect()
        }

        fn emitted(
            candidates: &[IssueCandidate],
            gaps: &[Gap],
            strict: bool,
        ) -> (usize, crate::ExitStatus) {
            let deps = DependencyReport {
                candidates: candidates.to_vec(),
                ..DependencyReport::default()
            };
            let report = emit_issues(
                &ReachabilityReport::default(),
                &deps,
                &[],
                &ParseSummary::default(),
                gaps,
                &default_config(),
                &RuntimeOverrides {
                    strict: Some(strict),
                    ..RuntimeOverrides::default()
                },
                &ResolutionIndex::default(),
            );
            (report.issues.len(), report.exit_status)
        }

        proptest! {
            /// No gap, no change (ADR 0005).
            #[test]
            fn without_gaps_findings_are_unchanged(
                candidates in prop::collection::vec(candidate(), 0..6),
            ) {
                prop_assert_eq!(applied(&candidates, &[]), candidates);
            }

            /// A gap only weakens: confidence and severity never rise, and
            /// the finding, its count and its origins stay.
            #[test]
            fn gaps_never_strengthen_a_finding(
                candidates in prop::collection::vec(candidate(), 0..6),
                gaps in gaps(),
            ) {
                let lowered = applied(&candidates, &gaps);
                prop_assert_eq!(lowered.len(), candidates.len());
                for (after, before) in lowered.iter().zip(&candidates) {
                    prop_assert_eq!(identity(after), identity(before));
                    prop_assert!(after.confidence <= before.confidence, "{after:?}");
                    prop_assert!(after.severity.rank() <= before.severity.rank(), "{after:?}");
                }
            }

            /// Adding gaps lowers every finding at least as far, so the run
            /// reports no more issues and its exit status never worsens.
            #[test]
            fn more_gaps_lower_at_least_as_far(
                candidates in prop::collection::vec(candidate(), 0..6),
                gaps in gaps(),
                extra in gaps(),
                strict in any::<bool>(),
            ) {
                let more: Vec<Gap> = gaps.iter().chain(&extra).cloned().collect();
                let (fewer, wider) = (applied(&candidates, &gaps), applied(&candidates, &more));
                for (wider, fewer) in wider.iter().zip(&fewer) {
                    prop_assert!(wider.confidence <= fewer.confidence, "{wider:?}");
                    prop_assert!(wider.severity.rank() <= fewer.severity.rank(), "{wider:?}");
                }
                let (fewer_issues, fewer_status) = emitted(&candidates, &gaps, strict);
                let (wider_issues, wider_status) = emitted(&candidates, &more, strict);
                prop_assert!(wider_issues <= fewer_issues);
                if fewer_status == crate::ExitStatus::Success {
                    prop_assert_eq!(wider_status, crate::ExitStatus::Success);
                }
            }

            /// A gap never touches a rule its kind does not declare, nor a
            /// finding outside its workspace member.
            #[test]
            fn gaps_leave_findings_outside_their_scope_alone(
                candidate in candidate(),
                gaps in gaps(),
                extra in gaps(),
            ) {
                let outside: Vec<Gap> = extra
                    .into_iter()
                    .filter(|gap| {
                        !declared_rules(gap.kind).contains(&candidate.rule)
                            || gap.member.as_ref().is_some_and(|member| {
                                candidate.workspace_member.as_ref() != Some(member)
                            })
                    })
                    .collect();
                let with_outside: Vec<Gap> = gaps.iter().chain(&outside).cloned().collect();
                let candidates = [candidate];
                prop_assert_eq!(applied(&candidates, &with_outside), applied(&candidates, &gaps));
                prop_assert_eq!(applied(&candidates, &outside), candidates.to_vec());
            }
        }

        proptest! {
            // Each case applies every gap the vocabulary can write.
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(64)))]

            /// The same for every gap aimed at the finding: whatever file,
            /// site or target it names, a kind that does not declare the
            /// finding's rule, or a gap in another member, changes nothing.
            #[test]
            fn no_gap_of_an_undeclared_kind_reaches_a_finding(candidate in candidate()) {
                let undeclared: Vec<GapKind> = KINDS
                    .into_iter()
                    .filter(|kind| !declared_rules(*kind).contains(&candidate.rule))
                    .collect();
                let mut gaps = every_gap(&undeclared, candidate.workspace_member.as_ref());
                for member in MEMBERS.iter().flatten() {
                    if candidate.workspace_member.as_deref() != Some(member) {
                        gaps.extend(every_gap(&KINDS, Some(&(*member).to_owned())));
                    }
                }
                let candidates = [candidate];
                prop_assert_eq!(applied(&candidates, &gaps), candidates.to_vec());
            }
        }
    }
}
