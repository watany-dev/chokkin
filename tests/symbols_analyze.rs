//! Integration tests for symbol usage analysis (pipeline step 11).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chokkin::internals::RuleContext;
use chokkin::internals::analyze_with_context;
use chokkin::internals::{
    Confidence, PluginExtractRequest, ProjectRoot, RootMarker, RuleId, ScopedDeclarations,
    Severity, add_parsed_imports, analyze_reachability, apply_resolution_to_graph,
    build_entry_roots, build_graph_skeleton, discover_project_root, discover_sources,
    extract_manifest, extract_plugin_hints_with_parse, load_config,
    parse_project_sources_with_cache, resolve_imports_for_analysis, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/symbols")
        .join(name)
}

struct SymbolInputs {
    manifest: chokkin::internals::LoadedManifest,
    sources: chokkin::internals::DiscoveredSources,
    plugins: chokkin::internals::PluginHints,
    parse: chokkin::internals::ParseSummary,
    graph: chokkin::internals::ProjectGraph,
    resolution: chokkin::internals::ResolutionIndex,
    reachability: chokkin::internals::ReachabilityReport,
    entry: chokkin::internals::EntryPlan,
}

fn load_symbols(path: &Path, production: bool) -> SymbolInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse");
    let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &root,
        config: &loaded,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
    })
    .expect("plugin hints");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, production);

    let mut graph = build_graph_skeleton(&manifest, &sources).expect("graph skeleton");
    for module in &parse.modules {
        let file_id = graph.file_id(&module.path).expect("file id");
        add_parsed_imports(&mut graph, file_id, module).expect("parsed imports");
    }
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    for reference in &plugin_refs {
        let _ = graph.intern_module(
            reference.module.clone(),
            chokkin::internals::ModuleOrigin::Unknown,
        );
    }
    let resolution = resolve_imports_for_analysis(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
        &BTreeMap::new(),
        &ScopedDeclarations::default(),
    );
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    let reachability =
        analyze_reachability(&mut graph, &sources, &entry, &plugins, &parse, production)
            .expect("reachability");

    SymbolInputs {
        manifest,
        sources,
        plugins,
        parse,
        graph,
        resolution,
        reachability,
        entry,
    }
}

fn analyze_fixture(name: &str) -> Vec<chokkin::internals::IssueCandidate> {
    let inputs = load_symbols(&fixture(name), false);
    analyze_with_context(
        &RuleContext {
            resolution: &inputs.resolution,
            reachability: &inputs.reachability,
            graph: &inputs.graph,
            sources: &inputs.sources,
            parse: &inputs.parse,
        },
        &inputs.entry,
        &inputs.plugins,
        &inputs.manifest,
        &[],
        None,
    )
}

fn find_symbol<'a>(
    report: &'a [chokkin::internals::IssueCandidate],
    rule: RuleId,
    module: &str,
    name: &str,
) -> Option<&'a chokkin::internals::IssueCandidate> {
    report.iter().find(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::internals::IssueSubject::Symbol { module: m, name: n }
                    if m == module && n == name
            )
    })
}

fn has_symbol_rule(
    report: &[chokkin::internals::IssueCandidate],
    rule: RuleId,
    module: &str,
    name: &str,
) -> bool {
    find_symbol(report, rule, module, name).is_some()
}

#[test]
fn unused_public_function_emits_chk006() {
    let report = analyze_fixture("unused_export");
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "dead_api"
    ));
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "helper"
    ));
    let dead = report
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk006
                && matches!(
                    &candidate.subject,
                    chokkin::internals::IssueSubject::Symbol { name, .. } if name == "dead_api"
                )
        })
        .expect("dead_api candidate");
    assert_eq!(dead.severity, Severity::Warning);
    assert_eq!(dead.confidence, Confidence::Likely);
}

#[test]
fn pytest_fixture_is_not_reported() {
    let report = analyze_fixture("pytest_fixture");
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.conftest",
        "sample_data"
    ));
}

#[test]
fn fastapi_route_and_websocket_handlers_are_external() {
    let report = analyze_fixture("fastapi_websocket");
    for name in ["list_items", "stream"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "acme.routes", name),
            "{name}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.routes",
        "dead_api"
    ));
}

