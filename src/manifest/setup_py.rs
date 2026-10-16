//! Static `setup.py` manifest extraction.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use crate::path_util::rel_to_root;

use super::error::ManifestError;
use super::literals::parse_module;
use super::requirements::{extract_requirements_path, requirements_line_distribution};
use super::setup_py_eval::{
    DependencyItem, SetupCall, Value, dependency_items, evaluate_setup_call,
};
use super::types::{DeclaredDependency, DependencyContext, ExtrasState, ProjectMetadata};
use super::util::{DependencyPush, push_dependency, read_text};
use super::warnings::ManifestWarning;

/// Partial extraction result from `setup.py`.
#[derive(Debug, Default)]
pub(super) struct SetupPyExtraction {
    /// Project metadata when statically available.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Whether static parsing succeeded.
    pub parsed: bool,
    /// `setup()` passes a `name` that cannot be read statically.
    pub dynamic_name: bool,
    /// `install_requires` could not be fully read (no static `setup()` call,
    /// or a value the evaluator cannot follow).
    pub runtime_unknown: bool,
    /// How much of `extras_require` could be read; `None` when `setup()`
    /// passes none, not even through `**kwargs`.
    pub extras: Option<ExtrasState>,
    /// Root-relative requirements files that `setup.py` reads.
    pub files_read: Vec<String>,
    /// Root-relative requirements file candidates probed but absent.
    pub files_missing: Vec<String>,
    /// Distributions a requirement table lists (transformers `_deps`): a
    /// version pin table, not a declaration, so they only name packages.
    pub table_names: Vec<String>,
}

/// Extract manifest data from `setup.py` without executing Python.
pub(super) fn extract_setup_py(
    root: &Path,
    path: &Path,
) -> Result<SetupPyExtraction, ManifestError> {
    let rel = rel_to_root(root, path);
    let mut result = SetupPyExtraction::default();
    let Some(contents) = read_text(path)? else {
        return Ok(unreadable(ManifestWarning::FileUndecodable { file: rel }));
    };

    let call = parse_module(&contents).and_then(|stmts| evaluate_setup_call(root, &stmts));
    let Some(call) = call else {
        return Ok(unreadable(ManifestWarning::SetupPyNotStatic { file: rel }));
    };
    result
        .files_missing
        .extend(call.probed_missing.iter().cloned());
    result.table_names = call
        .table_requirements
        .iter()
        .filter_map(|raw| requirements_line_distribution(raw))
        .collect();

    result.metadata.name = string_keyword(&call, "name");
    result.dynamic_name = result.metadata.name.is_none() && call.keyword("name").is_some();
    result.metadata.version = string_keyword(&call, "version");
    result.metadata.requires_python = string_keyword(&call, "python_requires");

    let install_requires = call.keyword("install_requires").map(dependency_items);
    // `extras_require` the evaluator could not read, entries of it the
    // evaluator could not key (#869), or one `**kwargs` may pass (a keyword
    // passed twice is a `TypeError`, so an explicit one wins).
    let (extras_require, extras_hidden): (&[(String, Value)], bool) =
        match call.keyword("extras_require") {
            Some(Value::Dict { items, partial, .. }) => (items, *partial),
            Some(_) => (&[], true),
            None => (&[], call.unpacked),
        };
    result.extras = if extras_hidden {
        Some(ExtrasState::Unknown)
    } else {
        call.keyword("extras_require").map(|_| ExtrasState::Known)
    };
    let nothing_read = install_requires
        .as_ref()
        .is_none_or(|items| items.items.is_empty() && !items.complete);
    if nothing_read && extras_require.is_empty() {
        result.runtime_unknown = install_requires.is_some() || call.unpacked;
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    }

    result.parsed = true;
    if extras_hidden {
        result
            .warnings
            .push(ManifestWarning::SetupPyPartiallyStatic {
                file: rel.clone(),
                argument: "extras_require".to_owned(),
            });
    }

    result.runtime_unknown = match install_requires {
        Some(items) => !push_items(root, &mut result, &rel, &items, &DependencyContext::Runtime),
        None => call.unpacked,
    };

    let mut lost = BTreeSet::new();
    for (extra, value) in extras_require {
        if !push_items(
            root,
            &mut result,
            &rel,
            &dependency_items(value),
            &DependencyContext::SetupExtra(extra.clone()),
        ) {
            lost.insert(extra.clone());
        }
    }
    // Extras we cannot name dominate the ones we named but could not read.
    if !lost.is_empty() && !extras_hidden {
        result.extras = Some(ExtrasState::Partial(lost));
    }

    Ok(result)
}

