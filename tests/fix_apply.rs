//! Integration tests for optional fix (pipeline step 13).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::path::{Path, PathBuf};

use chokkin::{
    ExitStatus, FixOptions, ProjectRoot, RootMarker, RuleId, RuntimeOverrides, add_parsed_imports,
    analyze_reachability, analyze_symbols, apply_entry_plan, apply_fixes,
    apply_resolution_to_graph, build_entry_roots, build_graph_skeleton, discover_project_root,
    discover_sources, emit_issues, extract_manifest, extract_plugin_hints, load_config,
    parse_project_sources, reconcile_dependencies, resolve_imports, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

#[test]
fn fix_removes_certain_unused_dependency_from_pyproject() {
    let source = fixture("unused_boto3");
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&source, temp.path()).expect("copy fixture");
    let path = temp.path().to_path_buf();

    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
        start: path.clone(),
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let plugins = extract_plugin_hints(&root, &loaded, &sources, &manifest).expect("plugin hints");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, false)
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
    )
    .expect("resolve imports");
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    apply_entry_plan(&mut graph, &entry).expect("apply entry plan");
    let reachability = analyze_reachability(
        &mut graph,
        &sources,
        &entry,
        &plugins,
        &parse,
        &entry.mode,
        false,
    )
    .expect("reachability");
    let deps = reconcile_dependencies(
        &manifest,
        &resolution,
        &reachability,
        &plugins,
        &loaded.effective,
        &sources,
        &parse,
        &graph,
        false,
    );
    let symbols = analyze_symbols(
        &parse,
        &resolution,
        &reachability,
        &entry,
        &plugins,
        &entry.mode,
        &graph,
        &sources,
        &manifest,
    );
    let issues = emit_issues(
        &reachability,
        &deps,
        &symbols,
        &parse,
        &loaded.effective,
        &RuntimeOverrides::default(),
        &entry.mode,
    );
    assert_eq!(issues.exit_status, ExitStatus::IssuesFound);

    let pyproject = root.path.join("pyproject.toml");
    let before = std::fs::read_to_string(&pyproject).expect("read pyproject");
    assert!(before.contains("boto3"));

    let fix_report =
        apply_fixes(&issues, &root, &manifest, FixOptions::default()).expect("apply fixes");
    assert_eq!(fix_report.applied.len(), 1);
    assert_eq!(fix_report.applied[0].rule, RuleId::Chk002);

    let after = std::fs::read_to_string(&pyproject).expect("read pyproject after fix");
    assert!(!after.contains("\"boto3\""));
    assert!(after.contains("requests"));
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
