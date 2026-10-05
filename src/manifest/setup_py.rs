//! Static `setup.py` manifest extraction.

use std::path::Path;

use crate::path_util::rel_to_root;

use super::error::ManifestError;
use super::literals::parse_module;
use super::requirements::extract_requirements_path;
use super::setup_py_eval::{
    DependencyItem, SetupCall, Value, dependency_items, evaluate_setup_call,
};
use super::types::{DeclaredDependency, DependencyContext, ProjectMetadata};
use super::util::{DependencyPush, push_dependency, read_to_string};
use super::warnings::ManifestWarning;

/// Partial extraction result from `setup.py`.
#[derive(Debug, Default)]
pub struct SetupPyExtraction {
    /// Project metadata when statically available.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Whether static parsing succeeded.
    pub parsed: bool,
    /// `install_requires` could not be fully read (no static `setup()` call,
    /// or a value the evaluator cannot follow).
    pub runtime_unknown: bool,
    /// Root-relative requirements files that `setup.py` reads.
    pub files_read: Vec<String>,
    /// Root-relative requirements file candidates probed but absent.
    pub files_missing: Vec<String>,
}

/// Extract manifest data from `setup.py` without executing Python.
pub fn extract_setup_py(root: &Path, path: &Path) -> Result<SetupPyExtraction, ManifestError> {
    let contents = read_to_string(path)?;
    let rel = rel_to_root(root, path);
    let mut result = SetupPyExtraction::default();

    let call = parse_module(&contents).and_then(|stmts| evaluate_setup_call(root, &stmts));
    let Some(call) = call else {
        result.runtime_unknown = true;
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    };
    result
        .files_missing
        .extend(call.probed_missing.iter().cloned());

    result.metadata.name = string_keyword(&call, "name");
    result.metadata.version = string_keyword(&call, "version");

    let install_requires = call.keyword("install_requires").map(dependency_items);
    let extras_require: Vec<(String, Value)> = match call.keyword("extras_require") {
        Some(Value::Dict(items)) => items.clone(),
        _ => Vec::new(),
    };
    let nothing_read = install_requires
        .as_ref()
        .is_none_or(|items| items.items.is_empty() && !items.complete);
    if nothing_read && extras_require.is_empty() {
        result.runtime_unknown = install_requires.is_some();
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    }

    result.parsed = true;

    if let Some(items) = install_requires {
        result.runtime_unknown =
            !push_items(root, &mut result, &rel, &items, &DependencyContext::Runtime);
    }

    for (extra, value) in extras_require {
        let items = dependency_items(&value);
        let _ = push_items(
            root,
            &mut result,
            &rel,
            &items,
            &DependencyContext::SetupExtra(extra.clone()),
        );
    }

    Ok(result)
}

