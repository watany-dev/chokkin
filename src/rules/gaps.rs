//! Input the analysis could not read, and the one stage that lowers the
//! findings it may hide (ADR 0005).
//!
//! A gap is recorded where it is found; each kind declares which findings it
//! weakens, so no rule has to know about unread input itself.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{Confidence, DependencyGroupsConfig, TargetVersion};
use crate::entry::resolve_module_to_path;
use crate::graph::ProjectGraph;
use crate::manifest::{
    ExtrasState, InlineScript, LoadedManifest, ManifestWarning, invalid_requirement_name,
};
use crate::parser::{ImportContext, ParseSummary};
use crate::path_util::join_rel;
use crate::plugins::{CommandTarget, CommandTargetRef, PluginHints, UnreadSettings};
use crate::reachability::ReachabilityReport;
use crate::resolver::{ResolveWarning, StdlibRange, import_root, is_first_party_import};
use crate::rules::RuleContext;
use crate::rules::deps::{DeclarationBucket, declaration_bucket, file_context};
use crate::rules::types::{IssueCandidate, IssueSubject, Origin, RuleId, Severity, import_site};
use crate::sources::FileContext;

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
    /// A skipped public module of a library that no root reaches: a caller
    /// may import it, so it may use any declared distribution (#550). It
    /// leaves orphans alone, since what it imports must ship in the wheel
    /// and so is public too.
    UnreadPublicModule,
    /// A reachable notebook was read, but some cells that may be code were
    /// not (#902): what those cells import is unknown. Every notebook is an
    /// entry (#514), so none is a public orphan (#942).
    UnreadNotebookCells,
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
    /// A Django project's settings could not be found or read, or star-import
    /// a module that is not a project file (#914): the apps, middleware and
    /// URL configuration they name are unknown.
    UnreadDjangoSettings,
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
}

/// How a gap changes a finding in its scope.
enum Lowering {
    /// Confidence falls to at most this level.
    Cap(Confidence),
    /// The finding becomes an info hint at `Maybe`.
    Hint,
}

impl GapKind {
    /// The scope each kind declares: what it lowers and how, if at all.
    ///
    /// Unread code may import any file or distribution, so it lowers the
    /// absence findings; an unknown declaration list may declare any import,
    /// so it lowers the missing-declaration findings to hints; an unread extra
    /// may declare one only as optional, beside a runtime list that was read,
    /// so it caps them and the misplaced ones (#975); an unresolved import may
    /// name what an unused declaration provides. A script's own block is read
    /// separately, so no project manifest gap reaches its findings; what the
    /// block itself could not read lowers that script's alone (#936).
    const fn lowering(self, candidate: &IssueCandidate) -> Option<Lowering> {
        match (self, candidate.rule, &candidate.subject) {
            (
                Self::UnreadModule
                | Self::SkippedEntry
                | Self::UnreadNotebookCells
                | Self::OpaqueDynamicImport
                | Self::UnreadCommandTarget
                | Self::UnreadDjangoSettings,
                RuleId::Chk001,
                _,
            )
            | (
                Self::UnreadModule
                | Self::SkippedEntry
                | Self::UnreadPublicModule
                | Self::UnreadNotebookCells
                | Self::UnreadDjangoSettings,
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
            | (Self::UnknownExtras, RuleId::Chk005, IssueSubject::Distribution { .. }) => {
                Some(Lowering::Cap(Confidence::Likely))
            },
            (
                Self::RuntimeDependenciesUnknown
                | Self::InvalidRequirement
                | Self::InvalidRuntimeRequirement,
                RuleId::Chk003 | RuleId::Chk004,
                IssueSubject::Import { .. },
            )
            | (
                Self::RuntimeDependenciesUnknown | Self::InvalidRuntimeRequirement,
                RuleId::Chk005,
                IssueSubject::Distribution { .. },
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
            ) => Some(Lowering::Hint),
            _ => None,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::UnreadModule => "a reachable module could not be read",
            Self::SkippedEntry => "an entry point is not a discovered source file",
            Self::UnreadPublicModule => "a public module of the library could not be read",
            Self::UnreadNotebookCells => {
                "some code cells of a reachable notebook could not be read"
            },
            Self::OpaqueDynamicImport => {
                "a reachable module imports a module by a name computed at runtime"
            },
            Self::RuntimeDependenciesUnknown => "runtime dependencies could not be read statically",
            Self::UnknownExtras => "extras could not be read statically",
            Self::InvalidRequirement | Self::InvalidRuntimeRequirement => {
                "a requirement line naming the distribution could not be read"
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
            Self::UnreadDjangoSettings => "the Django settings could not be found or read",
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
        }
    }
}

impl Gap {
    /// Whether the gap is in `candidate`'s scope: a member's gap reaches that
    /// member's findings, a script's import or block that script's alone, an
    /// assumed stdlib status the imports of that root in that file (and the
    /// script declarations they miss), and an invalid requirement the
    /// findings on the distribution it names.
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
                *script == self.file && self.site.as_ref().is_some_and(|(site, _)| site == name)
            },
            (
                GapKind::InvalidRequirement | GapKind::InvalidRuntimeRequirement,
                IssueSubject::Import { distribution, .. },
            ) => self
                .site
                .as_ref()
                .is_some_and(|(name, _)| distribution.as_deref() == Some(name.as_str())),
            (GapKind::InvalidRuntimeRequirement, IssueSubject::Distribution { name }) => {
                self.site.as_ref().is_some_and(|(site, _)| site == name)
            },
            _ => true,
        };
        in_member && in_site
    }

    /// Whether the site is the root of `module`.
    fn names_root(&self, module: &str) -> bool {
        self.site
            .as_ref()
            .is_some_and(|(root, _)| root == import_root(module))
    }

    /// What `--explain` names: the import or target and its line, or else the file.
    fn label(&self) -> String {
        match &self.site {
            Some((what, line)) => format!("`{what}` in `{}`", import_site(&self.file, *line)),
            None => format!("`{}`", self.file),
        }
    }
}

