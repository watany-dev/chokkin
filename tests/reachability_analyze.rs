//! Integration tests for reachability analysis (pipeline step 9).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::{
    Confidence, GraphEdge, ProjectMode, ProjectRoot, RootMarker, add_parsed_imports,
    analyze_reachability, apply_entry_plan, apply_resolution_to_graph, build_entry_roots,
    build_graph_skeleton, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints, load_config, parse_project_sources, resolve_imports,
    resolve_target_version, trace_to_file,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reachability")
        .join(name)
}

fn plugins_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

struct ReachabilityInputs {
    sources: chokkin::DiscoveredSources,
    plugins: chokkin::PluginHints,
    parse: chokkin::ParseSummary,
    entry: chokkin::EntryPlan,
    graph: chokkin::ProjectGraph,
}

fn load_reachability(path: &Path, production: bool) -> ReachabilityInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let plugins = extract_plugin_hints(&root, &loaded, &sources, &manifest).expect("plugin hints");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, production)
        .expect("entry plan");

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
        &root,
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
    )
    .expect("resolve imports");
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    apply_entry_plan(&mut graph, &entry).expect("apply entry plan");

    ReachabilityInputs {
        sources,
        plugins,
        parse,
        entry,
        graph,
    }
}

#[test]
fn chain_import_reaches_transitive_modules() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert_eq!(inputs.entry.mode.mode, ProjectMode::App);
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/main.py").expect("main"))
    );
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/a.py").expect("a"))
    );
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/b.py").expect("b"))
    );
    assert!(
        report
            .unreachable
            .iter()
            .any(|file| file.path == "src/acme/legacy.py")
    );
    assert!(
        inputs
            .graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::FileReachesFile { .. }))
    );
}

#[test]
fn orphan_file_is_unreachable_in_app_mode() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let legacy = report
        .unreachable
        .iter()
        .find(|file| file.path == "src/acme/legacy.py")
        .expect("legacy");
    assert_eq!(legacy.max_confidence, Confidence::Certain);
}

