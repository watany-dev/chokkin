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
}

impl fmt::Display for ProbeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(warning) => write_manifest_warning(formatter, warning),
            Self::Sources(warning) => write_sources_warning(formatter, warning),
            Self::Plugin(warning) => write_plugin_warning(formatter, warning),
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
    }
}

fn write_plugin_warning(
    formatter: &mut fmt::Formatter<'_>,
    warning: &PluginsWarning,
) -> fmt::Result {
    match warning {
        PluginsWarning::PluginNoOp { plugin } => {
            write!(formatter, "plugin: `{}` produced no hints", plugin.as_key())
        },
        PluginsWarning::PartialSettingsParse { path, fields } => write!(
            formatter,
            "plugin: partial Django settings parse at `{path}` (fields: {})",
            fields.join(", ")
        ),
        PluginsWarning::PytestConfigUnreadable { path } => {
            write!(formatter, "plugin: unreadable pytest config `{path}`")
        },
        PluginsWarning::AmbiguousSettings { chosen, candidates } => write!(
            formatter,
            "plugin: ambiguous Django settings; chose `{chosen}` from {candidates:?}"
        ),
        PluginsWarning::PluginExtractFailed { plugin, detail } => write!(
            formatter,
            "plugin: `{}` extraction failed: {detail}",
            plugin.as_key()
        ),
    }
}

fn write_sources_warning(
    formatter: &mut fmt::Formatter<'_>,
    warning: &SourcesWarning,
) -> fmt::Result {
    match warning {
        SourcesWarning::MissingEntryPath { path } => {
            write!(formatter, "sources: missing entry path `{path}`")
        },
        SourcesWarning::EntryPathIsDirectory { path } => {
            write!(formatter, "sources: entry path is a directory `{path}`")
        },
        SourcesWarning::AmbiguousFlatLayout { candidates, chosen } => write!(
            formatter,
            "sources: ambiguous flat layout ({candidates:?}); chose `{chosen}`"
        ),
        SourcesWarning::GitignoreUnreadable { path } => {
            write!(
                formatter,
                "sources: could not read `.gitignore` at `{path}`"
            )
        },
        SourcesWarning::LargeProject { file_count } => write!(
            formatter,
            "sources: large project ({file_count} files discovered)"
        ),
        SourcesWarning::PathUnreadable { path, reason } => {
            write!(formatter, "sources: could not read `{path}`: {reason}")
        },
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

pub(super) fn actionable_plugin_warnings(
    plugins: &crate::plugins::PluginHints,
) -> Vec<ProbeWarning> {
    plugins
        .warnings
        .iter()
        .filter(|warning| !matches!(warning, PluginsWarning::PluginNoOp { .. }))
        .cloned()
        .map(ProbeWarning::Plugin)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::PluginId;
    use crate::plugins::{PluginHints, PluginsWarning};

    use super::{ProbeWarning, actionable_plugin_warnings, write_probe_warnings};

    #[test]
    fn actionable_plugin_warnings_skip_noop_and_format_remaining() {
        let hints = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            warnings: vec![
                PluginsWarning::PluginNoOp {
                    plugin: PluginId::Pytest,
                },
                PluginsWarning::PartialSettingsParse {
                    path: "mysite/settings.py".to_owned(),
                    fields: vec!["INSTALLED_APPS".to_owned()],
                },
            ],
        };

        let warnings = actionable_plugin_warnings(&hints);

        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings.as_slice(),
            [ProbeWarning::Plugin(PluginsWarning::PartialSettingsParse { path, fields })]
                if path == "mysite/settings.py" && fields.as_slice() == ["INSTALLED_APPS"]
        ));

        let mut output = Vec::new();
        write_probe_warnings(&warnings, &mut output).unwrap();
        let stderr = String::from_utf8(output).unwrap();

        assert!(stderr.contains("plugin: partial Django settings parse"));
        assert!(stderr.contains("fields: INSTALLED_APPS"));
        assert!(!stderr.contains("produced no hints"));
    }
}