#[test]
fn unused_reexport_emits_chk007() {
    let report = analyze_fixture("unused_reexport");
    assert!(has_symbol_rule(&report, RuleId::Chk007, "acme", "foo"));
    assert!(has_symbol_rule(&report, RuleId::Chk007, "acme", "helpers"));
}

#[test]
fn reexport_imported_from_package_is_not_chk007() {
    // Generated at test time for the same reason as the star-import fixture.
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src/acme")).expect("package dir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"used-reexport\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"src/acme/main.py\"]\n",
        ),
        ("src/acme/__init__.py", "from .sub import bar, foo\n"),
        ("src/acme/main.py", "from acme import foo\n\nprint(foo)\n"),
        ("src/acme/sub.py", "foo = 1\nbar = 2\n"),
    ] {
        std::fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let inputs = load_symbols(temp.path(), false);
    let report = analyze_with_context(
        &RuleContext {
            resolution: &inputs.resolution,
            reachability: &inputs.reachability,
            graph: &inputs.graph,
            sources: &inputs.sources,
            parse: &inputs.parse,
        },
        &inputs.entry,
        &inputs.plugins,
        &inputs.manifest,
        &[],
        None,
    );
    assert!(!has_symbol_rule(&report, RuleId::Chk007, "acme", "foo"));
    assert!(has_symbol_rule(&report, RuleId::Chk007, "acme", "bar"));
}

#[test]
fn reexport_source_module_is_resolved_once() {
    let report = analyze_fixture("unused_reexport");
    let source_module = |name: &str| {
        let candidate = report
            .iter()
            .find(|candidate| {
                candidate.rule == RuleId::Chk007
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Symbol { name: n, .. } if n == name
                    )
            })
            .expect("CHK007 candidate");
        match candidate.origins.first() {
            Some(chokkin::internals::Origin::Import { module, .. }) => module.clone(),
            other => panic!("unexpected origin: {other:?}"),
        }
    };
    // `from .sub import foo`
    assert_eq!(source_module("foo"), "acme.sub");
    // `from . import helpers`
    assert_eq!(source_module("helpers"), "acme.helpers");
}

#[test]
fn unresolved_import_emits_chk010() {
    let report = analyze_fixture("unresolved_import");
    // `some_local_mod` normalizes to a different name; the spelling alone must
    // not turn it into a guessed third-party distribution (#361).
    for root in ["notarealpkg", "some_local_mod"] {
        assert!(
            report.iter().any(|candidate| {
                candidate.rule == RuleId::Chk010
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Import { module, .. } if module == root
                    )
            }),
            "expected CHK010 for {root}"
        );
    }
}

#[test]
fn type_checking_unresolved_import_is_info() {
    let report = analyze_fixture("type_checking_unresolved");
    let chk010 = |root: &str| {
        report
            .iter()
            .filter(|candidate| {
                candidate.rule == RuleId::Chk010
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Import { module, .. } if module == root
                    )
            })
            .map(|candidate| (candidate.severity, candidate.confidence))
            .collect::<Vec<_>>()
    };
    // Only the runtime `import _typeshed` is broken; the `TYPE_CHECKING` one
    // resolves to typeshed's stubs (#584).
    assert_eq!(
        chk010("_typeshed"),
        vec![(Severity::Warning, Confidence::Likely)]
    );
    assert_eq!(
        chk010("typeonlypkg"),
        vec![(Severity::Info, Confidence::Likely)]
    );
    assert_eq!(
        chk010("runtimeonlypkg"),
        vec![(Severity::Warning, Confidence::Likely)]
    );
}

