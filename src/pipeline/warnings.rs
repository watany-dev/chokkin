//! Pipeline warning aggregation and display.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::mem::discriminant;
use std::path::Path;

use crate::entry::EntryWarning;
use crate::manifest::{DependencyContext, ManifestWarning, invalid_requirement_name};
use crate::plugins::PluginsWarning;
use crate::reporters::{Diagnostic, condense_diagnostics, escape_control};
use crate::resolver::ResolveWarning;
use crate::sources::SourcesWarning;

/// Non-fatal warning collected during probe or analysis pipeline steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeWarning {
    /// Warning from manifest extraction.
    Manifest(ManifestWarning),
    /// Warning from a workspace member's manifest, with its files rebased
    /// onto the root.
    MemberManifest {
        /// Root-relative member directory.
        member: String,
        /// The member's warning.
        warning: ManifestWarning,
    },
    /// Warning from source file discovery.
    Sources(SourcesWarning),
    /// Warning from plugin hint extraction.
    Plugin(PluginsWarning),
    /// Warning from entry root construction.
    Entry(EntryWarning),
    /// Warning from import resolution. Never
    /// [`ResolveWarning::UnresolvedImport`]: CHK010 reports those.
    Resolve(ResolveWarning),
    /// Workspace members were inferred from nested `pyproject.toml` files.
    AutoWorkspace { member_count: usize },
    /// Nested projects outside the workspace were left out of the analysis.
    NestedProjectsSkipped { count: usize },
    /// A source that could not be read and was left out of the analysis.
    SkippedSource {
        /// Root-relative path of the source.
        path: String,
        /// Why the source could not be read.
        reason: SkipReason,
    },
    /// Something the parser could not read in a source it kept.
    ParseDiagnostic {
        /// Root-relative path of the source.
        path: String,
        /// 1-based line, or 0 when the diagnostic covers the whole file.
        line: u32,
        /// The parser's message.
        message: String,
    },
    /// Reachable modules load a module by a name computed at runtime, which
    /// caps unused-file findings below certain (#909). One per run.
    OpaqueDynamicImport {
        /// Root-relative path of the first such module.
        path: String,
        /// How many other reachable modules do the same.
        more: usize,
    },
    /// Requirements-file lines install archives chokkin does not read, each
    /// named for a distribution whose import findings become hints (#988).
    /// One per run.
    ArchiveRequirement {
        /// Root-relative path of the first such requirements file.
        path: String,
        /// Line of the first such requirement, when known.
        line: Option<u32>,
        /// The distribution its file name names.
        name: String,
        /// How many other such lines there are.
        more: usize,
    },
    /// A task file or tool config runs Python code no entry reaches, which
    /// caps unused-file findings below certain (#913). One per file.
    UnreadCommandTarget {
        /// Root-relative path of the command file.
        path: String,
        /// Line of the first such command, when known.
        line: Option<u32>,
        /// The first target as written.
        target: String,
        /// How many other targets in the file do the same.
        more: usize,
    },
    /// A string literal names a module no entry reaches, which may be loaded
    /// by that name, so that file's unused-file finding, where one is shown,
    /// is an info note (#1026). One per run, at the first such literal.
    StringModuleRef {
        /// Root-relative path of the file with the literal.
        path: String,
        /// Line of the literal.
        line: u32,
        /// The literal as written.
        text: String,
        /// Root-relative path of the file it names.
        target: String,
        /// How many other such literals there are in the project.
        more: usize,
    },
}

/// Why [`ProbeWarning::SkippedSource`] left a source out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Not UTF-8 and no supported PEP 263 coding declaration.
    Undecodable,
    /// The parser stopped at a syntax error.
    SyntaxError {
        /// 1-based line of the error.
        line: u32,
        /// The parser's message.
        message: String,
    },
    /// The notebook JSON has no code cells to read.
    InvalidNotebook {
        /// What the notebook reader could not read.
        message: String,
    },
}

impl ProbeWarning {
    pub(crate) fn diagnostic(&self) -> Diagnostic {
        let (file, line) = self.location();
        Diagnostic {
            kind: self.kind(),
            file: file.map(str::to_owned),
            line,
            text: self.to_string(),
        }
    }

    /// The variant, down to the wrapped stage warning: warnings that differ
    /// only in where they point share it. A parse diagnostic's kind is its
    /// message too, which the parser picks from a fixed set.
    fn kind(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        discriminant(self).hash(&mut hasher);
        match self {
            Self::Manifest(warning) | Self::MemberManifest { warning, .. } => {
                discriminant(warning).hash(&mut hasher);
            },
            Self::Sources(warning) => discriminant(warning).hash(&mut hasher),
            Self::Plugin(warning) => discriminant(warning).hash(&mut hasher),
            Self::Entry(warning) => discriminant(warning).hash(&mut hasher),
            Self::Resolve(warning) => discriminant(warning).hash(&mut hasher),
            Self::SkippedSource { reason, .. } => discriminant(reason).hash(&mut hasher),
            Self::ParseDiagnostic { message, .. } => message.hash(&mut hasher),
            Self::AutoWorkspace { .. }
            | Self::NestedProjectsSkipped { .. }
            | Self::OpaqueDynamicImport { .. }
            | Self::ArchiveRequirement { .. }
            | Self::UnreadCommandTarget { .. }
            | Self::StringModuleRef { .. } => {},
        }
        hasher.finish()
    }

