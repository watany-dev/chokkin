//! Integration tests for symbol usage analysis (pipeline step 11).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::rules::RuleContext;
use chokkin::rules::symbols::analyze_with_context;
use chokkin::{
    Confidence, PluginExtractRequest, ProjectRoot, RootMarker, RuleId, Severity,
    add_parsed_imports, analyze_reachability, apply_resolution_to_graph, build_entry_roots,
    build_graph_skeleton, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints_with_parse, load_config, parse_project_sources_with_cache,
    resolve_imports, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/symbols")
        .join(name)
}

struct SymbolInputs {
    manifest: chokkin::LoadedManifest,
    sources: chokkin::DiscoveredSources,
    plugins: chokkin::PluginHints,
    parse: chokkin::ParseSummary,
    graph: chokkin::ProjectGraph,
    resolution: chokkin::ResolutionIndex,
    reachability: chokkin::ReachabilityReport,
    entry: chokkin::EntryPlan,
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
        let _ = graph.intern_module(reference.module.clone(), chokkin::ModuleOrigin::Unknown);
    }
    let resolution = resolve_imports(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
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

fn analyze_fixture(name: &str) -> Vec<chokkin::IssueCandidate> {
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
        inputs.entry.mode,
        &inputs.manifest,
    )
}

fn find_symbol<'a>(
    report: &'a [chokkin::IssueCandidate],
    rule: RuleId,
    module: &str,
    name: &str,
) -> Option<&'a chokkin::IssueCandidate> {
    report.iter().find(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::IssueSubject::Symbol { module: m, name: n }
                    if m == module && n == name
            )
    })
}

fn has_symbol_rule(
    report: &[chokkin::IssueCandidate],
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
                    chokkin::IssueSubject::Symbol { name, .. } if name == "dead_api"
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
        inputs.entry.mode,
        &inputs.manifest,
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
                        chokkin::IssueSubject::Symbol { name: n, .. } if n == name
                    )
            })
            .expect("CHK007 candidate");
        match candidate.origins.first() {
            Some(chokkin::Origin::Import { module, .. }) => module.clone(),
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
                        chokkin::IssueSubject::Import { module, .. } if module == root
                    )
            }),
            "expected CHK010 for {root}"
        );
    }
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
                    chokkin::IssueSubject::Symbol { name, .. } if name == "unused_public"
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
                        chokkin::IssueSubject::Symbol { name, .. } if name == symbol
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
        inputs.entry.mode,
        &inputs.manifest,
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
        inputs.entry.mode,
        &inputs.manifest,
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

fn analyze_generated(files: &[(&str, &str)]) -> Vec<chokkin::IssueCandidate> {
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
        inputs.entry.mode,
        &inputs.manifest,
    )
}

const APP_PYPROJECT: &str = "[project]\nname = \"app\"\nversion = \"0.0.0\"\n\n[project.scripts]\napp = \"app.main:main\"\n";

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
                        chokkin::IssueSubject::Symbol { name: n, .. } if n == name
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
/// a `TYPE_CHECKING` / `else` pair (#540). App mode still reports them.
#[test]
fn library_mode_symbol_used_in_its_own_module_is_not_chk006() {
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

    let app = analyze_generated(&[
        ("pyproject.toml", APP_PYPROJECT),
        ("app/__init__.py", ""),
        ("app/base.py", IN_FILE_USE_MODULE),
        (
            "app/main.py",
            "from app.base import Client\n\ndef main():\n    Client()\n",
        ),
    ]);
    for name in ["T", "Arch", "Resource", "unreferenced"] {
        assert!(
            has_symbol_rule(&app, RuleId::Chk006, "app.base", name),
            "{name}: {app:?}"
        );
    }
}

const STRING_ANNOTATION_MODULE: &str = "from typing import TypeVar\n\nT = TypeVar(\"T\")\n\ndef f(x: \"list[T]\") -> \"T\":\n    return x[0]\n";

/// A name read only inside a quoted annotation is still read by its module
/// (#545).
#[test]
fn library_mode_symbol_used_in_string_annotation_is_not_chk006() {
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
        has_symbol_rule(&app, RuleId::Chk006, "app.base", "T"),
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