#[test]
fn optional_unresolved_import_is_info() {
    let report = analyze_fixture("optional_unresolved");
    let chk010 = |root: &str| {
        report
            .iter()
            .filter(|candidate| candidate.rule == RuleId::Chk010)
            .filter_map(|candidate| match &candidate.subject {
                chokkin::internals::IssueSubject::Import { module, line, .. } if module == root => {
                    Some((candidate.severity, *line))
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // A caught `ImportError` never breaks a run; the import stays reported
    // for typos (#654). The `except` fallback itself can still fail.
    assert_eq!(chk010("optionalpkg"), vec![(Severity::Info, 4)]);
    assert_eq!(chk010("suppressedpkg"), vec![(Severity::Info, 9)]);
    assert_eq!(chk010("fallbackpkg"), vec![(Severity::Warning, 6)]);
    // One unguarded site keeps the warning and anchors the issue.
    assert_eq!(chk010("mixedpkg"), vec![(Severity::Warning, 11)]);
}

#[test]
fn library_mode_downgrades_chk006_to_info() {
    let report = analyze_fixture("library_mode");
    let unused = report
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk006
                && matches!(
                    &candidate.subject,
                    chokkin::internals::IssueSubject::Symbol { name, .. } if name == "unused_public"
                )
        })
        .expect("unused_public candidate");
    assert_eq!(unused.severity, Severity::Info);
}

#[test]
fn library_mode_unshipped_package_keeps_chk006_warning() {
    let report = analyze_fixture("library_wheel_targets");
    let severity_of = |symbol: &str| {
        report
            .iter()
            .find(|candidate| {
                candidate.rule == RuleId::Chk006
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Symbol { name, .. } if name == symbol
                    )
            })
            .map(|candidate| candidate.severity)
    };
    assert_eq!(severity_of("unused_public"), Some(Severity::Info));
    assert_eq!(severity_of("unused_internal"), Some(Severity::Warning));
}

#[test]
fn import_module_attribute_access_counts_as_external_reference() {
    let report = analyze_fixture("import_attr_access");
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "helper"
    ));
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "dead_api"
    ));
}

#[test]
fn star_import_in_init_is_not_a_reexport() {
    // Generated at test time: a checked-in copy would be analyzed by the
    // repository's own chokkin baseline run.
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src/acme")).expect("package dir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"star-reexport\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"src/acme/main.py\"]\n",
        ),
        ("src/acme/__init__.py", "from .sub import *\n"),
        ("src/acme/main.py", "import acme\n"),
        ("src/acme/sub.py", "VALUE = 1\n"),
    ] {
        std::fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let inputs = load_symbols(temp.path(), false);
    let report = analyze_with_context(
        &RuleContext {
            resolution: &inputs.resolution,
            reachability: &inputs.reachability,
            graph: &inputs.graph,
            sources: &inputs.sources,
            parse: &inputs.parse,
        },
        &inputs.entry,
        &inputs.plugins,
        &inputs.manifest,
        &[],
        None,
    );
    assert!(!has_symbol_rule(&report, RuleId::Chk007, "acme", "*"));
}

#[test]
fn relative_package_import_counts_as_external_reference() {
    // Generated at test time for the same reason as the star-import fixture.
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("src/acme/api")).expect("package dir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"relative-ref\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"src/acme/api/main.py\"]\n",
        ),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/api/__init__.py",
            "def util():\n    return 1\n\ndef other():\n    return 2\n\ndef dead_api():\n    return 3\n",
        ),
        (
            "src/acme/api/main.py",
            "from . import util\nfrom acme.api import other\n\ndef run():\n    return util() + other()\n",
        ),
    ] {
        std::fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let inputs = load_symbols(temp.path(), false);
    let report = analyze_with_context(
        &RuleContext {
            resolution: &inputs.resolution,
            reachability: &inputs.reachability,
            graph: &inputs.graph,
            sources: &inputs.sources,
            parse: &inputs.parse,
        },
        &inputs.entry,
        &inputs.plugins,
        &inputs.manifest,
        &[],
        None,
    );
    for name in ["util", "other"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "acme.api", name),
            "{name}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.api",
        "dead_api"
    ));
}

fn analyze_generated(files: &[(&str, &str)]) -> Vec<chokkin::internals::IssueCandidate> {
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in files {
        let path = temp.path().join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write fixture");
    }
    let inputs = load_symbols(temp.path(), false);
    analyze_with_context(
        &RuleContext {
            resolution: &inputs.resolution,
            reachability: &inputs.reachability,
            graph: &inputs.graph,
            sources: &inputs.sources,
            parse: &inputs.parse,
        },
        &inputs.entry,
        &inputs.plugins,
        &inputs.manifest,
        &[],
        None,
    )
}

const APP_PYPROJECT: &str = "[project]\nname = \"app\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"app\"\n\n[project.scripts]\napp = \"app.main:main\"\n";

