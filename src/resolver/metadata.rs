//! PEP 794 / Core Metadata parsing helpers for dist-info.

/// Parsed metadata fields used by the resolver.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DistMetadata {
    /// Distribution `Name` field.
    pub name: Option<String>,
    /// PEP 794 `Import-Name` values.
    pub import_names: Vec<String>,
    /// PEP 794 `Import-Namespace` values.
    pub import_namespaces: Vec<String>,
}

/// Parse selected metadata fields from a `METADATA` file body.
#[must_use]
pub fn parse_metadata(contents: &str) -> DistMetadata {
    let mut metadata = DistMetadata::default();
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "Name" if metadata.name.is_none() => metadata.name = Some(value.to_owned()),
                "Import-Name" => metadata.import_names.push(value.to_owned()),
                "Import-Namespace" => metadata.import_namespaces.push(value.to_owned()),
                _ => {},
            }
        }
    }
    metadata
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
}