    /// The root-relative file the warning points into, and its line when the
    /// warning names one. A path that is missing, a directory, absolute, or
    /// one of several candidates points nowhere a reader can open.
    fn location(&self) -> (Option<&str>, Option<u32>) {
        match self {
            Self::Manifest(warning) | Self::MemberManifest { warning, .. } => {
                manifest_location(warning)
            },
            Self::Sources(warning) => match warning {
                SourcesWarning::GitignoreUnreadable { path }
                | SourcesWarning::SymlinkSkipped { path, .. } => (Some(path), None),
                SourcesWarning::MissingEntryPath { .. }
                | SourcesWarning::EntryPathIsDirectory { .. }
                | SourcesWarning::AmbiguousFlatLayout { .. }
                | SourcesWarning::GuessedPackageDir { .. }
                | SourcesWarning::PathUnreadable { .. }
                | SourcesWarning::LargeProject { .. } => (None, None),
            },
            Self::Plugin(warning) => match warning {
                PluginsWarning::PartialSettingsParse { path, .. }
                | PluginsWarning::PytestConfigUnreadable { path } => (Some(path), None),
                PluginsWarning::DjangoSettingsUnread { path, line, .. } => (Some(path), *line),
                PluginsWarning::SphinxExtensionsUnread { path, line, .. } => {
                    (Some(path), Some(*line))
                },
                PluginsWarning::AmbiguousSettings { .. }
                | PluginsWarning::PluginExtractFailed { .. } => (None, None),
            },
            Self::Entry(warning) => match warning {
                EntryWarning::UnresolvedModuleTarget { file, .. } => (Some(file), None),
                EntryWarning::MissingEntryPath { .. } => (None, None),
            },
            Self::Resolve(warning) => match warning {
                ResolveWarning::UnresolvedImport { file, line, .. } => (Some(file), *line),
                ResolveWarning::VenvDistributionIgnored { path, .. } => (Some(path), None),
                ResolveWarning::AmbiguousImport { .. } | ResolveWarning::VenvUnreadable { .. } => {
                    (None, None)
                },
            },
            Self::AutoWorkspace { .. } | Self::NestedProjectsSkipped { .. } => (None, None),
            Self::SkippedSource { path, reason } => {
                let line = match reason {
                    SkipReason::SyntaxError { line, .. } => Some(*line),
                    SkipReason::Undecodable | SkipReason::InvalidNotebook { .. } => None,
                };
                (Some(path), source_line(path, line))
            },
            Self::OpaqueDynamicImport { path, .. } => (Some(path), None),
            Self::ArchiveRequirement { path, line, .. }
            | Self::UnreadCommandTarget { path, line, .. } => (Some(path), *line),
            Self::StringModuleRef { path, line, .. } => (Some(path), Some(*line)),
            Self::ParseDiagnostic { path, line, .. } => (
                Some(path),
                source_line(path, Some(*line).filter(|line| *line > 0)),
            ),
        }
    }
}

/// A notebook's line counts its joined code cells, not the JSON a reader
/// opens, so it points at no line.
fn source_line(path: &str, line: Option<u32>) -> Option<u32> {
    let notebook = Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ipynb"));
    line.filter(|_| !notebook)
}

fn manifest_location(warning: &ManifestWarning) -> (Option<&str>, Option<u32>) {
    match warning {
        ManifestWarning::InvalidRequirementLine { file, line, .. } => (Some(file), *line),
        ManifestWarning::RequirementsOptionIgnored { file, line, .. } => (Some(file), Some(*line)),
        ManifestWarning::SetupPyNotStatic { file }
        | ManifestWarning::RuntimeDependenciesUnknown { file }
        | ManifestWarning::SetupPyPartiallyStatic { file, .. }
        | ManifestWarning::RequirementsUnreadable { file, .. }
        | ManifestWarning::PathOutsideRoot { file, .. }
        | ManifestWarning::PathSourceMissing { file, .. }
        | ManifestWarning::PathSourceNotProject { file, .. }
        | ManifestWarning::LockfileSkipped { file, .. }
        | ManifestWarning::DependencyGroupIncludeUndefined { file, .. }
        | ManifestWarning::DependencyGroupIncludeCycle { file, .. }
        | ManifestWarning::FileUndecodable { file }
        | ManifestWarning::DynamicExtrasUnread { file, .. }
        | ManifestWarning::RequiresPythonUnreadable { file, .. }
        | ManifestWarning::InlineScriptRequiresPythonUnreadable { file, .. }
        | ManifestWarning::InlineScriptInvalid { file, .. }
        | ManifestWarning::EntryPointUnreadable { file, .. } => (Some(file), None),
        // The sections are named, not the file; a conflict spans two files;
        // a missing constraints file does not exist.
        ManifestWarning::PoetryDetected
        | ManifestWarning::PdmDetected
        | ManifestWarning::HatchDetected
        | ManifestWarning::MetadataConflict { .. }
        | ManifestWarning::RequirementsConstraintMissing { .. } => (None, None),
    }
}