#[test]
fn symbol_referenced_only_from_tests_is_not_reported() {
    for tests_init in [false, true] {
        let mut files = vec![
            ("pyproject.toml", APP_PYPROJECT),
            ("app/__init__.py", ""),
            ("app/sub/__init__.py", ""),
            (
                "app/sub/exceptions.py",
                "class UsedOnlyInTests(Exception):\n    pass\n\nclass Dead(Exception):\n    pass\n",
            ),
            ("app/main.py", "def main():\n    pass\n"),
            (
                "tests/test_x.py",
                "from app.sub.exceptions import UsedOnlyInTests\n\ndef test_x():\n    assert UsedOnlyInTests\n",
            ),
        ];
        if tests_init {
            files.push(("tests/__init__.py", ""));
        }
        let report = analyze_generated(&files);
        assert!(
            !has_symbol_rule(
                &report,
                RuleId::Chk006,
                "app.sub.exceptions",
                "UsedOnlyInTests"
            ),
            "tests/__init__.py: {tests_init}"
        );
        assert!(
            has_symbol_rule(&report, RuleId::Chk006, "app.sub.exceptions", "Dead"),
            "tests/__init__.py: {tests_init}"
        );
        assert!(!has_symbol_rule(
            &report,
            RuleId::Chk006,
            "tests.test_x",
            "test_x"
        ));
    }
}

#[test]
fn from_imported_submodule_attribute_access_counts_as_external_reference() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        ("app/sub/__init__.py", ""),
        (
            "app/sub/exceptions.py",
            "class ViaAttr(Exception):\n    pass\n\nclass ViaAlias(Exception):\n    pass\n\nclass ViaRelative(Exception):\n    pass\n\nclass Dead(Exception):\n    pass\n",
        ),
        (
            "app/sub/runner.py",
            "from . import exceptions\n\ndef run():\n    raise exceptions.ViaRelative()\n",
        ),
        (
            "app/main.py",
            "from app.sub import exceptions\nfrom app.sub import exceptions as exc\nfrom app.sub.runner import run\n\ndef main():\n    run()\n    if exc.ViaAlias:\n        raise exceptions.ViaAttr()\n",
        ),
    ]);
    for name in ["ViaAttr", "ViaAlias", "ViaRelative"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "app.sub.exceptions", name),
            "{name}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.sub.exceptions",
        "Dead"
    ));
}

#[test]
fn classes_fetched_by_computed_getattr_on_prefix_imports_are_used() {
    // poetry's command loader (#728).
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        ("app/commands/__init__.py", ""),
        (
            "app/commands/self/lock.py",
            "class SelfLockCommand:\n    pass\n\ndef helper():\n    pass\n",
        ),
        ("app/plugins/__init__.py", ""),
        ("app/plugins/extra.py", "class ExtraPlugin:\n    pass\n"),
        (
            "app/main.py",
            "from importlib import import_module\n\ndef load(name):\n    module = import_module(\"app.commands.\" + name)\n    return getattr(module, name.title() + \"Command\")\n\ndef plugin(name):\n    module = import_module(f\"app.plugins.{name}\")\n    return getattr(module, \"Plugin\")\n\ndef main():\n    load(\"self.lock\")\n    plugin(\"extra\")\n",
        ),
    ]);
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.commands.self.lock",
        "SelfLockCommand"
    ));
    // Only classes, and only under a loader whose getattr name is computed.
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.commands.self.lock",
        "helper"
    ));
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.plugins.extra",
        "ExtraPlugin"
    ));
}

#[test]
fn chk006_message_names_the_symbol_kind() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/dead.py",
            "LIMIT = 3\n\ndef dead_fn():\n    pass\n\nclass DeadClass:\n    pass\n",
        ),
        ("app/main.py", "import app.dead\n\ndef main():\n    pass\n"),
    ]);
    for (name, kind) in [
        ("dead_fn", "function"),
        ("DeadClass", "class"),
        ("LIMIT", "constant"),
    ] {
        let candidate = report
            .iter()
            .find(|candidate| {
                candidate.rule == RuleId::Chk006
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Symbol { name: n, .. } if n == name
                    )
            })
            .unwrap_or_else(|| panic!("{name} candidate"));
        assert_eq!(
            candidate.message,
            format!(
                "public {kind} `{name}` in `app.dead` is not referenced from outside the module"
            )
        );
    }
}

