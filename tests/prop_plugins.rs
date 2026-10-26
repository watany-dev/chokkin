//! Property-based tests for the plugins' config readers (#1105): whatever
//! text the files they read hold, extraction returns without a panic and
//! reads the same project the same way twice.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::fs;
use std::path::Path;

use chokkin::internals::{
    PluginExtractRequest, PluginHints, PluginId, ProjectRoot, RootMarker, discover_sources,
    extract_manifest, extract_plugin_hints_with_parse, load_config,
    parse_project_sources_with_cache, resolve_target_version,
};
use proptest::prelude::*;
use tempfile::TempDir;

/// Files a plugin or the config scan reads, by root-relative path.
const FILES: [&str; 26] = [
    "tox.ini",
    "tox.toml",
    "setup.cfg",
    "pytest.ini",
    ".pre-commit-config.yaml",
    "mkdocs.yml",
    "noxfile.py",
    "Makefile",
    "justfile",
    "Procfile",
    ".gitlab-ci.yml",
    "Dockerfile",
    ".flaskenv",
    ".env",
    "alembic.ini",
    "alembic/env.py",
    "scrapy.cfg",
    "airflow.cfg",
    "workspace.yaml",
    "dg.toml",
    "prefect.yaml",
    "app/settings.py",
    "app/__init__.py",
    "scripts/run.sh",
    ".github/workflows/ci.yml",
    "docs/conf.py",
];

/// Pieces of the formats those files use, so random text reaches past the
/// first line of each reader.
const FRAGMENTS: [&str; 64] = [
    "\n",
    "\r\n",
    "  ",
    "\t",
    "    ",
    "[",
    "]",
    "{",
    "}",
    "(",
    ")",
    "\"",
    "'",
    "`",
    "#",
    "=",
    ":",
    ",",
    "-",
    "|",
    ">",
    "\\\n",
    "${",
    "{posargs}",
    "&&",
    ";",
    "$(",
    "é",
    "[testenv]",
    "[tox]",
    "[alembic]",
    "[settings]",
    "[core]",
    "[tool:pytest]",
    "[pytest]",
    "commands =",
    "deps =",
    "addopts = -p ",
    "script_location = ",
    "default = ",
    "dags_folder = ",
    "FLASK_APP=",
    "repos:",
    "- repo: local",
    "hooks:",
    "- id: ",
    "entry: ",
    "plugins:",
    "theme:",
    "load_from:",
    "python_file: ",
    "module_name: ",
    "script:",
    "RUN ",
    "CMD [",
    "web: ",
    "test:\n\t",
    "python -m ",
    "uv run ",
    "scripts/x.py",
    "app:app",
    "app.urls",
    "INSTALLED_APPS = [",
    "@nox.session\ndef s(session):\n    session.install(",
];

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS.to_vec()), 0..24)
        .prop_map(|parts| parts.concat())
}

/// A TOML basic string's body: printable, no `"` or `\`.
fn toml_text() -> impl Strategy<Value = String> {
    "[a-z0-9 ./:_{}$=-]{0,16}"
}

fn pyproject(tools: &[String; 5]) -> String {
    let plugins: String = PluginId::all()
        .iter()
        .flat_map(|plugin| [plugin.as_key(), " = true\n"])
        .collect();
    let [pdm, poe, hatch, addopts, dagster] = tools;
    format!(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         dependencies = [\"django\", \"flask\", \"apache-airflow\", \"dagster\", \"scrapy\"]\n\n\
         [tool.chokkin.plugins]\n{plugins}\n\
         [tool.pdm.scripts]\nx = \"{pdm}\"\n\n\
         [tool.poe.tasks]\ny = \"{poe}\"\n\n\
         [tool.hatch.envs.default.scripts]\nz = \"{hatch}\"\n\n\
         [tool.pytest.ini_options]\naddopts = \"{addopts}\"\n\n\
         [tool.dagster]\nmodule_name = \"{dagster}\"\n"
    )
}

fn extract(root: &ProjectRoot) -> PluginHints {
    let config = load_config(root).expect("load config");
    let manifest = extract_manifest(root, &config).expect("extract manifest");
    let sources = discover_sources(root, &config, &manifest).expect("discover sources");
    let target = resolve_target_version(&config.effective, &manifest);
    let parse =
        parse_project_sources_with_cache(root, &sources, &target, None).expect("parse sources");
    extract_plugin_hints_with_parse(&PluginExtractRequest {
        root,
        config: &config,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
        dockerfiles: &[],
    })
    .expect("extract plugin hints")
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, contents).expect("write");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(48)))]

    #[test]
    fn plugin_config_readers_are_total_and_deterministic(
        files in prop::collection::vec(
            (prop::sample::select(FILES.to_vec()), text()),
            1..8,
        ),
        tools in prop::array::uniform5(toml_text()),
    ) {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "pyproject.toml", &pyproject(&tools));
        for (rel, contents) in &files {
            write(dir.path(), rel, contents);
        }
        let root = ProjectRoot {
            path: fs::canonicalize(dir.path()).expect("canon"),
            marker: RootMarker::PyProjectToml,
        };
        let first = extract(&root);
        prop_assert_eq!(extract(&root), first);
    }
}