impl fmt::Display for ProbeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(warning) => write_manifest_warning(formatter, warning),
            Self::MemberManifest { member, warning } => {
                write_member_manifest_warning(formatter, member, warning)
            },
            Self::Sources(warning) => fmt::Display::fmt(warning, formatter),
            Self::Plugin(warning) => fmt::Display::fmt(warning, formatter),
            Self::Entry(warning) => fmt::Display::fmt(warning, formatter),
            Self::Resolve(warning) => write_resolve_warning(formatter, warning),
            Self::AutoWorkspace { member_count } => write!(
                formatter,
                "workspace: treating {member_count} nested pyproject.toml as workspace members (disable with --no-auto-workspace)"
            ),
            Self::NestedProjectsSkipped { count: 1 } => formatter.write_str(
                "workspace: skipped 1 nested project that is not a workspace member (--no-auto-workspace keeps it)",
            ),
            Self::NestedProjectsSkipped { count } => write!(
                formatter,
                "workspace: skipped {count} nested projects that are not workspace members (--no-auto-workspace keeps them)"
            ),
            Self::SkippedSource {
                path,
                reason: SkipReason::Undecodable,
            } => write!(
                formatter,
                "parse: skipped `{path}`: not UTF-8 and no supported PEP 263 coding declaration"
            ),
            Self::SkippedSource {
                path,
                reason: SkipReason::SyntaxError { line, message },
            } => write!(
                formatter,
                "parse: skipped `{path}`: syntax error at line {line}: {message}"
            ),
            Self::SkippedSource {
                path,
                reason: SkipReason::InvalidNotebook { message },
            } => write!(formatter, "parse: skipped `{path}`: {message}"),
            Self::ParseDiagnostic {
                path,
                line: 0,
                message,
            } => write!(formatter, "parse: `{path}`: {message}"),
            Self::ParseDiagnostic {
                path,
                line,
                message,
            } => write!(formatter, "parse: `{path}:{line}`: {message}"),
            Self::OpaqueDynamicImport { path, more: 0 } => write!(
                formatter,
                "parse: `{path}` imports a module by a name computed at runtime; unused files are reported below certain"
            ),
            Self::OpaqueDynamicImport { path, more } => write!(
                formatter,
                "parse: `{path}` and {more} more reachable module{} import a module by a name computed at runtime; unused files are reported below certain",
                if *more == 1 { "" } else { "s" }
            ),
            Self::ArchiveRequirement {
                path,
                line,
                name,
                more,
            } => write_archive_requirement(formatter, path, *line, name, *more),
            Self::UnreadCommandTarget {
                path,
                line,
                target,
                more,
            } => write_command_target(formatter, path, *line, target, *more),
            Self::StringModuleRef {
                path,
                line,
                text,
                target,
                more,
            } => write_string_module_ref(formatter, (path, *line), text, target, *more),
        }
    }
}

fn write_string_module_ref(
    formatter: &mut fmt::Formatter<'_>,
    (path, line): (&str, u32),
    text: &str,
    target: &str,
    more: usize,
) -> fmt::Result {
    let site = crate::rules::import_site(path, Some(line));
    match more {
        0 => write!(
            formatter,
            "parse: `{site}` names `{text}` in a string that may load `{target}`, which no entry reaches; that file is reported as unused at info, if at all"
        ),
        more => write!(
            formatter,
            "parse: `{site}` names `{text}` and {more} more module{} in strings that may load files no entry reaches; those files are reported as unused at info, if at all",
            if more == 1 { "" } else { "s" }
        ),
    }
}

fn write_command_target(
    formatter: &mut fmt::Formatter<'_>,
    path: &str,
    line: Option<u32>,
    target: &str,
    more: usize,
) -> fmt::Result {
    let site = crate::rules::import_site(path, line);
    let others = match more {
        0 => String::new(),
        1 => " and 1 more target".to_owned(),
        more => format!(" and {more} more targets"),
    };
    write!(
        formatter,
        "plugin: `{site}` runs `{target}`{others} as Python code no entry reaches; unused files are reported below certain"
    )
}

fn write_archive_requirement(
    formatter: &mut fmt::Formatter<'_>,
    path: &str,
    line: Option<u32>,
    name: &str,
    more: usize,
) -> fmt::Result {
    let site = crate::rules::import_site(path, line);
    match more {
        0 => write!(
            formatter,
            "manifest: `{site}` installs `{name}` from an archive chokkin does not read; findings on imports of `{name}` are reported as hints"
        ),
        more => write!(
            formatter,
            "manifest: `{site}` installs `{name}` and {more} more distribution{} from archives chokkin does not read; findings on imports of them are reported as hints",
            if more == 1 { "" } else { "s" }
        ),
    }
}