#[test]
fn private_and_type_checking_symbols_are_not_registered() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/api.py",
            "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    Hint = int\n\ndef _private():\n    pass\n\ndef dead_api():\n    pass\n",
        ),
        ("app/main.py", "import app.api\n\ndef main():\n    pass\n"),
    ]);
    for name in ["_private", "Hint"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "app.api", name),
            "{name}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.api",
        "dead_api"
    ));
}

#[test]
fn unused_export_listed_in_all_is_certain() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/api.py",
            "__all__ = [\"listed\"]\n\ndef listed():\n    pass\n\ndef unlisted():\n    pass\n",
        ),
        ("app/main.py", "import app.api\n\ndef main():\n    pass\n"),
    ]);
    let confidence = |name: &str| {
        find_symbol(&report, RuleId::Chk006, "app.api", name)
            .expect("CHK006 candidate")
            .confidence
    };
    assert_eq!(confidence("listed"), Confidence::Certain);
    assert_eq!(confidence("unlisted"), Confidence::Likely);
}

#[test]
fn same_module_reference_after_external_one_keeps_symbol_used() {
    // `app/main.py` is scanned before `app/zeta.py`, so the external reference
    // is recorded first and the self-import must not clear it.
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/main.py",
            "from app.zeta import shared\n\ndef main():\n    shared()\n",
        ),
        (
            "app/zeta.py",
            "from app.zeta import shared\n\ndef shared():\n    pass\n",
        ),
    ]);
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "app.zeta",
        "shared"
    ));
}

#[test]
fn reexport_collection_honours_relative_imports_and_all() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/pkg/__init__.py",
            "from .impl import public_unused, _listed, _private\nfrom app.pkg.impl import absolute\n\n__all__ = [\"_listed\"]\n",
        ),
        (
            "app/pkg/impl.py",
            "def public_unused():\n    pass\n\ndef _listed():\n    pass\n\ndef _private():\n    pass\n\ndef absolute():\n    pass\n",
        ),
        ("app/main.py", "import app.pkg\n\ndef main():\n    pass\n"),
    ]);
    for name in ["public_unused", "_listed"] {
        assert!(
            has_symbol_rule(&report, RuleId::Chk007, "app.pkg", name),
            "{name}"
        );
    }
    for name in ["_private", "absolute"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk007, "app.pkg", name),
            "{name}"
        );
    }
}

const LIBRARY_PYPROJECT: &str =
    "[project]\nname = \"acme\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"library\"\n";

/// `__all__`, `X as X`, `import *` into a public module, and a public
/// package's `__init__` each declare library API (#489).
#[test]
fn library_mode_declared_public_api_is_not_reported() {
    let report = analyze_generated(&[
        ("pyproject.toml", LIBRARY_PYPROJECT),
        (
            "src/acme/__init__.py",
            "from ._client import Client as Client\nfrom ._client import Session\nfrom ._models import *\nfrom . import _api\n\ndef top_level():\n    pass\n",
        ),
        (
            "src/acme/_client.py",
            "class Client:\n    pass\n\nclass Session:\n    pass\n",
        ),
        (
            "src/acme/_models.py",
            "from ._base import *\n\nclass Model:\n    pass\n",
        ),
        (
            "src/acme/_base.py",
            "__all__ = [\"Base\"]\n\nclass Base:\n    pass\n\nclass NotStarred:\n    pass\n",
        ),
        (
            "src/acme/_api.py",
            "__all__ = [\"listed\"]\n\ndef listed():\n    pass\n\ndef unlisted():\n    pass\n",
        ),
        (
            "src/acme/_internal/__init__.py",
            "from .impl import kept as kept, dropped\n\ndef internal_top():\n    pass\n",
        ),
        (
            "src/acme/_internal/impl.py",
            "def kept():\n    pass\n\ndef dropped():\n    pass\n",
        ),
    ]);
    for (rule, module, name) in [
        (RuleId::Chk007, "acme", "Client"),
        (RuleId::Chk007, "acme", "Session"),
        (RuleId::Chk006, "acme", "top_level"),
        (RuleId::Chk006, "acme._models", "Model"),
        (RuleId::Chk006, "acme._base", "Base"),
        (RuleId::Chk006, "acme._api", "listed"),
        (RuleId::Chk007, "acme._internal", "kept"),
    ] {
        assert!(
            !has_symbol_rule(&report, rule, module, name),
            "{rule:?} {module}.{name}: {report:?}"
        );
    }
    for (rule, module, name) in [
        (RuleId::Chk006, "acme._base", "NotStarred"),
        (RuleId::Chk006, "acme._api", "unlisted"),
        (RuleId::Chk006, "acme._internal", "internal_top"),
        (RuleId::Chk007, "acme._internal", "dropped"),
    ] {
        let candidate = find_symbol(&report, rule, module, name)
            .unwrap_or_else(|| panic!("{rule:?} {module}.{name}: {report:?}"));
        assert_eq!(candidate.severity, Severity::Info, "{module}.{name}");
    }
}