/// Gaps that can affect a finding.
///
/// One in a module that never runs hides nothing, so module and import gaps
/// count only where the module is reached or, for an unread module, where a
/// caller of the library may import it. `skipped_entries` are the entry
/// paths skipped as not discovered, less those that do not exist, which run
/// nothing.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn collect_gaps(
    context: &RuleContext<'_>,
    manifest: &LoadedManifest,
    members: &[MemberManifest<'_>],
    scripts: &[InlineScript],
    plugins: &PluginHints,
    skipped_entries: &[String],
    groups: &DependencyGroupsConfig,
    project_range: StdlibRange,
) -> Vec<Gap> {
    let mut gaps = module_gaps(context.parse, context.graph, context.reachability);
    gaps.extend(skipped_entries.iter().map(|path| Gap {
        file: path.clone(),
        kind: GapKind::SkippedEntry,
        member: None,
        site: None,
    }));
    gaps.extend(unread_public_module_gaps(context.reachability));
    gaps.extend(unresolved_import_gaps(context, manifest, scripts));
    gaps.extend(command_target_gaps(
        context,
        &plugins.config_command_targets,
    ));
    gaps.extend(plugins.unread_settings().map(settings_gap));
    gaps.extend(manifest_gap(manifest));
    gaps.extend(extras_gap(manifest));
    gaps.extend(invalid_requirement_gaps(manifest, groups));
    gaps.extend(assumed_target_gaps(context.parse, manifest, scripts));
    gaps.extend(script_gaps(context.parse, project_range, scripts));
    gaps.extend(members.iter().flat_map(|member| {
        let manifest = member.manifest;
        manifest_gap(manifest)
            .into_iter()
            .chain(extras_gap(manifest))
            .chain(invalid_requirement_gaps(manifest, groups))
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
    }
    gaps
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
/// imports are not followed, since commands add no entries yet (#912). A
/// target that names nothing discovered (a typo, a deleted file, a
/// third-party module such as `pytest`) hides no project file, so it is no gap.
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
            CommandTarget::Module { module } => {
                let layout = &context.sources.layout;
                let path =
                    resolve_module_to_path(&format!("{module}.__main__"), layout, &known_paths)
                        .or_else(|| resolve_module_to_path(module, layout, &known_paths));
                match path {
                    Some(path) if unreached(&path) => module.clone(),
                    _ => continue,
                }
            },
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
            site: Some((text, line)),
        })
        .collect()
}

/// Settings decide what Django runs in every member, so the gap lowers the
/// whole project's findings.
fn settings_gap(unread: &UnreadSettings) -> Gap {
    Gap {
        file: unread.file.clone(),
        kind: GapKind::UnreadDjangoSettings,
        member: None,
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
    let mut first: BTreeMap<(&str, &str), u32> = BTreeMap::new();
    for module in &parse.modules {
        let own_target = scripts
            .iter()
            .any(|script| script.path == module.path && script.target_version.is_some());
        if own_target {
            continue;
        }
        for import in &module.imports {
            let root = import_root(&import.module);
            if import.relative_level == 0 && !root.is_empty() && assumed.ships_only_outside(root) {
                let line = first.entry((&module.path, root)).or_insert(import.line);
                *line = (*line).min(import.line);
            }
        }
    }
    first
        .into_iter()
        .map(|((file, root), line)| Gap {
            file: file.to_owned(),
            kind: GapKind::AssumedTargetVersion,
            member: None,
            site: Some((root.to_owned(), Some(line))),
        })
        .collect()
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
        let mut first: BTreeMap<&str, u32> = BTreeMap::new();
        for import in &module.imports {
            let root = import_root(&import.module);
            if import.relative_level == 0
                && !root.is_empty()
                && project_range.ships_only_outside(root)
            {
                let line = first.entry(root).or_insert(import.line);
                *line = (*line).min(import.line);
            }
        }
        gaps.extend(first.into_iter().map(|(root, line)| Gap {
            file: script.path.clone(),
            kind: GapKind::AssumedScriptTargetVersion,
            member: None,
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
                site: Some((invalid_requirement_name(raw)?, *line)),
            }),
            _ => None,
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
                Lowering::Hint => {
                    candidate.severity = Severity::Info;
                    candidate.confidence = Confidence::Maybe;
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

    #[test]
    fn unread_module_skipped_entry_and_django_settings_cap_absence_findings_but_not_script_ones() {
        for gap in [
            gap("acme/latin.py", GapKind::UnreadModule),
            gap("manage.py", GapKind::SkippedEntry),
            gap("manage.py", GapKind::UnreadDjangoSettings),
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

        let unread = |path: &str, max_confidence| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence,
            mode: ProjectMode::Library,
        };
        let mut reachability = ReachabilityReport::default();
        reachability.unread = vec![
            unread("acme/client.py", Confidence::Maybe),
            unread("tools/release.py", Confidence::Certain),
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
}