fn string_keyword(call: &SetupCall, name: &str) -> Option<String> {
    match call.keyword(name) {
        Some(Value::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

/// Push the items as dependencies; `false` when part of them was unreadable.
fn push_items(
    root: &Path,
    result: &mut SetupPyExtraction,
    rel: &str,
    items: &super::setup_py_eval::DependencyItems,
    context: &DependencyContext,
) -> bool {
    let argument = match context {
        DependencyContext::SetupExtra(extra) => format!("extras_require.{extra}"),
        _ => "install_requires".to_owned(),
    };
    let mut complete = items.complete;
    for (index, item) in items.items.iter().enumerate() {
        match item {
            DependencyItem::Requirement(raw) => push_dependency(DependencyPush {
                dependencies: &mut result.dependencies,
                warnings: &mut result.warnings,
                raw,
                context: context.clone(),
                file: rel,
                label: format!("{argument}[{index}]"),
                line: None,
            }),
            DependencyItem::RequirementsFile(path) => {
                let (extracted, read_fully) = extract_requirements_path(root, path, context);
                result.dependencies.extend(extracted.dependencies);
                result.warnings.extend(extracted.warnings);
                result.files_read.extend(extracted.files_read);
                result.files_missing.extend(extracted.files_missing);
                complete &= read_fully;
            },
        }
    }
    if !complete {
        result
            .warnings
            .push(ManifestWarning::SetupPyPartiallyStatic {
                file: rel.to_owned(),
                argument,
            });
    }
    complete
}
#[cfg(test)]
mod tests {
    use super::*;

    fn extract(contents: &str, files: &[(&str, &str)]) -> SetupPyExtraction {
        let temp = tempfile::tempdir().expect("temp dir");
        for (path, body) in files {
            let path = temp.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, body).expect("write");
        }
        let setup = temp.path().join("setup.py");
        std::fs::write(&setup, contents).expect("write setup.py");
        extract_setup_py(temp.path(), &setup).expect("extract")
    }

    fn names_in(result: &SetupPyExtraction, context: &DependencyContext) -> Vec<String> {
        result
            .dependencies
            .iter()
            .filter(|dep| dep.context == *context)
            .map(|dep| dep.name.clone())
            .collect()
    }

    fn extra(name: &str) -> DependencyContext {
        DependencyContext::SetupExtra(name.to_owned())
    }

    #[test]
    fn setup_call_under_main_guard_is_found() {
        let result = extract(
            "import setuptools\nif __name__ == \"__main__\":\n    setuptools.setup(name=\"acme\")\n",
            &[],
        );
        assert_eq!(result.metadata.name.as_deref(), Some("acme"));
    }

    #[test]
    fn extras_require_keeps_string_key_entries() {
        let result = extract(
            "setup(extras_require={\"test\": [\"pytest\"], \"dev\": \"ruff\", **more})\n",
            &[],
        );
        assert_eq!(names_in(&result, &extra("test")), vec!["pytest"]);
        assert_eq!(names_in(&result, &extra("dev")), vec!["ruff"]);
    }

    #[test]
    fn top_level_variables_are_resolved() {
        let result = extract(
            "requires = ['jmespath>=0.7.1', 'python-dateutil']\n\
             extras_require = {'crt': ['awscrt==0.23.8']}\n\
             setup(install_requires=requires, extras_require=extras_require)\n",
            &[],
        );
        assert!(result.parsed);
        assert!(!result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["jmespath", "python-dateutil"]
        );
        assert_eq!(names_in(&result, &extra("crt")), vec!["awscrt"]);
    }

    #[test]
    fn helper_functions_reading_requirements_files_are_followed() {
        let contents = r#"
import os
EXTENSIONS = ('redis', 'yaml')

def _reqs(*f):
    return [r.strip() for r in open(os.path.join(os.getcwd(), 'requirements', *f)).readlines() if r]

def reqs(*f):
    """Parse requirement file."""
    return [req for subreq in _reqs(*f) for req in subreq]

def extras(*p):
    return reqs('extras', *p)

def install_requires():
    return reqs('default.txt')

def extras_require():
    return {x: extras(x + '.txt') for x in EXTENSIONS}

setuptools.setup(install_requires=install_requires(), extras_require=extras_require())
"#;
        let result = extract(
            contents,
            &[
                ("requirements/default.txt", "billiard>=4\nkombu\n"),
                ("requirements/extras/redis.txt", "redis>=4\n"),
            ],
        );
        assert!(!result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["billiard", "kombu"]
        );
        assert_eq!(names_in(&result, &extra("redis")), vec!["redis"]);
        assert!(names_in(&result, &extra("yaml")).is_empty());
        assert!(
            result
                .files_read
                .contains(&"requirements/default.txt".to_owned())
        );
        assert!(
            result
                .files_missing
                .contains(&"requirements/extras/yaml.txt".to_owned())
        );
        assert!(result.warnings.iter().any(|warning| matches!(
            warning,
            ManifestWarning::SetupPyPartiallyStatic { argument, .. } if argument == "extras_require.yaml"
        )));
    }

    #[test]
    fn computed_requirement_table_is_looked_up_by_name() {
        let contents = r#"
import re
_deps = ["numpy>=1.17", "tokenizers>=0.22,<=0.23", "torch>=2.2", "mistral-common[image]>=1.8"]
deps = {b: a for a, b in (re.findall(r"^(([^!=<>~ ]+)(?:[!=<>~ ].*)?$)", x)[0] for x in _deps)}

def deps_list(*pkgs):
    return [deps[pkg] for pkg in pkgs]

extras = {}
extras["torch"] = deps_list("torch")
extras["mistral-common"] = deps_list("mistral-common[image]")
if PYTHON_MINOR_VERSION < 14:
    extras["torch"] += deps_list("numpy")
extras["all"] = extras["torch"] + extras["mistral-common"]
install_requires = [deps["numpy"], deps["tokenizers"]]

if __name__ == "__main__":
    setup(extras_require=extras, install_requires=list(install_requires))
"#;
        let result = extract(contents, &[]);
        assert!(!result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["numpy", "tokenizers"]
        );
        assert_eq!(names_in(&result, &extra("torch")), vec!["torch", "numpy"]);
        assert_eq!(
            names_in(&result, &extra("all")),
            vec!["torch", "numpy", "mistral-common"]
        );
    }

    #[test]
    fn unreadable_install_requires_is_unknown() {
        let result = extract(
            "import pathlib\nsetup(install_requires=pathlib.Path('requirements.txt').read_text().splitlines())\n",
            &[],
        );
        assert!(!result.parsed);
        assert!(result.runtime_unknown);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| matches!(warning, ManifestWarning::SetupPyNotStatic { .. }))
        );
    }

    #[test]
    fn partially_readable_install_requires_keeps_known_entries() {
        let result = extract(
            "setup(install_requires=['requests'] + [r for r in load() if r])\n",
            &[],
        );
        assert!(result.parsed);
        assert!(result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["requests"]
        );
    }

    #[test]
    fn missing_setup_call_is_unknown() {
        let result = extract("from setuptools import setup\nsetup_kwargs = {}\n", &[]);
        assert!(result.runtime_unknown);
        assert!(!result.parsed);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn python_quote(value: &str) -> String {
            let mut quoted = String::with_capacity(value.len() + 2);
            quoted.push('"');
            for ch in value.chars() {
                if ch == '"' || ch == '\\' {
                    quoted.push('\\');
                }
                quoted.push(ch);
            }
            quoted.push('"');
            quoted
        }

        proptest! {
            #[test]
            fn evaluation_never_panics(input in "\\PC{0,300}") {
                if let Some(stmts) = parse_module(&input) {
                    let _ = evaluate_setup_call(Path::new("."), &stmts);
                }
            }

            #[test]
            fn full_setup_call_roundtrips_install_requires(
                deps in prop::collection::vec("[a-z][a-z0-9-]{0,15}", 0..6),
            ) {
                let rendered = deps
                    .iter()
                    .map(|dep| python_quote(dep))
                    .collect::<Vec<_>>()
                    .join(", ");
                let contents = format!(
                    "from setuptools import setup\nsetup(\n    name=\"acme\",\n    install_requires=[{rendered}],\n)\n"
                );
                let stmts = parse_module(&contents).expect("parse");
                let call = evaluate_setup_call(Path::new("."), &stmts).expect("setup call");
                let items = dependency_items(call.keyword("install_requires").expect("keyword"));
                prop_assert!(items.complete);
                let expected: Vec<DependencyItem> =
                    deps.into_iter().map(DependencyItem::Requirement).collect();
                prop_assert_eq!(items.items, expected);
            }
        }
    }
}