/// An `__init__` that reads its own import uses it; an alias is the name the
/// package exposes (#489).
#[test]
fn reexport_read_in_its_own_init_is_not_chk007() {
    let report = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        (
            "app/pkg/__init__.py",
            "from .impl import used_here, unused, original as renamed\n\nused_here()\n",
        ),
        (
            "app/pkg/impl.py",
            "def used_here():\n    pass\n\ndef unused():\n    pass\n\ndef original():\n    pass\n",
        ),
        ("app/main.py", "import app.pkg\n\ndef main():\n    pass\n"),
    ]);
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk007,
        "app.pkg",
        "used_here"
    ));
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk007,
        "app.pkg",
        "unused"
    ));
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk007,
        "app.pkg",
        "renamed"
    ));
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk007,
        "app.pkg",
        "original"
    ));
}

const IN_FILE_USE_MODULE: &str = "from typing import TYPE_CHECKING, Generic, TypeVar, Union\n\nif TYPE_CHECKING:\n    from pydantic import TypeAdapter\nelse:\n    TypeAdapter = dict\n\nT = TypeVar(\"T\")\nArch = Union[str, int]\n\nclass Resource:\n    pass\n\nclass Client(Generic[T]):\n    arch: Arch\n\n    @property\n    def resource(self) -> Resource:\n        return Resource()\n\n    def adapter(self) -> TypeAdapter:\n        return TypeAdapter()\n\ndef unreferenced():\n    pass\n";

/// A library cannot make private a name its own module reads: a `TypeVar`, a
/// type alias, a class reached only as an attribute's type, or a name bound in
/// a `TYPE_CHECKING` / `else` pair (#540). App mode skips them too unless
/// `__all__` exports them (#564).
#[test]
fn symbol_used_in_its_own_module_is_not_chk006() {
    let library = analyze_generated(&[
        ("pyproject.toml", LIBRARY_PYPROJECT),
        (
            "src/acme/__init__.py",
            "from ._base import Client as Client\n",
        ),
        ("src/acme/_base.py", IN_FILE_USE_MODULE),
    ]);
    for name in ["T", "Arch", "Resource", "TypeAdapter"] {
        assert!(
            !has_symbol_rule(&library, RuleId::Chk006, "acme._base", name),
            "{name}: {library:?}"
        );
    }
    assert!(has_symbol_rule(
        &library,
        RuleId::Chk006,
        "acme._base",
        "unreferenced"
    ));

    let app_module = format!("__all__ = [\"Client\", \"Arch\"]\n\n{IN_FILE_USE_MODULE}");
    let app = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        ("app/base.py", &app_module),
        (
            "app/main.py",
            "from app.base import Client\n\ndef main():\n    Client()\n",
        ),
    ]);
    for name in ["T", "Resource", "TypeAdapter"] {
        assert!(
            !has_symbol_rule(&app, RuleId::Chk006, "app.base", name),
            "{name}: {app:?}"
        );
    }
    let arch = find_symbol(&app, RuleId::Chk006, "app.base", "Arch")
        .unwrap_or_else(|| panic!("Arch: {app:?}"));
    assert_eq!(arch.severity, Severity::Warning);
    assert_eq!(arch.confidence, Confidence::Certain);
    assert!(has_symbol_rule(
        &app,
        RuleId::Chk006,
        "app.base",
        "unreferenced"
    ));
}

