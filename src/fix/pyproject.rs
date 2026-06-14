//! `pyproject.toml` edits via `toml_edit`.

use toml_edit::{DocumentMut, Item, Value};

use super::error::FixError;

/// Remove a dependency entry identified by a manifest label.
pub fn remove_by_label(path: &std::path::Path, label: &str) -> Result<String, FixError> {
    let rel = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pyproject.toml");
    let contents = std::fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|error| FixError::InvalidToml {
            path: rel.to_owned(),
            detail: error.to_string(),
        })?;

    let removed = remove_label_in_document(&mut doc, label)?;
    if !removed {
        return Err(FixError::Unsupported {
            detail: format!("could not find `{label}` in {rel}"),
        });
    }

    std::fs::write(path, doc.to_string()).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    Ok(format!("removed `{label}` from {rel}"))
}

/// Move a dependency from a dev group into `[project].dependencies`.
pub fn move_group_to_runtime(
    path: &std::path::Path,
    from_label: &str,
    raw: &str,
) -> Result<String, FixError> {
    let rel = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pyproject.toml");
    let contents = std::fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|error| FixError::InvalidToml {
            path: rel.to_owned(),
            detail: error.to_string(),
        })?;

    let removed = remove_label_in_document(&mut doc, from_label)?;
    if !removed {
        return Err(FixError::Unsupported {
            detail: format!("could not remove source entry `{from_label}`"),
        });
    }

    let project = doc
        .entry("project")
        .or_insert(Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| FixError::Unsupported {
            detail: "[project] is not a table".to_owned(),
        })?;
    let deps = project
        .entry("dependencies")
        .or_insert(Item::Value(Value::Array(toml_edit::Array::new())))
        .as_array_mut()
        .ok_or_else(|| FixError::Unsupported {
            detail: "project.dependencies is not an array".to_owned(),
        })?;
    deps.push(raw);

    std::fs::write(path, doc.to_string()).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    Ok(format!("moved dependency to project.dependencies in {rel}"))
}

fn remove_label_in_document(doc: &mut DocumentMut, label: &str) -> Result<bool, FixError> {
    if let Some(index) = parse_indexed_label(label, "project.dependencies") {
        return remove_array_index(doc, &["project", "dependencies"], index);
    }
    if let Some((extra, index)) = parse_group_label(label, "project.optional-dependencies.") {
        return remove_array_index(
            doc,
            &["project", "optional-dependencies", extra.as_str()],
            index,
        );
    }
    if let Some((group, index)) = parse_group_label(label, "dependency-groups.") {
        return remove_array_index(doc, &["dependency-groups", group.as_str()], index);
    }
    Err(FixError::Unsupported {
        detail: format!("unsupported pyproject label `{label}`"),
    })
}

fn parse_indexed_label(label: &str, prefix: &str) -> Option<usize> {
    let rest = label.strip_prefix(prefix)?;
    let index = rest.strip_prefix('[')?.strip_suffix(']')?;
    index.parse().ok()
}

fn parse_group_label(label: &str, prefix: &str) -> Option<(String, usize)> {
    let rest = label.strip_prefix(prefix)?;
    let (group, index_part) = rest.split_once('[')?;
    let index = index_part.strip_suffix(']')?.parse().ok()?;
    Some((group.to_owned(), index))
}

fn remove_array_index(
    doc: &mut DocumentMut,
    path: &[&str],
    index: usize,
) -> Result<bool, FixError> {
    let mut current = doc.as_item_mut();
    for segment in path {
        current = current
            .get_mut(*segment)
            .ok_or_else(|| FixError::Unsupported {
                detail: format!("missing TOML path `{}`", path.join(".")),
            })?;
    }
    let array = current
        .as_array_mut()
        .ok_or_else(|| FixError::Unsupported {
            detail: format!("`{}` is not an array", path.join(".")),
        })?;
    if index >= array.len() {
        return Ok(false);
    }
    array.remove(index);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn removes_project_dependency() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "demo"
dependencies = ["boto3>=1.0", "requests>=2.0"]
"#,
        )
        .expect("write");

        remove_by_label(&path, "project.dependencies[0]").expect("remove");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(!updated.contains("boto3"));
        assert!(updated.contains("requests"));
    }
}