fn write_resolve_warning(
    formatter: &mut fmt::Formatter<'_>,
    warning: &ResolveWarning,
) -> fmt::Result {
    match warning {
        ResolveWarning::AmbiguousImport { import, candidates } => write!(
            formatter,
            "resolve: import `{import}` maps to several distributions ({})",
            candidates.join(", ")
        ),
        ResolveWarning::UnresolvedImport { import, file, .. } => write!(
            formatter,
            "resolve: could not resolve import `{import}` in `{file}`"
        ),
        ResolveWarning::VenvDistributionIgnored { path, name } => write!(
            formatter,
            "resolve: ignored `{path}`: `{name}` is not a PEP 508 distribution name"
        ),
        ResolveWarning::VenvUnreadable { path, reason } => write!(
            formatter,
            "resolve: could not read virtualenv `{path}`: {reason}"
        ),
    }
}

/// A member's warning reads like the root's, except that a warning naming no
/// file names the member, and runtime dependencies it could not read lower
/// only that member's findings (#910).
fn write_member_manifest_warning(
    formatter: &mut fmt::Formatter<'_>,
    member: &str,
    warning: &ManifestWarning,
) -> fmt::Result {
    match warning {
        ManifestWarning::RuntimeDependenciesUnknown { file } => write!(
            formatter,
            "manifest: runtime dependencies in `{file}` could not be read statically; missing-dependency findings in `{member}` are reported as low-confidence info"
        ),
        // The resolver reads the root's `requires-python` alone, so a
        // member's assumes nothing that a finding depends on.
        ManifestWarning::RequiresPythonUnreadable { file, value } => write!(
            formatter,
            "manifest: could not read `requires-python` \"{value}\" in `{file}`; assuming target version py311"
        ),
        ManifestWarning::PoetryDetected
        | ManifestWarning::PdmDetected
        | ManifestWarning::HatchDetected
        | ManifestWarning::RequirementsConstraintMissing { .. } => {
            let text = ProbeWarning::Manifest(warning.clone()).to_string();
            let rest = text.strip_prefix("manifest: ").unwrap_or(&text);
            write!(formatter, "manifest: `{member}`: {rest}")
        },
        _ => write_manifest_warning(formatter, warning),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per variant; splitting the match would only scatter the texts"
)]
fn write_manifest_warning(
    formatter: &mut fmt::Formatter<'_>,
    warning: &ManifestWarning,
) -> fmt::Result {
    match warning {
        ManifestWarning::SetupPyNotStatic { file } => write!(
            formatter,
            "manifest: skipped non-static setup.py at `{file}`"
        ),
        ManifestWarning::RuntimeDependenciesUnknown { file } => write!(
            formatter,
            "manifest: runtime dependencies in `{file}` could not be read statically; missing-dependency findings are reported as low-confidence info"
        ),
        ManifestWarning::PoetryDetected => write!(
            formatter,
            "manifest: Poetry sections detected (partial dependency extraction)"
        ),
        ManifestWarning::PdmDetected => write!(
            formatter,
            "manifest: PDM sections detected (partial dependency extraction)"
        ),
        ManifestWarning::HatchDetected => write!(
            formatter,
            "manifest: Hatch sections detected (partial dependency extraction)"
        ),
        ManifestWarning::InvalidRequirementLine {
            file,
            line,
            label,
            raw,
            context,
        } => {
            let at = line.map_or_else(
                || format!("`{file}` ({label})"),
                |line| format!("`{file}:{line}`"),
            );
            write!(formatter, "manifest: invalid requirement at {at}: {raw}")?;
            // A group's bucket is config's to decide, which the text does not
            // see; only the fixed runtime contexts name misplaced findings. A
            // PEP 723 block has no misplaced findings (#936), and a build
            // requirement lowers like a dev one.
            let script = label.starts_with("script.");
            let findings = if script
                || matches!(
                    context,
                    DependencyContext::Group(_) | DependencyContext::Build
                ) {
                "undeclared imports"
            } else {
                "undeclared or misplaced imports"
            };
            match invalid_requirement_name(raw) {
                Some(name) => write!(formatter, "; {findings} of `{name}` are reported as hints"),
                None => Ok(()),
            }
        },
        ManifestWarning::SetupPyPartiallyStatic { file, argument } => write!(
            formatter,
            "manifest: partially static setup.py `{file}` (argument `{argument}`)"
        ),
        ManifestWarning::MetadataConflict {
            field,
            kept,
            ignored,
            kept_source,
            ignored_source,
        } => write!(
            formatter,
            "manifest: metadata conflict on `{field}`: kept `{kept}` from `{kept_source}`, ignored `{ignored}` from `{ignored_source}`"
        ),
        ManifestWarning::RequirementsOptionIgnored { file, line, raw } => write!(
            formatter,
            "manifest: ignored requirements option at `{file}:{line}`: {raw}"
        ),
        ManifestWarning::RequirementsConstraintMissing { path } => {
            write!(formatter, "manifest: missing constraints file `{path}`")
        },
        ManifestWarning::PathOutsideRoot { file, field, path } => write!(
            formatter,
            "manifest: `{path}` (`{field}` in `{file}`) resolves outside the project root and was not read"
        ),
        ManifestWarning::PathSourceMissing { file, field, path } => write!(
            formatter,
            "manifest: `{path}` (`{field}` in `{file}`) does not exist"
        ),
        ManifestWarning::PathSourceNotProject { file, field, path } => write!(
            formatter,
            "manifest: `{path}` (`{field}` in `{file}`) has no `pyproject.toml` with a `[project]` table, so its code is analyzed as the root's without its own entries or dependencies; findings under it are lowered"
        ),
        ManifestWarning::RequirementsUnreadable { file, reason } => {
            write!(formatter, "manifest: stopped reading `{file}`: {reason}")
        },
        ManifestWarning::LockfileSkipped { file, reason } => {
            write!(formatter, "manifest: skipped lockfile `{file}`: {reason}")
        },
        ManifestWarning::FileUndecodable { file } => write!(
            formatter,
            "manifest: skipped `{file}`: not UTF-8 and no supported PEP 263 coding declaration"
        ),
        ManifestWarning::DynamicExtrasUnread { file, extras } => {
            write_dynamic_extras_unread(formatter, file, extras)
        },
        ManifestWarning::RequiresPythonUnreadable { file, value } => write!(
            formatter,
            "manifest: could not read `requires-python` \"{value}\" in `{file}`; assuming target version py311; imports of modules only other versions ship as stdlib are reported as hints"
        ),
        ManifestWarning::InlineScriptRequiresPythonUnreadable { file, value } => write!(
            formatter,
            "manifest: could not read `requires-python` \"{value}\" in the PEP 723 block of `{file}`; using the project's target version; its imports of modules only other versions ship as stdlib are reported as hints"
        ),
        ManifestWarning::InlineScriptInvalid { file, reason } => write!(
            formatter,
            "manifest: ignored PEP 723 script block in `{file}`: {reason}"
        ),
        ManifestWarning::EntryPointUnreadable {
            file,
            label,
            reason,
        } => write!(
            formatter,
            "manifest: skipped entry point `{label}` in `{file}`: {reason}"
        ),
        ManifestWarning::DependencyGroupIncludeUndefined {
            file,
            group,
            include,
        } => write!(
            formatter,
            "manifest: dependency group `{group}` in `{file}` includes undefined group `{include}`"
        ),
        ManifestWarning::DependencyGroupIncludeCycle { file, groups } => write!(
            formatter,
            "manifest: dependency groups in `{file}` include each other in a cycle: {}",
            groups.join(" -> ")
        ),
    }
}

