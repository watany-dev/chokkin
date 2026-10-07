//! `uv.lock` graph extraction.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::error::ManifestError;
use super::pep508_util::normalize_distribution_name;
use super::types::LockfileGraph;

/// Only the fields the graph needs: skipping `sdist`/`wheels` without building
/// values keeps a monorepo's hundreds of member lockfiles cheap to read (#488).
#[derive(Deserialize)]
struct UvLock {
    #[serde(default)]
    package: Vec<UvPackage>,
}

#[derive(Deserialize)]
struct UvPackage {
    name: Option<String>,
    #[serde(default)]
    dependencies: Vec<UvDependency>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: BTreeMap<String, Vec<UvDependency>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UvDependency {
    Table { name: String },
    Name(String),
    Other(IgnoredAny),
}

/// Parse `uv.lock` into a dependency name graph.
pub(super) fn extract_uv_lock(path: &Path) -> Result<LockfileGraph, ManifestError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(lock) = scan_uv_layout(&contents) {
        return Ok(lock_graph(lock));
    }

    // A filtered lock that no longer parses means the strip guessed wrong, so
    // the full text decides whether the file is really invalid.
    let stripped = strip_artifacts(&contents);
    let lock: UvLock = stripped
        .as_deref()
        .map_or_else(|| toml::from_str(&contents), toml::from_str)
        .or_else(|_| toml::from_str(&contents))
        .map_err(|error| ManifestError::InvalidUvLock {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    Ok(lock_graph(lock))
}

fn lock_graph(lock: UvLock) -> LockfileGraph {
    let mut graph = LockfileGraph::default();
    for package in lock.package {
        let Some(name) = package.name.as_deref().map(normalize_distribution_name) else {
            continue;
        };
        if !package.optional_dependencies.is_empty() {
            let extras = package
                .optional_dependencies
                .iter()
                .map(|(extra, deps)| (normalize_distribution_name(extra), dependency_names(deps)))
                .collect();
            graph.extras.insert(name.clone(), extras);
        }
        graph
            .edges
            .insert(name, dependency_names(&package.dependencies));
    }
    graph
}

/// Where the items of a multi-line array go.
enum ArrayTarget {
    Dependencies,
    Extra(String),
}

/// Which table the scanner is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Table {
    Package,
    OptionalDependencies,
    Other,
}

/// Read a lock in the exact layout uv writes without a TOML parser: even
/// stripped of artifacts, parsing 600 member lockfiles took a third of an
/// undeclared monorepo's member probe (#592). `None` for any line uv would
/// not write, such as a comment or an inline `dependencies` array, so `toml`
/// decides those files.
fn scan_uv_layout(contents: &str) -> Option<UvLock> {
    if !single_line_strings(contents) {
        return None;
    }
    let mut packages: Vec<UvPackage> = Vec::new();
    let mut table = Table::Other;
    let mut array: Option<ArrayTarget> = None;
    let mut keys: Vec<&str> = Vec::new();
    let mut top_tables: Vec<&str> = Vec::new();
    let mut package_tables: Vec<&str> = Vec::new();
    let mut rest = contents;
    while !rest.is_empty() {
        let (line, next) = rest.split_once('\n').unwrap_or((rest, ""));
        rest = next;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(target) = &array {
            if line == "]" {
                array = None;
                continue;
            }
            let item = line.strip_prefix("    ")?.strip_suffix(',')?;
            let dependency = scan_array_item(item)?;
            let package = packages.last_mut()?;
            match target {
                ArrayTarget::Dependencies => &mut package.dependencies,
                ArrayTarget::Extra(extra) => package.optional_dependencies.get_mut(extra)?,
            }
            .push(dependency);
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if line == "[[package]]" {
            packages.push(UvPackage {
                name: None,
                dependencies: Vec::new(),
                optional_dependencies: BTreeMap::new(),
            });
            table = Table::Package;
            keys.clear();
            package_tables.clear();
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            if !is_bare_key(header, true) {
                return None;
            }
            keys.clear();
            table = if let Some(sub) = header.strip_prefix("package.") {
                (!packages.is_empty()).then_some(())?;
                package_table(sub, &mut package_tables)?
            } else {
                top_table(header, &mut top_tables)?
            };
            continue;
        }

        let (key, value) = line.split_once(" = ")?;
        // A root-level `package` key would be the package array itself.
        let in_root = packages.is_empty() && top_tables.is_empty();
        if !is_bare_key(key, false) || keys.contains(&key) || (in_root && key == "package") {
            return None;
        }
        keys.push(key);
        if !is_graph_key(table, key)? {
            if value == "[" {
                // `wheels` arrays are most of a lock's bytes.
                rest = skip_array(rest)?;
            } else if !is_plain_value(value) {
                return None;
            }
            continue;
        }
        graph_value(packages.last_mut()?, table, key, value, &mut array)?;
    }
    array.is_none().then_some(UvLock { package: packages })
}

/// Whether the key feeds the graph; `None` for an inline extras table, since
/// uv writes extras as a `[package.optional-dependencies]` table.
fn is_graph_key(table: Table, key: &str) -> Option<bool> {
    match table {
        Table::Package if key == "optional-dependencies" => None,
        Table::Package => Some(matches!(key, "name" | "dependencies")),
        Table::OptionalDependencies => Some(true),
        Table::Other => Some(false),
    }
}

/// Without escapes or multi-line strings every string closes on its own line,
/// so a line's shape tells whether it ends an array.
fn single_line_strings(contents: &str) -> bool {
    let bytes = contents.as_bytes();
    memchr::memchr(b'\\', bytes).is_none()
        && memchr::memmem::find(bytes, b"\"\"\"").is_none()
        && memchr::memmem::find(bytes, b"'''").is_none()
}

/// A top-level table header, `None` for a repeat or a `package` table.
fn top_table<'a>(header: &'a str, seen: &mut Vec<&'a str>) -> Option<Table> {
    if header.split('.').next() == Some("package") || seen.contains(&header) {
        return None;
    }
    seen.push(header);
    Some(Table::Other)
}

/// The table a `[package.<sub>]` header opens, `None` when uv would not
/// write it: a repeat, or a graph field other than the extras table.
fn package_table<'a>(sub: &'a str, seen: &mut Vec<&'a str>) -> Option<Table> {
    let first = sub.split('.').next().unwrap_or(sub);
    let graph_field = matches!(first, "name" | "dependencies" | "optional-dependencies");
    if seen.contains(&sub) || (graph_field && sub != "optional-dependencies") {
        return None;
    }
    seen.push(sub);
    Some(if sub == "optional-dependencies" {
        Table::OptionalDependencies
    } else {
        Table::Other
    })
}