/// Neither dependencies nor extras could be read.
fn unreadable(warning: ManifestWarning) -> SetupPyExtraction {
    SetupPyExtraction {
        runtime_unknown: true,
        extras: Some(ExtrasState::Unknown),
        warnings: vec![warning],
        ..SetupPyExtraction::default()
    }
}

fn string_keyword(call: &SetupCall, name: &str) -> Option<String> {
    match call.keyword(name) {
        Some(Value::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

/// Push the items as dependencies; `false` when part of them was unreadable.
///
/// A requirement string repeated in one evaluated list is pushed once: lists
/// built by concatenation (`extras["dev"] = extras["all"] + extras["testing"]`)
/// repeat the same literal, and setup.py items carry no line to fix (#726).
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
    let mut seen = HashSet::new();
    for (index, item) in items.items.iter().enumerate() {
        match item {
            DependencyItem::Requirement(raw) if !seen.insert(raw.as_str()) => {},
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
                let (extracted, outcome) = extract_requirements_path(root, path, context);
                complete &= outcome.is_ok() && !extracted.skipped;
                result.dependencies.extend(extracted.dependencies);
                result.warnings.extend(extracted.warnings);
                result.files_read.extend(extracted.files_read);
                result.files_missing.extend(extracted.files_missing);
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
    fn method_calls_on_dict_entries_mark_the_entry_partial() {
        let result = extract(
            "extras = {'dev': ['ruff'], 'test': ['pytest']}\n\
             extras['dev'].append(load())\n\
             setup(extras_require=extras)\n",
            &[],
        );
        assert_eq!(names_in(&result, &extra("dev")), vec!["ruff"]);
        assert!(result.warnings.iter().any(|warning| matches!(
            warning,
            ManifestWarning::SetupPyPartiallyStatic { argument, .. }
                if argument == "extras_require.dev"
        )));
        assert!(!result.warnings.iter().any(|warning| matches!(
            warning,
            ManifestWarning::SetupPyPartiallyStatic { argument, .. }
                if argument == "extras_require.test"
        )));
    }

    #[test]
    fn deleted_dict_entries_are_not_reported() {
        let result = extract(
            "extras = {'dev': ['ruff'], 'docs': ['sphinx']}\n\
             del extras['docs']\n\
             setup(extras_require=extras)\n",
            &[],
        );
        assert_eq!(names_in(&result, &extra("dev")), vec!["ruff"]);
        assert_eq!(names_in(&result, &extra("docs")), Vec::<String>::new());
    }

    #[test]
    fn repeated_dict_keys_keep_the_last_value_like_python() {
        let result = extract(
            "base = {'dev': ['old-base']}\n\
             extras = {**base, 'dev': ['ruff'], 'test': ['old'], 'test': ['pytest']}\n\
             table = {k: [k + '-v1'] for k in ['a', 'a']}\n\
             setup(install_requires=extras['dev'] + table['a'], extras_require=extras)\n",
            &[],
        );
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["ruff", "a-v1"]
        );
        assert_eq!(names_in(&result, &extra("dev")), vec!["ruff"]);
        assert_eq!(names_in(&result, &extra("test")), vec!["pytest"]);
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
        assert_eq!(names_in(&result, &extra("yaml")), Vec::<String>::new());
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

    /// A requirements file that cannot be fully read leaves the argument
    /// partly unknown even when the read itself returned.
    #[test]
    fn unreadable_requirements_files_leave_the_argument_partial() {
        let contents = r"
import os

def reqs(f):
    return [r.strip() for r in open(os.path.join(os.getcwd(), 'requirements', f)).readlines() if r]

setuptools.setup(install_requires=reqs('default.txt'), extras_require={'bin': reqs('bin.txt')})
";
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(temp.path().join("requirements")).expect("mkdir");
        // An include of itself fails; bytes that are not UTF-8 are skipped.
        std::fs::write(
            temp.path().join("requirements/default.txt"),
            "-r default.txt\n",
        )
        .expect("write");
        std::fs::write(temp.path().join("requirements/bin.txt"), [0xff, 0xfe, 0x00])
            .expect("write");
        let setup = temp.path().join("setup.py");
        std::fs::write(&setup, contents).expect("write setup.py");
        let result = extract_setup_py(temp.path(), &setup).expect("extract");

        for expected in ["install_requires", "extras_require.bin"] {
            assert!(
                result.warnings.iter().any(|warning| matches!(
                    warning,
                    ManifestWarning::SetupPyPartiallyStatic { argument, .. } if argument == expected
                )),
                "{expected}: {:?}",
                result.warnings
            );
        }
    }

    #[test]
    fn computed_requirement_table_is_looked_up_by_name() {
        let contents = r#"
import re
_deps = ["numpy>=1.17", "tokenizers>=0.22,<=0.23", "torch>=2.2", "mistral-common[image]>=1.8", "peft>=0.20"]
deps = {b: a for a, b in (re.findall(r"^(([^!=<>~ ]+)(?:[!=<>~ ].*)?$)", x)[0] for x in _deps)}
docs_only = ["sphinx"]

def deps_list(*pkgs):
    return [deps[pkg] for pkg in pkgs]

extras = {}
extras["torch"] = deps_list("torch")
extras["mistral-common"] = deps_list("mistral-common[image]")
if PYTHON_MINOR_VERSION < 14:
    extras["torch"] += deps_list("numpy")
extras["all"] = extras["torch"] + extras["mistral-common"]
install_requires = [deps["numpy"], deps["tokenizers"]]
_deps = []

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
        // The whole table names packages, `peft` included, as it was when
        // looked up; a list never looked up is not a table.
        assert_eq!(
            result.table_names,
            ["mistral-common", "numpy", "peft", "tokenizers", "torch"]
        );
        assert!(
            !result
                .dependencies
                .iter()
                .any(|dep| dep.name == "peft" || dep.name == "sphinx")
        );
    }

    /// Concatenated extras repeat the same literal; it is one declaration (#726).
    #[test]
    fn repeated_requirement_in_concatenated_list_is_pushed_once() {
        let contents = r#"
from setuptools import setup

_deps = ["accelerate>=1", "datasets", "pytest"]
deps = {d.split(">")[0]: d for d in _deps}

def deps_list(*pkgs):
    return [deps[p] for p in pkgs]

extras = {}
extras["torch"] = deps_list("accelerate")
extras["testing"] = deps_list("pytest", "datasets") + extras["torch"]
extras["all"] = extras["torch"] + deps_list("datasets")
extras["dev"] = extras["all"] + extras["testing"]

setup(name="pkg", version="0", packages=["pkg"], extras_require=extras,
      install_requires=["requests", "requests", "requests>=2"])
"#;
        let result = extract(contents, &[]);
        assert_eq!(
            names_in(&result, &extra("dev")),
            vec!["accelerate", "datasets", "pytest"]
        );
        // Only an identical string is merged; another specifier stays.
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["requests", "requests"]
        );
    }

    #[test]
    fn readable_extras_are_known() {
        let result = extract(
            "setup(install_requires=['requests'], extras_require={'test': ['pytest']})\n",
            &[],
        );
        assert_eq!(result.extras, Some(ExtrasState::Known));
        let shim = extract("from setuptools import setup\nsetup()\n", &[]);
        assert_eq!(shim.extras, None);
    }

    /// celery builds its extras with a dict comprehension over a name tuple,
    /// through helpers that forward `*p` to requirements files (#975): every
    /// extra is read, so the state is Known.
    #[test]
    fn extras_from_helpers_over_requirements_files_are_known() {
        let contents = r"
import os
import setuptools

EXTENSIONS = ('redis', 'sqlalchemy')

def _reqs(*f):
    return [r for r in (l.strip() for l in open(
        os.path.join(os.getcwd(), 'requirements', *f)).readlines()) if r]

def reqs(*f):
    return [req for subreq in _reqs(*f) for req in subreq]

def extras(*p):
    return reqs('extras', *p)

def extras_require():
    return {x: extras(x + '.txt') for x in EXTENSIONS}

setuptools.setup(name='celery', install_requires=reqs('default.txt'),
                 extras_require=extras_require())
";
        let result = extract(
            contents,
            &[
                ("requirements/default.txt", "kombu>=5.3\n"),
                ("requirements/extras/redis.txt", "kombu[redis]>=5.3\n"),
                ("requirements/extras/sqlalchemy.txt", "kombu[sqlalchemy]\n"),
            ],
        );
        assert_eq!(result.extras, Some(ExtrasState::Known));
        assert_eq!(names_in(&result, &extra("redis")), vec!["kombu"]);
        assert_eq!(names_in(&result, &extra("sqlalchemy")), vec!["kombu"]);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["kombu"]
        );
    }

    #[test]
    fn unreadable_extras_are_partial_per_extra() {
        let result = extract(
            "setup(extras_require={'test': ['pytest'], 'doc': load(), 'yaml': ['pyyaml'] + more})\n",
            &[],
        );
        assert_eq!(
            result.extras,
            Some(ExtrasState::Partial(
                ["doc", "yaml"].map(str::to_owned).into()
            ))
        );
        assert_eq!(names_in(&result, &extra("test")), vec!["pytest"]);
    }

    /// A dict entry the evaluator cannot key, or code it cannot follow,
    /// leaves the read entries in place and says the dict is incomplete (#869).
    #[test]
    fn extras_require_with_unread_entries_keeps_the_read_ones() {
        for script in [
            "setup(extras_require={'s3': ['boto3'], compute(): ['x']})\n",
            "setup(extras_require={'s3': ['boto3'], **load()})\n",
            "extras = {'s3': ['boto3']}\nextras[name()] = ['x']\nsetup(extras_require=extras)\n",
            "extras = {'s3': ['boto3']}\nfor k in ('gcs',):\n    extras[k] = ['gcs']\n\
             setup(install_requires=['requests'], extras_require=extras)\n",
            "extras = {'s3': ['boto3']}\ndel extras[name()]\nsetup(extras_require=extras)\n",
            "extras = {'s3': ['boto3']}\nextras.update(load())\nsetup(extras_require=extras)\n",
            "setup(extras_require={'s3': ['boto3'], **{key: [] for key in load()}})\n",
            "extras = {'s3': ['boto3']}\nif flag:\n    extras.update(load())\n\
             setup(extras_require=extras)\n",
        ] {
            let result = extract(script, &[]);
            assert!(result.parsed, "{script}");
            assert_eq!(names_in(&result, &extra("s3")), vec!["boto3"], "{script}");
            assert_eq!(result.extras, Some(ExtrasState::Unknown), "{script}");
            assert!(
                result.warnings.iter().any(|warning| matches!(
                    warning,
                    ManifestWarning::SetupPyPartiallyStatic { argument, .. }
                        if argument == "extras_require"
                )),
                "{script}: {:?}",
                result.warnings
            );
        }
    }

    #[test]
    fn extras_require_that_is_not_a_dict_is_unknown() {
        let result = extract(
            "setup(install_requires=['requests'], extras_require=load())\n",
            &[],
        );
        assert!(result.parsed);
        assert_eq!(result.extras, Some(ExtrasState::Unknown));
        assert!(result.warnings.iter().any(|warning| matches!(
            warning,
            ManifestWarning::SetupPyPartiallyStatic { argument, .. } if argument == "extras_require"
        )));
    }

    #[test]
    fn unpacked_setup_kwargs_hide_extras_require() {
        let hidden = extract(
            "kw = load()\nsetup(install_requires=['requests'], **kw)\n",
            &[],
        );
        assert_eq!(hidden.extras, Some(ExtrasState::Unknown));
        assert!(hidden.warnings.iter().any(|warning| matches!(
            warning,
            ManifestWarning::SetupPyPartiallyStatic { argument, .. } if argument == "extras_require"
        )));
        // Python rejects a keyword passed twice, so `**kw` cannot replace it.
        let explicit = extract(
            "kw = load()\nsetup(extras_require={'test': ['pytest']}, **kw)\n",
            &[],
        );
        assert_eq!(explicit.extras, Some(ExtrasState::Known));
    }

    #[test]
    fn unread_setup_py_leaves_extras_unknown() {
        for script in [
            "from setuptools import setup\nsetup_kwargs = {}\n",
            "setup(extras_require={'test': [\n",
        ] {
            let result = extract(script, &[]);
            assert_eq!(result.extras, Some(ExtrasState::Unknown), "{script}");
        }
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
    fn mutated_or_looped_lists_are_partial() {
        for script in [
            "reqs = ['requests']\nreqs.append('pyyaml')\nsetup(install_requires=reqs)\n",
            "reqs = ['requests']\nfor r in extra:\n    reqs += [r]\nsetup(install_requires=reqs)\n",
        ] {
            let result = extract(script, &[]);
            assert!(result.runtime_unknown, "{script}");
            assert_eq!(
                names_in(&result, &DependencyContext::Runtime),
                vec!["requests"]
            );
        }
    }

    #[test]
    fn if_else_branches_are_unioned() {
        let result = extract(
            "if win:\n    reqs = ['requests', 'pyyaml']\nelse:\n    reqs = []\nsetup(install_requires=reqs)\n",
            &[],
        );
        assert!(!result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["requests", "pyyaml"]
        );
    }

    #[test]
    fn unpacked_setup_kwargs_hide_install_requires() {
        let result = extract("kw = load()\nsetup(name='acme', **kw)\n", &[]);
        assert!(result.runtime_unknown);
    }

    #[test]
    fn runaway_evaluation_ends_as_unknown() {
        let doubling = format!(
            "x = ['a']\n{}setup(install_requires=x)\n",
            "x = x + x\n".repeat(40)
        );
        let fan_out =
            "def f():\n    return [f(), f(), f(), f(), f(), f()]\nsetup(install_requires=f())\n";
        for script in [doubling.as_str(), fan_out] {
            assert!(extract(script, &[]).runtime_unknown);
        }
    }

    #[test]
    fn requirements_references_stay_inside_root() {
        let result = extract("setup(install_requires=read('../outside.txt'))\n", &[]);
        assert!(result.runtime_unknown);
        assert_eq!(result.files_missing, Vec::<String>::new());
    }

    #[test]
    fn unknown_non_name_subscript_is_not_a_requirement_table() {
        let result = extract(
            "import os\nreqs = ['requests']\nsetup(install_requires=[os.environ['requests']])\n",
            &[],
        );
        assert!(result.runtime_unknown);
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            Vec::<String>::new()
        );
    }

    #[test]
    fn missing_setup_call_is_unknown() {
        let result = extract("from setuptools import setup\nsetup_kwargs = {}\n", &[]);
        assert!(result.runtime_unknown);
        assert!(!result.parsed);
    }

    #[test]
    fn unreadable_name_keyword_is_dynamic() {
        let result = extract(
            "from setuptools import setup\nabout = {}\nexec(open(\"src/acme/__version__.py\").read(), about)\nsetup(name=about[\"__title__\"], packages=[\"acme\"], package_dir={\"\": \"src\"})\n",
            &[],
        );
        assert!(result.dynamic_name);
        assert_eq!(result.metadata.name, None);
        assert!(!extract("from setuptools import setup\nsetup()\n", &[]).dynamic_name);
    }

    fn extract_bytes(contents: &[u8]) -> SetupPyExtraction {
        let temp = tempfile::tempdir().expect("temp dir");
        let setup = temp.path().join("setup.py");
        std::fs::write(&setup, contents).expect("write setup.py");
        extract_setup_py(temp.path(), &setup).expect("extract")
    }

    /// A latin-1 `setup.py` with its PEP 263 declaration is read like any
    /// other (#552).
    #[test]
    fn declared_latin1_setup_py_is_decoded() {
        let result = extract_bytes(
            b"# -*- coding: latin-1 -*-\nfrom setuptools import setup\n\
              setup(name='acme', author='Ren\xe9', install_requires=['requests'])\n",
        );
        assert!(result.parsed);
        assert!(!result.runtime_unknown);
        assert_eq!(result.metadata.name.as_deref(), Some("acme"));
        assert_eq!(
            names_in(&result, &DependencyContext::Runtime),
            vec!["requests"]
        );
        assert_eq!(result.warnings, []);
    }

    #[test]
    fn undecodable_setup_py_is_skipped_as_unknown() {
        let result = extract_bytes(
            b"from setuptools import setup\nsetup(name='acme', author='Ren\xe9', install_requires=['requests'])\n",
        );
        assert!(!result.parsed);
        assert!(result.runtime_unknown);
        assert_eq!(result.extras, Some(ExtrasState::Unknown));
        assert_eq!(result.dependencies, []);
        assert_eq!(
            result.warnings,
            [ManifestWarning::FileUndecodable {
                file: "setup.py".to_owned()
            }]
        );
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