const STRING_ANNOTATION_MODULE: &str = "from typing import TypeVar\n\nT = TypeVar(\"T\")\n\ndef f(x: \"list[T]\") -> \"T\":\n    return x[0]\n";

/// A name read only inside a quoted annotation is still read by its module
/// (#545), which skips CHK006 in both modes (#540, #564).
#[test]
fn symbol_used_in_string_annotation_is_not_chk006() {
    let library = analyze_generated(&[
        ("pyproject.toml", LIBRARY_PYPROJECT),
        ("src/acme/__init__.py", "from ._base import f as f\n"),
        ("src/acme/_base.py", STRING_ANNOTATION_MODULE),
    ]);
    assert!(
        !has_symbol_rule(&library, RuleId::Chk006, "acme._base", "T"),
        "{library:?}"
    );

    let app = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        ("app/base.py", STRING_ANNOTATION_MODULE),
        (
            "app/main.py",
            "from app.base import f\n\ndef main():\n    f([1])\n",
        ),
    ]);
    assert!(
        !has_symbol_rule(&app, RuleId::Chk006, "app.base", "T"),
        "{app:?}"
    );
}

/// Tests outside `tests/` still use library symbols: the rootdir
/// `conftest.py` and a suite under pytest `testpaths` are discovered (#544).
#[test]
fn library_mode_symbol_used_by_root_conftest_or_testpaths_is_not_chk006() {
    let report = analyze_generated(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.0.0\"\n\n[tool.chokkin]\nmode = \"library\"\n\n[tool.pytest.ini_options]\ntestpaths = \"t/unit/\"\n",
        ),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/helpers.py",
            "def from_conftest():\n    pass\n\ndef from_testpath():\n    pass\n\ndef unreferenced():\n    pass\n",
        ),
        (
            "conftest.py",
            "from acme.helpers import from_conftest\n\nfrom_conftest()\n",
        ),
        (
            "t/unit/test_helpers.py",
            "from acme.helpers import from_testpath\n\ndef test_it():\n    from_testpath()\n",
        ),
    ]);
    for name in ["from_conftest", "from_testpath"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "acme.helpers", name),
            "{name}: {report:?}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.helpers",
        "unreferenced"
    ));
}

const ALEMBIC_REVISION: &str = "revision = \"a1\"\ndown_revision = None\nbranch_labels = None\ndepends_on = None\n\ndef upgrade():\n    pass\n\ndef downgrade():\n    pass\n\ndef helper():\n    pass\n";

/// protoc output, a transformers `modular_*.py` beside its `modeling_*.py`,
/// and alembic's revision attributes are read by tools, not imports (#655).
#[test]
fn generated_code_and_alembic_revision_names_are_not_chk006() {
    let report = analyze_generated(&[
        (
            "pyproject.toml",
            &format!("{LIBRARY_PYPROJECT}\n[tool.chokkin.plugins]\nalembic = true\n"),
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/protos/__init__.py", ""),
        ("src/acme/protos/svc_pb2.py", "AWS_URL = 1\n"),
        (
            "src/acme/protos/svc_pb2_grpc.py",
            "class SvcStub:\n    pass\n",
        ),
        ("src/acme/bert/__init__.py", ""),
        (
            "src/acme/bert/modeling_bert.py",
            "class BertModel:\n    pass\n",
        ),
        (
            "src/acme/bert/modular_bert.py",
            "class BertModel:\n    pass\n",
        ),
        ("src/acme/gpt/__init__.py", ""),
        ("src/acme/gpt/modular_gpt.py", "class GptModel:\n    pass\n"),
        ("src/acme/migrations/__init__.py", ""),
        ("src/acme/migrations/versions/__init__.py", ""),
        ("src/acme/migrations/versions/a1_init.py", ALEMBIC_REVISION),
        ("src/acme/versions.py", "def upgrade():\n    pass\n"),
        (
            "conftest.py",
            "import acme.protos.svc_pb2\nimport acme.protos.svc_pb2_grpc\nimport acme.bert.modeling_bert\nimport acme.bert.modular_bert\nimport acme.gpt.modular_gpt\nimport acme.migrations.versions.a1_init\nimport acme.versions\n",
        ),
    ]);
    let quiet = [
        ("acme.protos.svc_pb2", "AWS_URL"),
        ("acme.protos.svc_pb2_grpc", "SvcStub"),
        ("acme.bert.modular_bert", "BertModel"),
        ("acme.migrations.versions.a1_init", "revision"),
        ("acme.migrations.versions.a1_init", "down_revision"),
        ("acme.migrations.versions.a1_init", "branch_labels"),
        ("acme.migrations.versions.a1_init", "depends_on"),
        ("acme.migrations.versions.a1_init", "upgrade"),
        ("acme.migrations.versions.a1_init", "downgrade"),
    ];
    for (module, name) in quiet {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, module, name),
            "{module}.{name}: {report:?}"
        );
    }
    let reported = [
        ("acme.bert.modeling_bert", "BertModel"),
        ("acme.gpt.modular_gpt", "GptModel"),
        ("acme.migrations.versions.a1_init", "helper"),
        ("acme.versions", "upgrade"),
    ];
    for (module, name) in reported {
        assert!(
            has_symbol_rule(&report, RuleId::Chk006, module, name),
            "{module}.{name}: {report:?}"
        );
    }

    // Without the alembic plugin, `versions/` is just a directory name.
    let report = analyze_generated(&[
        ("pyproject.toml", LIBRARY_PYPROJECT),
        ("src/acme/__init__.py", ""),
        ("src/acme/versions/__init__.py", ""),
        ("src/acme/versions/a1_init.py", ALEMBIC_REVISION),
        ("conftest.py", "import acme.versions.a1_init\n"),
    ]);
    assert!(
        has_symbol_rule(&report, RuleId::Chk006, "acme.versions.a1_init", "upgrade"),
        "{report:?}"
    );
}

