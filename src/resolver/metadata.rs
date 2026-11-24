//! PEP 794 / Core Metadata parsing helpers for dist-info.

/// Parsed metadata fields used by the resolver.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct DistMetadata {
    /// Distribution `Name` field.
    pub name: Option<String>,
    /// PEP 794 `Import-Name` values, without `; private`; an empty value
    /// ("provides no import names") is dropped.
    pub import_names: Vec<String>,
    /// PEP 794 `Import-Namespace` values, without `; private`.
    pub import_namespaces: Vec<String>,
}

/// Parse selected fields from a `METADATA` / `PKG-INFO` body.
///
/// Core Metadata is RFC 822: the header ends at the first empty line (the
/// rest is the description, often a README quoting field lines, #1395),
/// field names are case-insensitive, and a line starting with whitespace
/// continues the previous field. A header line without `:` is skipped:
/// `email.parser` treats it as a defect and reads none of the fields we use
/// from it.
#[must_use]
pub(super) fn parse_metadata(contents: &str) -> DistMetadata {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in contents.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }

    let mut metadata = DistMetadata::default();
    for (key, value) in fields {
        match key.as_str() {
            "name" if metadata.name.is_none() => metadata.name = Some(value),
            "import-name" => metadata.import_names.extend(import_name_value(&value)),
            "import-namespace" => metadata.import_namespaces.extend(import_name_value(&value)),
            _ => {},
        }
    }
    metadata
}

/// PEP 794 value: the dotted name before an optional `; private` (#1398).
fn import_name_value(value: &str) -> Option<String> {
    let name = value.split(';').next().unwrap_or_default().trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_import_name_fields() {
        let body = "Name: demo\nImport-Name: demo_pkg\nImport-Namespace: demo_ns\n";
        let metadata = parse_metadata(body);
        assert_eq!(metadata.name.as_deref(), Some("demo"));
        assert_eq!(metadata.import_names, vec!["demo_pkg".to_owned()]);
        assert_eq!(metadata.import_namespaces, vec!["demo_ns".to_owned()]);
    }

    #[test]
    fn first_name_field_wins_over_later_name_lines() {
        let body = "Name: demo\nVersion: 1.0\nName: second\n";
        assert_eq!(parse_metadata(body).name.as_deref(), Some("demo"));
    }

    /// The description after the first empty line is no header, whatever
    /// field it quotes (#1395).
    #[test]
    fn header_ends_at_the_first_empty_line() {
        let body = "Name: docs-helper\nVersion: 1.0\n\nExample:\n\nImport-Name: widget\nName: x\n";
        let metadata = parse_metadata(body);
        assert_eq!(metadata.name.as_deref(), Some("docs-helper"));
        assert!(metadata.import_names.is_empty());
        let crlf = "Name: a\r\nVersion: 1\r\n\r\nImport-Name: b\r\n";
        assert!(parse_metadata(crlf).import_names.is_empty());
    }

    #[test]
    fn field_names_are_case_insensitive_and_lines_continue() {
        let body = "name: demo\nimport-name: demo_pkg\nIMPORT-NAMESPACE:\n  demo_ns\n";
        let metadata = parse_metadata(body);
        assert_eq!(metadata.name.as_deref(), Some("demo"));
        assert_eq!(metadata.import_names, vec!["demo_pkg".to_owned()]);
        assert_eq!(metadata.import_namespaces, vec!["demo_ns".to_owned()]);
    }

    /// `; private` is no part of the name, and an empty value names nothing
    /// (#1398).
    #[test]
    fn import_name_drops_private_and_empty_values() {
        let body =
            "Name: a\nImport-Name: foo._impl ; private\nImport-Name:\nImport-Name: bar.core\n";
        assert_eq!(
            parse_metadata(body).import_names,
            vec!["foo._impl".to_owned(), "bar.core".to_owned()]
        );
    }
}