/// Record a graph key's one-line value, or the array its `[` line opens.
fn graph_value(
    package: &mut UvPackage,
    table: Table,
    key: &str,
    value: &str,
    array: &mut Option<ArrayTarget>,
) -> Option<()> {
    let target = match (table, key) {
        (Table::Package, "name") => {
            package.name = Some(quoted(value)?.to_owned());
            return Some(());
        },
        (Table::Package, _) => ArrayTarget::Dependencies,
        _ => {
            package
                .optional_dependencies
                .insert(key.to_owned(), Vec::new());
            ArrayTarget::Extra(key.to_owned())
        },
    };
    match value {
        "[" => *array = Some(target),
        "[]" => {},
        _ => return None,
    }
    Some(())
}

/// One `    { name = "idna", marker = "..." },` line, comma and indent removed.
fn scan_array_item(item: &str) -> Option<UvDependency> {
    if let Some(name) = quoted(item) {
        return Some(UvDependency::Name(name.to_owned()));
    }
    let inner = item.strip_prefix("{ ")?.strip_suffix(" }")?;
    if let Some(rest) = inner.strip_prefix("name = \"") {
        let (name, rest) = rest.split_once('"')?;
        if !(rest.is_empty() || rest.starts_with(", ")) {
            return None;
        }
        return Some(UvDependency::Table {
            name: name.to_owned(),
        });
    }
    // A `name` key after another key would still make this a named table.
    (!inner.contains("name =")).then_some(UvDependency::Other(IgnoredAny))
}

