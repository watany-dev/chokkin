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
    AutoWorkspace { member_count: usize },
    /// A source that could not be decoded and was left out of the analysis.
    SkippedSource {
        /// Root-relative path of the source.
        path: String,
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
            Self::SkippedSource { path } => write!(
                formatter,
                "parse: skipped `{path}`: not UTF-8 and no supported PEP 263 coding declaration"
            ),
        }
    }
}

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
        } => {
            let at = line.map_or_else(
                || format!("`{file}` ({label})"),
                |line| format!("`{file}:{line}`"),
            );
            write!(formatter, "manifest: invalid requirement at {at}: {raw}")
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
        ManifestWarning::FileUndecodable { file } => write!(
            formatter,
            "manifest: skipped `{file}`: not UTF-8 and no supported PEP 263 coding declaration"
        ),
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
    use crate::manifest::ManifestWarning;
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
            })
        };

        assert_eq!(
            warning(Some(3)).to_string(),
            "manifest: invalid requirement at `pyproject.toml:3`: foo @"
        );
        assert_eq!(
            warning(None).to_string(),
            "manifest: invalid requirement at `pyproject.toml` (project.dependencies[0]): foo @"
        );
    }
}