fn write_dynamic_extras_unread(
    formatter: &mut fmt::Formatter<'_>,
    file: &str,
    extras: &[String],
) -> fmt::Result {
    if extras.is_empty() {
        return write!(
            formatter,
            "manifest: `{file}` lists `optional-dependencies` in `dynamic`, but no source chokkin reads fills them"
        );
    }
    let names: Vec<String> = extras.iter().map(|extra| format!("`{extra}`")).collect();
    write!(
        formatter,
        "manifest: extras {} in `{file}` are filled at build time and could not be read statically",
        names.join(", ")
    )
}

/// Each warning once, in order, told apart by its text. Two stages can
/// report the same file (a shared `-r` include, a member the root also
/// scans), and the reader needs it once.
#[must_use]
pub fn diagnostic_lines<'a>(
    warnings: impl IntoIterator<Item = &'a ProbeWarning>,
) -> Vec<Diagnostic> {
    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .map(ProbeWarning::diagnostic)
        .filter(|diagnostic| seen.insert(diagnostic.text.clone()))
        .collect()
}

/// Write warning lines to `err`, repeated kinds condensed.
pub fn write_diagnostics(diagnostics: &[Diagnostic], err: &mut impl Write) -> io::Result<()> {
    for diagnostic in condense_diagnostics(diagnostics) {
        writeln!(err, "{}", escape_control(&diagnostic.text))?;
    }
    Ok(())
}

pub fn write_probe_warnings(warnings: &[ProbeWarning], err: &mut impl Write) -> io::Result<()> {
    write_diagnostics(&diagnostic_lines(warnings), err)
}

pub(super) fn collect_warnings(
    manifest: &crate::manifest::LoadedManifest,
    sources: &crate::sources::DiscoveredSources,
) -> Vec<ProbeWarning> {
    let mut warnings = Vec::new();
    warnings.extend(
        manifest
            .warnings
            .iter()
            .cloned()
            .map(ProbeWarning::Manifest),
    );
    warnings.extend(sources.warnings.iter().cloned().map(ProbeWarning::Sources));
    warnings
}

#[cfg(test)]
mod tests {
    use crate::config::PluginId;
    use crate::entry::EntryWarning;
    use crate::manifest::{DependencyContext, ManifestWarning};
    use crate::plugins::PluginsWarning;
    use crate::sources::SourcesWarning;

    use super::{ProbeWarning, SkipReason, write_probe_warnings};

    fn located(warning: &ProbeWarning) -> (Option<String>, Option<u32>) {
        let diagnostic = warning.diagnostic();
        (diagnostic.file, diagnostic.line)
    }