#[test]
fn library_mode_caps_orphan_confidence() {
    let mut inputs = load_reachability(&fixture("library_orphan"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert_eq!(inputs.entry.mode.mode, ProjectMode::Library);
    let orphan = report
        .unreachable
        .iter()
        .find(|file| file.path == "src/acme/orphan.py")
        .expect("orphan");
    assert_eq!(orphan.max_confidence, Confidence::Maybe);
}

#[test]
fn plugin_module_reference_reaches_app_package() {
    let mut inputs = load_reachability(&fixture("plugin_module_ref"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert!(
        report.reachable.contains(
            &inputs
                .graph
                .file_id("myapp/__init__.py")
                .expect("myapp init")
        )
    );
    assert!(
        inputs
            .graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::ConfigReferenceUsesModule { .. }))
    );
}

#[test]
fn dynamic_literal_import_reaches_target_module() {
    let mut inputs = load_reachability(&fixture("dynamic_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert!(
        report.reachable.contains(
            &inputs
                .graph
                .file_id("src/acme/plugins.py")
                .expect("plugins")
        )
    );
}

#[test]
fn django_migrations_are_framework_used() {
    let mut inputs = load_reachability(&plugins_fixture("django_migrations"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let migration = inputs
        .graph
        .file_id("mysite/migrations/0001_initial.py")
        .expect("migration");
    assert!(report.framework_used.contains(&migration));
    assert!(report.reachable.contains(&migration));
    assert!(
        !report
            .unreachable
            .iter()
            .any(|file| file.path == "mysite/migrations/0001_initial.py")
    );
}

#[test]
fn trace_to_file_returns_import_chain() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let target = inputs.graph.file_id("src/acme/b.py").expect("target file");
    let trace = trace_to_file(&report, target).expect("trace");
    assert_eq!(trace.target, target);
    assert!(!trace.steps.is_empty());
}

mod golden {
    use std::fs;
    use std::path::Path;

    use chokkin::{EntryOrigin, UnreachableReason};
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct ReachabilitySnapshot {
        mode: String,
        mode_confidence: String,
        entry_roots: Vec<EntryRootSnapshot>,
        reachable: Vec<String>,
        unreachable: Vec<UnreachableSnapshot>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct EntryRootSnapshot {
        path: String,
        origins: Vec<String>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct UnreachableSnapshot {
        path: String,
        confidence: String,
        reasons: Vec<String>,
    }

    fn format_origin(origin: &EntryOrigin) -> String {
        match origin {
            EntryOrigin::Config => "config".to_owned(),
            EntryOrigin::Manifest { name, group } => format!("manifest:{group}:{name}"),
            EntryOrigin::Plugin { plugin, label } => format!("plugin:{}:{label}", plugin.as_key()),
            EntryOrigin::Auto { rule } => format!("auto:{rule}"),
            EntryOrigin::SymbolRef { label, .. } => format!("symbol:{label}"),
        }
    }

    fn format_reason(reason: UnreachableReason) -> String {
        match reason {
            UnreachableReason::NotReachable => "not_reachable",
            UnreachableReason::ExcludedInit => "excluded_init",
            UnreachableReason::ExcludedStub => "excluded_stub",
            UnreachableReason::ExcludedTestContext => "excluded_test_context",
            UnreachableReason::ExcludedProductionContext => "excluded_production_context",
            UnreachableReason::FrameworkUsed => "framework_used",
        }
        .to_owned()
    }

    fn snapshot_from_inputs(
        inputs: &ReachabilityInputs,
        report: &chokkin::ReachabilityReport,
    ) -> ReachabilitySnapshot {
        let mut reachable = report
            .reachable
            .iter()
            .filter_map(|file_id| inputs.graph.file(*file_id).map(|node| node.path.clone()))
            .collect::<Vec<_>>();
        reachable.sort();

        let unreachable = report
            .unreachable
            .iter()
            .map(|file| UnreachableSnapshot {
                path: file.path.clone(),
                confidence: file.max_confidence.as_str().to_owned(),
                reasons: file
                    .reasons
                    .iter()
                    .map(|reason| format_reason(*reason))
                    .collect(),
            })
            .collect();

        let mut entry_roots = inputs
            .entry
            .roots
            .iter()
            .map(|root| EntryRootSnapshot {
                path: root.spec.path.clone(),
                origins: root.origins.iter().map(format_origin).collect(),
            })
            .collect::<Vec<_>>();
        entry_roots.sort_by(|left, right| left.path.cmp(&right.path));

        ReachabilitySnapshot {
            mode: format!("{:?}", inputs.entry.mode.mode).to_ascii_lowercase(),
            mode_confidence: format!("{:?}", inputs.entry.mode.confidence).to_ascii_lowercase(),
            entry_roots,
            reachable,
            unreachable,
        }
    }

    fn analyze_fixture(
        path: &Path,
        production: bool,
    ) -> (ReachabilityInputs, chokkin::ReachabilityReport) {
        let mut inputs = load_reachability(path, production);
        let report = analyze_reachability(
            &mut inputs.graph,
            &inputs.sources,
            &inputs.entry,
            &inputs.plugins,
            &inputs.parse,
            &inputs.entry.mode,
            production,
        )
        .expect("reachability");
        (inputs, report)
    }

    fn assert_matches_golden(fixture_name: &str, golden_name: &str, production: bool) {
        let path = fixture(fixture_name);
        let (inputs, report) = analyze_fixture(&path, production);
        let snapshot = snapshot_from_inputs(&inputs, &report);
        let golden_path = path.join("golden").join(golden_name);
        let expected = fs::read_to_string(&golden_path).expect("read golden file");
        let expected_snapshot: ReachabilitySnapshot =
            serde_json::from_str(&expected).expect("parse golden json");
        assert_eq!(
            snapshot,
            expected_snapshot,
            "golden mismatch for {}",
            golden_path.display()
        );
    }

    #[test]
    #[ignore = "run manually to refresh golden files"]
    fn write_golden_snapshots() {
        let cases = [
            ("shipped_entries", "default.json", false),
            ("shipped_entries", "production.json", true),
            ("shipped_entries_library", "default.json", false),
        ];
        for (fixture_name, golden_name, production) in cases {
            let path = fixture(fixture_name);
            let (inputs, report) = analyze_fixture(&path, production);
            let snapshot = snapshot_from_inputs(&inputs, &report);
            let golden_dir = path.join("golden");
            fs::create_dir_all(&golden_dir).expect("golden dir");
            let json = serde_json::to_string_pretty(&snapshot).expect("serialize");
            fs::write(golden_dir.join(golden_name), format!("{json}\n")).expect("write golden");
        }
    }

    #[test]
    fn shipped_entries_default_matches_golden() {
        assert_matches_golden("shipped_entries", "default.json", false);
    }

    #[test]
    fn shipped_entries_production_matches_golden() {
        assert_matches_golden("shipped_entries", "production.json", true);
    }

    #[test]
    fn shipped_entries_library_matches_golden() {
        assert_matches_golden("shipped_entries_library", "default.json", false);
    }

    #[test]
    fn removing_manifest_script_entry_shrinks_reachability() {
        let path = fixture("shipped_entries");
        let (mut inputs, full_report) = analyze_fixture(&path, false);
        let used_by_cli = "src/acme/used_by_cli.py";
        assert!(
            full_report
                .reachable
                .contains(&inputs.graph.file_id(used_by_cli).expect("used_by_cli"))
        );

        inputs
            .entry
            .roots
            .retain(|root| root.spec.path != "src/acme/cli.py");
        let reduced_report = analyze_reachability(
            &mut inputs.graph,
            &inputs.sources,
            &inputs.entry,
            &inputs.plugins,
            &inputs.parse,
            &inputs.entry.mode,
            false,
        )
        .expect("reachability");

        assert!(
            !reduced_report
                .reachable
                .contains(&inputs.graph.file_id(used_by_cli).expect("used_by_cli id"))
        );
        assert!(
            reduced_report.reachable.contains(
                &inputs
                    .graph
                    .file_id("src/acme/reached_via_main.py")
                    .expect("main chain")
            )
        );
    }
}