/// Files alembic loads from `script_location` are never imported, so even
/// names only a project's own tooling reads (airflow's `airflow_version`)
/// are no API (#667).
#[test]
fn alembic_script_files_are_not_chk006() {
    let report = analyze_generated(&[
        (
            "pyproject.toml",
            &format!("{LIBRARY_PYPROJECT}\n[tool.chokkin.plugins]\nalembic = true\n"),
        ),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/alembic.ini",
            "[alembic]\nscript_location = %(here)s/migrations\n",
        ),
        ("src/acme/migrations/__init__.py", ""),
        (
            "src/acme/migrations/env.py",
            "def run_migrations_online():\n    pass\n",
        ),
        ("src/acme/migrations/versions/__init__.py", ""),
        (
            "src/acme/migrations/versions/a1_init.py",
            &format!("{ALEMBIC_REVISION}acme_version = \"1.0\"\n"),
        ),
    ]);
    let chk006: Vec<_> = report
        .iter()
        .filter(|candidate| candidate.rule == RuleId::Chk006)
        .collect();
    assert_eq!(chk006, Vec::<&chokkin::internals::IssueCandidate>::new());
}

#[test]
fn repeated_unresolved_import_emits_one_chk010_per_file() {
    let report = analyze_fixture("repeated_unresolved");
    let chk010 = |path: &str| {
        report
            .iter()
            .filter(|candidate| {
                candidate.rule == RuleId::Chk010
                    && matches!(
                        &candidate.subject,
                        chokkin::internals::IssueSubject::Import { module, file, .. }
                            if module == "lazypkg" && file == path
                    )
            })
            .collect::<Vec<_>>()
    };
    let main = chk010("src/acme/main.py");
    assert_eq!(main.len(), 1, "{main:?}");
    // The runtime imports outrank the `TYPE_CHECKING` one on line 4, as both
    // severity and anchor.
    assert!(matches!(
        &main[0].subject,
        chokkin::internals::IssueSubject::Import { line: 8, .. }
    ));
    assert_eq!(main[0].severity, Severity::Warning);
    let origin_lines = main[0]
        .origins
        .iter()
        .map(|origin| match origin {
            chokkin::internals::Origin::Import { line, .. } => *line,
            other => panic!("unexpected origin: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(origin_lines, vec![8, 4, 14]);
    assert!(
        main[0]
            .explain
            .details
            .contains(&"also imported at lines 4, 14".to_owned()),
        "{:?}",
        main[0].explain.details
    );

    let types = chk010("src/acme/types.py");
    assert_eq!(types.len(), 1, "{types:?}");
    assert_eq!(types[0].severity, Severity::Info);
}