    /// One opaque-import warning names the first module and counts the rest (#909).
    #[test]
    fn opaque_import_warning_names_the_first_module() {
        let one = ProbeWarning::OpaqueDynamicImport {
            path: "acme/main.py".to_owned(),
            more: 0,
        };
        let three = ProbeWarning::OpaqueDynamicImport {
            path: "acme/main.py".to_owned(),
            more: 2,
        };
        assert_eq!(
            one.to_string(),
            "parse: `acme/main.py` imports a module by a name computed at runtime; unused files are reported below certain"
        );
        assert_eq!(
            three.to_string(),
            "parse: `acme/main.py` and 2 more reachable modules import a module by a name computed at runtime; unused files are reported below certain"
        );
        let two = ProbeWarning::OpaqueDynamicImport {
            path: "acme/main.py".to_owned(),
            more: 1,
        };
        assert_eq!(
            two.to_string(),
            "parse: `acme/main.py` and 1 more reachable module import a module by a name computed at runtime; unused files are reported below certain"
        );
        assert_eq!(located(&three), (Some("acme/main.py".to_owned()), None));
    }

    /// One archive-requirement warning names the first line and counts the rest (#988).
    #[test]
    fn archive_requirement_warning_names_the_first_line() {
        let warning = |more| ProbeWarning::ArchiveRequirement {
            path: "requirements.txt".to_owned(),
            line: Some(2),
            name: "bar".to_owned(),
            more,
        };
        assert_eq!(
            warning(0).to_string(),
            "manifest: `requirements.txt:2` installs `bar` from an archive chokkin does not read; findings on imports of `bar` are reported as hints"
        );
        assert_eq!(
            warning(1).to_string(),
            "manifest: `requirements.txt:2` installs `bar` and 1 more distribution from archives chokkin does not read; findings on imports of them are reported as hints"
        );
        assert_eq!(
            warning(2).to_string(),
            "manifest: `requirements.txt:2` installs `bar` and 2 more distributions from archives chokkin does not read; findings on imports of them are reported as hints"
        );
        assert_eq!(
            located(&warning(0)),
            (Some("requirements.txt".to_owned()), Some(2))
        );
    }

    /// The unread-extras warning names the extras, or says none were named (#905).
    #[test]
    fn dynamic_extras_warning_names_the_unread_extras() {
        let unread = |extras: &[&str]| {
            ProbeWarning::Manifest(ManifestWarning::DynamicExtrasUnread {
                file: "pyproject.toml".to_owned(),
                extras: extras.iter().map(|extra| (*extra).to_owned()).collect(),
            })
            .to_string()
        };
        assert_eq!(
            unread(&[]),
            "manifest: `pyproject.toml` lists `optional-dependencies` in `dynamic`, but no source chokkin reads fills them"
        );
        assert_eq!(
            unread(&["s3", "gcs"]),
            "manifest: extras `s3`, `gcs` in `pyproject.toml` are filled at build time and could not be read statically"
        );
    }

    /// A warning lands on the file and line it names (#895).
    #[test]
    fn warnings_locate_their_file_and_line() {
        let syntax_error = ProbeWarning::SkippedSource {
            path: "acme/broken.py".to_owned(),
            reason: SkipReason::SyntaxError {
                line: 9,
                message: "Expected a statement".to_owned(),
            },
        };
        let option = ProbeWarning::MemberManifest {
            member: "packages/a".to_owned(),
            warning: ManifestWarning::RequirementsOptionIgnored {
                file: "packages/a/requirements.txt".to_owned(),
                line: 4,
                raw: "--hash=sha256:00".to_owned(),
            },
        };
        let undecodable = ProbeWarning::SkippedSource {
            path: "acme/latin.py".to_owned(),
            reason: SkipReason::Undecodable,
        };
        let diagnostic = |line| ProbeWarning::ParseDiagnostic {
            path: "acme/aliases.py".to_owned(),
            line,
            message: "file uses `type` aliases; set target_version to py312".to_owned(),
        };

        assert_eq!(
            located(&syntax_error),
            (Some("acme/broken.py".to_owned()), Some(9))
        );
        assert_eq!(
            located(&option),
            (Some("packages/a/requirements.txt".to_owned()), Some(4))
        );
        assert_eq!(
            located(&undecodable),
            (Some("acme/latin.py".to_owned()), None)
        );
        assert_eq!(
            located(&diagnostic(1)),
            (Some("acme/aliases.py".to_owned()), Some(1))
        );
        // Line 0 is a file-wide diagnostic, not a position.
        assert_eq!(
            located(&diagnostic(0)),
            (Some("acme/aliases.py".to_owned()), None)
        );
        assert_eq!(
            located(&ProbeWarning::Manifest(ManifestWarning::HatchDetected)),
            (None, None)
        );
    }

