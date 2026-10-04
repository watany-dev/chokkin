//! Pipeline warning aggregation and display.

use std::fmt;
use std::io::{self, Write};

use crate::manifest::ManifestWarning;
use crate::plugins::PluginsWarning;
use crate::sources::SourcesWarning;

/// Non-fatal warning collected during probe or analysis pipeline steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeWarning {
    /// Warning from manifest extraction.
    Manifest(ManifestWarning),
    /// Warning from source file discovery.
    Sources(SourcesWarning),
    /// Warning from plugin hint extraction.
    Plugin(PluginsWarning),
    /// Workspace members were inferred from nested `pyproject.toml` files.
    AutoWorkspace {
        /// Number of inferred members.
        member_count: usize,
    },
}

impl fmt::Display for ProbeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(warning) => write_manifest_warning(formatter, warning),
            Self::Sources(warning) => fmt::Display::fmt(warning, formatter),
            Self::Plugin(warning) => fmt::Display::fmt(warning, formatter),
            Self::AutoWorkspace { member_count } => write!(
                formatter,
                "workspace: treating {member_count} nested pyproject.toml as workspace members (disable with --no-auto-workspace)"
            ),
        }
    }
}

fn write_manifest_warning(
    formatter: &mut fmt::Formatter<'_>,
    warning: &ManifestWarning,
) -> fmt::Result {
    match warning {
        ManifestWarning::SetupPyNotStatic { file } => {
            write!(
                formatter,
                "manifest: skipped non-static setup.py at `{file}`"
            )
        },
        ManifestWarning::PoetryDetected => {
            write!(
                formatter,
                "manifest: Poetry sections detected (partial dependency extraction)"
            )
        },
        ManifestWarning::PdmDetected => {
            write!(
                formatter,
                "manifest: PDM sections detected (partial dependency extraction)"
            )
        },
        ManifestWarning::HatchDetected => {
            write!(
                formatter,
                "manifest: Hatch sections detected (partial dependency extraction)"
            )
        },
        ManifestWarning::InvalidRequirementLine { file, line, raw } => write!(
            formatter,
            "manifest: invalid requirement at `{file}:{line}`: {raw}"
        ),
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
        ManifestWarning::InlineScriptInvalid { file, reason } => write!(
            formatter,
            "manifest: ignored PEP 723 script block in `{file}`: {reason}"
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

/// Write pipeline warnings to `err`, one per line.
pub fn write_probe_warnings(warnings: &[ProbeWarning], err: &mut impl Write) -> io::Result<()> {
    for warning in warnings {
        writeln!(err, "{warning}")?;
    }
    Ok(())
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
    use crate::plugins::PluginsWarning;
    use crate::sources::SourcesWarning;

    use super::{ProbeWarning, write_probe_warnings};

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
                r#"sources: ambiguous flat layout (["a", "b"]); chose `a`"#,
            ),
            (
                ProbeWarning::Sources(SourcesWarning::GitignoreUnreadable {
                    path: ".gitignore".to_owned(),
                }),
                "sources: could not read `.gitignore` at `.gitignore`",
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
}