/// The text after the `]` line closing a multi-line array. `None` unless every
/// item is one `    {...},` or `    "...",` line, since an array closed on its
/// last item would otherwise run on to a later array's `]`.
fn skip_array(mut rest: &str) -> Option<&str> {
    loop {
        let end = memchr::memchr(b'\n', rest.as_bytes()).unwrap_or(rest.len());
        let line = &rest[..end];
        let line = line.strip_suffix('\r').unwrap_or(line);
        rest = rest.get(end + 1..).unwrap_or("");
        if line == "]" {
            return Some(rest);
        }
        let item = line.strip_prefix("    ")?.strip_suffix(',')?;
        let closed = (item.starts_with('{') && item.ends_with('}'))
            || (item.starts_with('"') && item.ends_with('"'));
        if !closed {
            return None;
        }
    }
}

/// The contents of a basic string uv writes: no inner quotes.
fn quoted(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('"')).then_some(inner)
}

/// A one-line value of a key the graph ignores.
fn is_plain_value(value: &str) -> bool {
    quoted(value).is_some()
        || (value.starts_with('{') && value.ends_with('}'))
        || (value.starts_with('[') && value.ends_with(']'))
        || matches!(value, "true" | "false")
        || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_bare_key(key: &str, dotted: bool) -> bool {
    !key.is_empty()
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        && (dotted || !key.contains('.'))
}

fn dependency_names(deps: &[UvDependency]) -> Vec<String> {
    deps.iter()
        .filter_map(|dep| match dep {
            UvDependency::Table { name } | UvDependency::Name(name) => {
                Some(normalize_distribution_name(name))
            },
            UvDependency::Other(_) => None,
        })
        .collect()
}

/// Drop the `sdist` and `wheels` lines uv writes for every package: they are
/// ~95% of a lock's bytes and TOML parsing them dominated the probe of a
/// monorepo with hundreds of member lockfiles (#513). `None` when a `wheels`
/// block is not uv's one-table-per-line layout, since skipping to the next
/// `]` line could then drop graph lines and still parse.
fn strip_artifacts(contents: &str) -> Option<String> {
    let mut kept = String::with_capacity(contents.len() / 16);
    let mut in_wheels = false;
    for line in contents.lines() {
        if in_wheels {
            in_wheels = line != "]";
            let item = line.trim();
            if in_wheels && !(item.starts_with('{') && item.ends_with("},")) {
                return None;
            }
        } else if line == "wheels = [" {
            in_wheels = true;
        } else if !line.starts_with("sdist = ") && !line.starts_with("wheels = [{") {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    (!in_wheels).then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> Result<LockfileGraph, ManifestError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("uv.lock");
        std::fs::write(&path, contents).expect("write uv.lock");
        extract_uv_lock(&path)
    }

    #[test]
    fn extracts_package_edges() {
        let graph = parse(
            "requires-python = \">=3.11\"\n\n\
             [[package]]\nname = \"Acme_Lib\"\n\
             dependencies = [{ name = \"requests\" }, \"PyYAML\"]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(
            graph.edges.get("acme-lib"),
            Some(&vec!["requests".to_owned(), "pyyaml".to_owned()])
        );
    }

    #[test]
    fn extracts_optional_dependencies_per_extra() {
        let graph = parse(
            "[[package]]\nname = \"psycopg\"\n\
             dependencies = [{ name = \"typing-extensions\" }]\n\n\
             [package.optional-dependencies]\n\
             pool = [{ name = \"Psycopg_Pool\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(
            graph
                .extras
                .get("psycopg")
                .and_then(|extras| extras.get("pool")),
            Some(&vec!["psycopg-pool".to_owned()])
        );
        assert_eq!(
            graph.edges.get("psycopg"),
            Some(&vec!["typing-extensions".to_owned()])
        );
    }

    #[test]
    fn skips_sdist_and_wheels() {
        let graph = parse(
            "[[package]]\nname = \"acme\"\n\
             sdist = { url = \"https://x/acme.tar.gz\", hash = \"sha256:00\" }\n\
             wheels = [\n    { url = \"https://x/acme.whl\", hash = \"sha256:00\" },\n]\n\
             dependencies = [{ name = \"idna\" }]\n\n\
             [[package]]\nname = \"idna\"\n\
             wheels = [{ url = \"https://x/idna.whl\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(graph.edges.get("acme"), Some(&vec!["idna".to_owned()]));
        assert_eq!(graph.edges.get("idna"), Some(&Vec::new()));
    }

    // The full-text fallback hides a wrong strip from `parse`, so check the
    // filtered text itself.
    #[test]
    fn strip_artifacts_keeps_only_graph_lines() {
        let stripped = strip_artifacts(
            "[[package]]\nname = \"acme\"\n\
             sdist = { url = \"https://x/acme.tar.gz\" }\n\
             wheels = [\n    { url = \"https://x/acme.whl\" },\n]\n\
             dependencies = [{ name = \"idna\" }]\n\
             wheels = [{ url = \"https://x/idna.whl\" }]\n",
        );

        assert_eq!(
            stripped.as_deref(),
            Some("[[package]]\nname = \"acme\"\ndependencies = [{ name = \"idna\" }]\n")
        );
        assert_eq!(
            strip_artifacts("wheels = [\n    { url = \"a\" }]\nx = 1\n]\n"),
            None
        );
        assert_eq!(strip_artifacts("wheels = [\n    { url = \"a\" },\n"), None);
        assert_eq!(strip_artifacts("wheels = [\n    { url = \"a\",\n]\n"), None);
        assert_eq!(strip_artifacts("wheels = [\n    url = \"a\" },\n]\n"), None);
    }

    #[test]
    fn wheels_array_closed_on_its_last_item_does_not_hide_later_packages() {
        let graph = parse(
            "[[package]]\nname = \"acme\"\n\
             dependencies = [{ name = \"idna\" }]\n\
             wheels = [\n    { url = \"https://x/acme.whl\" }]\n\n\
             [[package]]\nname = \"idna\"\n\
             dependencies = [{ name = \"six\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(graph.edges.get("acme"), Some(&vec!["idna".to_owned()]));
        assert_eq!(graph.edges.get("idna"), Some(&vec!["six".to_owned()]));
    }

    /// A lock in the layout uv writes, which the scanner reads on its own.
    const UV_LAYOUT: &str = "version = 1\n\
        revision = 2\n\
        requires-python = \">=3.10\"\n\
        resolution-markers = [\n    \"python_full_version >= '3.12'\",\n    \"python_full_version < '3.12'\",\n]\n\n\
        [options]\nexclude-newer = \"2025-01-01T00:00:00Z\"\n\n\
        [manifest]\nmembers = [\n    \"acme\",\n    \"acme-core\",\n]\n\n\
        [[package]]\nname = \"Acme\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n\
        dependencies = [\n    { name = \"acme-core\" },\n    { name = \"idna\", marker = \"sys_platform == 'linux'\" },\n]\n\n\
        [package.optional-dependencies]\nPool = [\n    { name = \"Psycopg_Pool\" },\n]\nempty = []\n\n\
        [package.metadata]\nrequires-dist = [\n    { name = \"idna\" },\n]\n\n\
        [[package]]\nname = \"acme-core\"\nsource = { editable = \"core\" }\ndependencies = []\n\n\
        [[package]]\nname = \"idna\"\nversion = \"3.7\"\n\
        source = { registry = \"https://pypi.org/simple\" }\n\
        sdist = { url = \"https://x/idna.tar.gz\", hash = \"sha256:00\", size = 1 }\n\
        wheels = [\n    { url = \"https://x/idna.whl\", hash = \"sha256:00\", size = 1 },\n]\n";

    fn toml_graph(contents: &str) -> LockfileGraph {
        lock_graph(toml::from_str(contents).expect("valid TOML"))
    }

    #[test]
    fn scanner_reads_uv_layout_like_toml() {
        let scanned = scan_uv_layout(UV_LAYOUT).map(lock_graph);

        assert_eq!(scanned.as_ref(), Some(&toml_graph(UV_LAYOUT)));
        let graph = scanned.expect("uv layout");
        assert_eq!(
            graph.edges.get("acme"),
            Some(&vec!["acme-core".to_owned(), "idna".to_owned()])
        );
        assert_eq!(
            graph
                .extras
                .get("acme")
                .and_then(|extras| extras.get("pool")),
            Some(&vec!["psycopg-pool".to_owned()])
        );
        assert_eq!(
            scan_uv_layout(&UV_LAYOUT.replace('\n', "\r\n")).map(lock_graph),
            Some(graph)
        );
    }

    #[test]
    fn scanner_leaves_layouts_uv_does_not_write_to_toml() {
        for contents in [
            "# comment\n[[package]]\nname = \"a\"\n",
            "[[package]]\nname = \"a\" # comment\n",
            "[[package]]\nname = \"a\"\ndependencies = [{ name = \"b\" }]\n",
            "[[package]]\nname = \"a\"\noptional-dependencies = { x = [] }\n",
            "[[package]]\nname = \"a\"\ndependencies = [\n    { name = \"b\" },\n",
            "[[package]]\nname = 'a'\n",
            "[[package]]\nname = \"a\"\nname = \"b\"\n",
            "[[package]]\nname = \"a\"\nwheels = [\n    { url = \"a\" }]\nx = [\n]\n",
            "[[package]]\nname = \"a\"\nwheels = [\n    { url = \"\"\"a\" },\n]\n",
            "[package.metadata]\nx = 1\n",
            "package = []\n",
        ] {
            assert!(scan_uv_layout(contents).is_none(), "{contents}");
        }
    }

    #[test]
    fn rejects_invalid_toml() {
        let error = parse("[[package\n").expect_err("invalid TOML");
        assert!(matches!(error, ManifestError::InvalidUvLock { .. }));
    }

    mod props {
        use std::fmt::Write as _;

        use super::*;
        use proptest::prelude::*;

        /// `wheels` arrays as uv writes them and as hand edits may leave them.
        const WHEELS: [&str; 7] = [
            "",
            "wheels = [\n    { url = \"https://x/a.whl\", hash = \"sha256:00\" },\n]\n",
            "wheels = [{ url = \"https://x/a.whl\" }]\n",
            "wheels = [\n    { url = \"https://x/a.whl\" }]\n",
            "wheels = [\n    { url = \"https://x/a.whl\" },\n    { url = \"https://x/b.whl\" }\n]\n",
            "wheels = [\n    { url = \"https://x/a.whl\",\n      hash = \"sha256:00\" },\n]\n",
            "wheels = [\n  # comment\n  { url = \"https://x/a.whl\" },\n]\n",
        ];

        fn package_name() -> impl Strategy<Value = String> {
            "[A-Za-z0-9]([A-Za-z0-9._-]{0,12}[A-Za-z0-9])?"
        }

        proptest! {
            #[test]
            fn extract_uv_lock_never_panics(contents in "\\PC{0,400}") {
                let _ = parse(&contents);
            }

            #[test]
            fn extract_uv_lock_roundtrips_generated_graph(
                packages in prop::collection::btree_map(
                    package_name(),
                    prop::collection::vec(package_name(), 0..4),
                    0..5,
                ),
            ) {
                let mut contents = String::new();
                for (name, deps) in &packages {
                    writeln!(contents, "\n[[package]]\nname = \"{name}\"").expect("write");
                    let rendered = deps
                        .iter()
                        .map(|dep| format!("{{ name = \"{dep}\" }}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(contents, "dependencies = [{rendered}]").expect("write");
                }

                let graph = parse(&contents).expect("generated uv.lock is valid TOML");

                // Distinct raw names may normalize to the same key, so compare
                // against a reference map built with the same normalization.
                let mut expected = std::collections::BTreeMap::new();
                for (name, deps) in &packages {
                    expected.insert(
                        normalize_distribution_name(name),
                        deps.iter()
                            .map(|dep| normalize_distribution_name(dep))
                            .collect::<Vec<_>>(),
                    );
                }
                prop_assert_eq!(graph.edges.len(), expected.len());
                for (name, deps) in &expected {
                    prop_assert_eq!(graph.edges.get(name), Some(deps));
                }
            }

            #[test]
            fn artifact_layout_does_not_change_the_graph(
                packages in prop::collection::btree_map(
                    package_name(),
                    (prop::collection::vec(package_name(), 0..3), 0usize..WHEELS.len(), any::<bool>()),
                    1..5,
                ),
            ) {
                let mut contents = String::from("version = 1\n");
                let mut expected = std::collections::BTreeMap::new();
                for (name, (deps, wheels, sdist)) in &packages {
                    writeln!(contents, "\n[[package]]\nname = \"{name}\"").expect("write");
                    if *sdist {
                        contents.push_str("sdist = { url = \"https://x/a.tar.gz\", hash = \"sha256:00\" }\n");
                    }
                    contents.push_str(WHEELS[*wheels]);
                    let rendered = deps
                        .iter()
                        .map(|dep| format!("{{ name = \"{dep}\" }}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(contents, "dependencies = [{rendered}]").expect("write");
                    expected.insert(
                        normalize_distribution_name(name),
                        deps.iter().map(|dep| normalize_distribution_name(dep)).collect::<Vec<_>>(),
                    );
                }

                let graph = parse(&contents).expect("generated uv.lock is valid TOML");
                prop_assert_eq!(graph.edges, expected, "{}", contents);
            }

            #[test]
            fn scanner_matches_toml_on_uv_layout(
                packages in prop::collection::btree_map(
                    package_name(),
                    (
                        prop::collection::vec(package_name(), 0..3),
                        prop::collection::btree_map(package_name(), prop::collection::vec(package_name(), 0..3), 0..3),
                        any::<bool>(),
                    ),
                    1..5,
                ),
            ) {
                let mut contents = String::from("version = 1\nrequires-python = \">=3.10\"\n");
                for (name, (deps, extras, wheels)) in &packages {
                    writeln!(contents, "\n[[package]]\nname = \"{name}\"\nsource = {{ editable = \".\" }}").expect("write");
                    if deps.is_empty() {
                        contents.push_str("dependencies = []\n");
                    } else {
                        contents.push_str("dependencies = [\n");
                        for dep in deps {
                            writeln!(contents, "    {{ name = \"{dep}\" }},").expect("write");
                        }
                        contents.push_str("]\n");
                    }
                    if *wheels {
                        contents.push_str(WHEELS[1]);
                    }
                    if !extras.is_empty() {
                        contents.push_str("\n[package.optional-dependencies]\n");
                        for (extra, deps) in extras {
                            writeln!(contents, "{extra} = [").expect("write");
                            for dep in deps {
                                writeln!(contents, "    {{ name = \"{dep}\", marker = \"x\" }},").expect("write");
                            }
                            contents.push_str("]\n");
                        }
                    }
                }

                // A dotted generated extra is a valid TOML key the scanner refuses.
                if let Some(lock) = scan_uv_layout(&contents) {
                    prop_assert_eq!(lock_graph(lock), toml_graph(&contents), "{}", contents);
                } else {
                    prop_assert!(packages.values().any(|(_, extras, _)| extras.keys().any(|extra| extra.contains('.'))), "{}", contents);
                }
            }

            #[test]
            fn all_edge_names_are_normalized(contents in "\\PC{0,400}") {
                if let Ok(graph) = parse(&contents) {
                    for (name, deps) in &graph.edges {
                        let renormalized = normalize_distribution_name(name);
                        prop_assert_eq!(&renormalized, name);
                        for dep in deps {
                            let dep_renormalized = normalize_distribution_name(dep);
                            prop_assert_eq!(&dep_renormalized, dep);
                        }
                    }
                }
            }
        }
    }
}