    /// A notebook's line numbers count its joined code cells, not the JSON a
    /// reader opens, so its warnings point at the file alone (#895).
    #[test]
    fn notebook_warnings_point_at_the_file_alone() {
        let syntax_error = ProbeWarning::SkippedSource {
            path: "acme/demo.ipynb".to_owned(),
            reason: SkipReason::SyntaxError {
                line: 3,
                message: "Expected a statement".to_owned(),
            },
        };
        let diagnostic = ProbeWarning::ParseDiagnostic {
            path: "acme/Demo.IPYNB".to_owned(),
            line: 1,
            message: "relative import without a package".to_owned(),
        };

        assert_eq!(
            located(&syntax_error),
            (Some("acme/demo.ipynb".to_owned()), None)
        );
        assert_eq!(
            located(&diagnostic),
            (Some("acme/Demo.IPYNB".to_owned()), None)
        );
    }

    #[test]
    fn plugin_warning_formats_partial_settings_parse() {
        let warnings = [ProbeWarning::Plugin(PluginsWarning::PartialSettingsParse {
            path: "mysite/settings.py".to_owned(),
            fields: vec!["INSTALLED_APPS".to_owned()],
        })];

        let mut output = Vec::new();
        write_probe_warnings(&warnings, &mut output).unwrap();
        let stderr = String::from_utf8(output).unwrap();

        assert!(stderr.contains("plugin: partial Django settings parse"));
        assert!(stderr.contains("fields: INSTALLED_APPS"));
    }

    #[test]
    fn plugin_and_sources_warnings_render_exact_text() {
        let cases = [
            (
                ProbeWarning::Plugin(PluginsWarning::PartialSettingsParse {
                    path: "s.py".to_owned(),
                    fields: vec!["A".to_owned(), "B".to_owned()],
                }),
                "plugin: partial Django settings parse at `s.py` (fields: A, B)",
            ),
            (
                ProbeWarning::Plugin(PluginsWarning::PytestConfigUnreadable {
                    path: "pytest.ini".to_owned(),
                }),
                "plugin: unreadable pytest config `pytest.ini`",
            ),
            (
                ProbeWarning::Plugin(PluginsWarning::AmbiguousSettings {
                    chosen: "a.py".to_owned(),
                    candidates: vec!["a.py".to_owned(), "b.py".to_owned()],
                }),
                r#"plugin: ambiguous Django settings; chose `a.py` from ["a.py", "b.py"]"#,
            ),
            (
                ProbeWarning::Plugin(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Django,
                    detail: "boom".to_owned(),
                }),
                "plugin: `django` extraction failed: boom",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::MissingEntryPath {
                    path: "x.py".to_owned(),
                }),
                "sources: missing entry path `x.py`",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::EntryPathIsDirectory {
                    path: "pkg".to_owned(),
                }),
                "sources: entry path is a directory `pkg`",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::AmbiguousFlatLayout {
                    candidates: vec!["a".to_owned(), "b".to_owned()],
                    chosen: "a".to_owned(),
                }),
                r#"sources: ambiguous package directory (["a", "b"]); chose `a`"#,
            ),
            (
                ProbeWarning::Sources(SourcesWarning::GuessedPackageDir {
                    project: "acme".to_owned(),
                    chosen: "lib/other".to_owned(),
                }),
                "sources: no package directory matches project `acme`; guessed `lib/other` \
                 (declare it in the build backend config or [tool.chokkin] project)",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::GitignoreUnreadable {
                    path: ".gitignore".to_owned(),
                }),
                "sources: could not read `.gitignore`; its ignore rules may be missing",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::LargeProject { file_count: 12 }),
                "sources: large project (12 files discovered)",
            ),
            (
                ProbeWarning::Sources(SourcesWarning::PathUnreadable {
                    path: "d".to_owned(),
                    reason: "denied".to_owned(),
                }),
                "sources: could not read `d`: denied",
            ),
        ];

        for (warning, expected) in cases {
            assert_eq!(warning.to_string(), expected);
        }
    }

    #[test]
    fn skipped_source_names_its_reason() {
        let undecodable = ProbeWarning::SkippedSource {
            path: "a.py".to_owned(),
            reason: SkipReason::Undecodable,
        };
        let syntax = ProbeWarning::SkippedSource {
            path: "b.py".to_owned(),
            reason: SkipReason::SyntaxError {
                line: 3,
                message: "Expected an expression".to_owned(),
            },
        };

        assert_eq!(
            undecodable.to_string(),
            "parse: skipped `a.py`: not UTF-8 and no supported PEP 263 coding declaration"
        );
        assert_eq!(
            syntax.to_string(),
            "parse: skipped `b.py`: syntax error at line 3: Expected an expression"
        );
    }

    #[test]
    fn parse_diagnostic_names_its_line_unless_it_covers_the_file() {
        let diagnostic = |line| ProbeWarning::ParseDiagnostic {
            path: "a.py".to_owned(),
            line,
            message: "unsupported `__all__` assignment form".to_owned(),
        };

        assert_eq!(
            diagnostic(4).to_string(),
            "parse: `a.py:4`: unsupported `__all__` assignment form"
        );
        assert_eq!(
            diagnostic(0).to_string(),
            "parse: `a.py`: unsupported `__all__` assignment form"
        );
    }

    #[test]
    fn entry_warnings_render_exact_text() {
        assert_eq!(
            ProbeWarning::Entry(EntryWarning::MissingEntryPath {
                path: "a.py".to_owned(),
            })
            .to_string(),
            "entry: skipped entry `a.py`: not a discovered source file; unused files and dependencies are reported below certain"
        );
        assert_eq!(
            ProbeWarning::Entry(EntryWarning::UnresolvedModuleTarget {
                module: "acme.cli".to_owned(),
                file: "pyproject.toml".to_owned(),
                label: "project.scripts.acme".to_owned(),
            })
            .to_string(),
            "entry: could not map `acme.cli` to a source file (`project.scripts.acme` in `pyproject.toml`)"
        );
    }

    #[test]
    fn member_manifest_warnings_name_root_relative_files_or_the_member() {
        let member = |mut warning: ManifestWarning| {
            warning.rebase("packages/x");
            ProbeWarning::MemberManifest {
                member: "packages/x".to_owned(),
                warning,
            }
        };
        let cases = [
            (
                member(ManifestWarning::MetadataConflict {
                    field: "name".to_owned(),
                    kept: "a".to_owned(),
                    ignored: "b".to_owned(),
                    kept_source: "pyproject.toml".to_owned(),
                    ignored_source: "setup.cfg".to_owned(),
                }),
                "manifest: metadata conflict on `name`: kept `a` from `packages/x/pyproject.toml`, ignored `b` from `packages/x/setup.cfg`",
            ),
            (
                member(ManifestWarning::HatchDetected),
                "manifest: `packages/x`: Hatch sections detected (partial dependency extraction)",
            ),
            (
                member(ManifestWarning::RequirementsConstraintMissing {
                    path: "c.txt".to_owned(),
                }),
                "manifest: `packages/x`: missing constraints file `c.txt`",
            ),
            (
                member(ManifestWarning::RuntimeDependenciesUnknown {
                    file: "setup.py".to_owned(),
                }),
                "manifest: runtime dependencies in `packages/x/setup.py` could not be read statically; missing-dependency findings in `packages/x` are reported as low-confidence info",
            ),
            (
                ProbeWarning::Manifest(ManifestWarning::RequirementsUnreadable {
                    file: "requirements/ci.txt".to_owned(),
                    reason: "could not be read".to_owned(),
                }),
                "manifest: stopped reading `requirements/ci.txt`: could not be read",
            ),
        ];
        for (warning, expected) in cases {
            assert_eq!(warning.to_string(), expected);
        }
    }

    #[test]
    fn undecodable_manifest_file_renders_like_a_skipped_source() {
        let warning = ProbeWarning::Manifest(ManifestWarning::FileUndecodable {
            file: "setup.py".to_owned(),
        });

        assert_eq!(
            warning.to_string(),
            "manifest: skipped `setup.py`: not UTF-8 and no supported PEP 263 coding declaration"
        );
    }

    #[test]
    fn invalid_requirement_shows_line_or_label() {
        let warning = |line| {
            ProbeWarning::Manifest(ManifestWarning::InvalidRequirementLine {
                file: "pyproject.toml".to_owned(),
                line,
                label: "project.dependencies[0]".to_owned(),
                raw: "foo @".to_owned(),
                context: DependencyContext::Runtime,
            })
        };

        assert_eq!(
            warning(Some(3)).to_string(),
            "manifest: invalid requirement at `pyproject.toml:3`: foo @; undeclared or misplaced imports of `foo` are reported as hints"
        );
        assert_eq!(
            warning(None).to_string(),
            "manifest: invalid requirement at `pyproject.toml` (project.dependencies[0]): foo @; undeclared or misplaced imports of `foo` are reported as hints"
        );
        // A PEP 723 block has no misplaced findings to name (#936).
        let script = ProbeWarning::Manifest(ManifestWarning::InvalidRequirementLine {
            file: "tool.py".to_owned(),
            line: Some(4),
            label: "script.dependencies[0]".to_owned(),
            raw: "rich>=".to_owned(),
            context: DependencyContext::Runtime,
        });
        assert_eq!(
            script.to_string(),
            "manifest: invalid requirement at `tool.py:4`: rich>=; undeclared imports of `rich` are reported as hints"
        );
        let build = ProbeWarning::Manifest(ManifestWarning::InvalidRequirementLine {
            file: "pyproject.toml".to_owned(),
            line: Some(2),
            label: "build-system.requires[0]".to_owned(),
            raw: "hatchling>=".to_owned(),
            context: DependencyContext::Build,
        });
        assert_eq!(
            build.to_string(),
            "manifest: invalid requirement at `pyproject.toml:2`: hatchling>=; undeclared imports of `hatchling` are reported as hints"
        );
    }

    #[test]
    fn nested_projects_skipped_agrees_in_number() {
        assert_eq!(
            ProbeWarning::NestedProjectsSkipped { count: 4 }.to_string(),
            "workspace: skipped 4 nested projects that are not workspace members (--no-auto-workspace keeps them)"
        );
        assert_eq!(
            ProbeWarning::NestedProjectsSkipped { count: 1 }.to_string(),
            "workspace: skipped 1 nested project that is not a workspace member (--no-auto-workspace keeps it)"
        );
    }
}
