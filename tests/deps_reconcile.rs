//! Integration tests for dependency reconciliation (pipeline step 10).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chokkin::internals::reconcile_with_context;
use chokkin::internals::{
    Confidence, GraphEdge, ModuleOrigin, PluginExtractRequest, ProjectRoot, RootMarker, RuleId,
    ScopedDeclarations, Severity, UsedModule, WorkspaceDependencyBoundary, add_parsed_imports,
    analyze_reachability, apply_resolution_to_graph, build_entry_roots, build_graph_skeleton,
    discover_project_root, discover_sources, extract_manifest, extract_plugin_hints_with_parse,
    load_config, parse_project_sources_with_cache, resolve_imports_for_analysis,
    resolve_target_version,
};
use chokkin::internals::{DependencyRuleContext, RuleContext};
use chokkin::{RuntimeOverrides, probe_project};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

struct DepsInputs {
    manifest: chokkin::internals::LoadedManifest,
    config: chokkin::internals::ChokkinConfig,
    sources: chokkin::internals::DiscoveredSources,
    plugins: chokkin::internals::PluginHints,
    parse: chokkin::internals::ParseSummary,
    graph: chokkin::internals::ProjectGraph,
    resolution: chokkin::internals::ResolutionIndex,
    reachability: chokkin::internals::ReachabilityReport,
    workspace_inputs: Vec<chokkin::internals::WorkspaceMemberInputs>,
}

fn load_deps(path: &Path, production: bool) -> DepsInputs {
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
        let _ = graph.intern_module(reference.module.clone(), ModuleOrigin::Unknown);
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
    let workspace_inputs = probe_project(path, None, &RuntimeOverrides::default())
        .expect("probe")
        .workspace_inputs;

    DepsInputs {
        manifest,
        config: loaded.effective,
        sources,
        plugins,
        parse,
        graph,
        resolution,
        reachability,
        workspace_inputs,
    }
}

fn reconcile_fixture(name: &str) -> chokkin::internals::DependencyReport {
    reconcile_fixture_with_strict(name, false)
}

fn reconcile_fixture_with_strict(name: &str, strict: bool) -> chokkin::internals::DependencyReport {
    reconcile_inputs(&load_deps(&fixture(name), false), strict)
}

fn reconcile_production_fixture(name: &str, strict: bool) -> chokkin::internals::DependencyReport {
    let mut inputs = load_deps(&fixture(name), true);
    inputs.config.production = true;
    reconcile_inputs(&inputs, strict)
}

fn reconcile_inputs(inputs: &DepsInputs, strict: bool) -> chokkin::internals::DependencyReport {
    let workspace_boundaries = inputs
        .workspace_inputs
        .iter()
        .map(|input| WorkspaceDependencyBoundary {
            member_id: &input.member.id,
            manifest: &input.manifest,
        })
        .collect::<Vec<_>>();
    reconcile_with_context(
        &DependencyRuleContext {
            rules: &RuleContext {
                resolution: &inputs.resolution,
                reachability: &inputs.reachability,
                graph: &inputs.graph,
                sources: &inputs.sources,
                parse: &inputs.parse,
            },
            config: &inputs.config,
            strict,
        },
        &inputs.manifest,
        &inputs.plugins,
        &workspace_boundaries,
        &[],
    )
}

/// Matches `IssueSubject::Distribution` only; CHK003/CHK004 carry `Import` subjects.
fn has_dist_rule(report: &chokkin::internals::DependencyReport, rule: RuleId, name: &str) -> bool {
    report.candidates.iter().any(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::internals::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

/// CHK003/CHK004/CHK005 rules reported for `name`, which §10 keeps exclusive.
fn rules_mentioning(report: &chokkin::internals::DependencyReport, name: &str) -> Vec<RuleId> {
    report
        .candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.rule,
                RuleId::Chk003 | RuleId::Chk004 | RuleId::Chk005
            ) && candidate.message.contains(name)
        })
        .map(|candidate| candidate.rule)
        .collect()
}

fn candidate_for_distribution<'a>(
    report: &'a chokkin::internals::DependencyReport,
    rule: RuleId,
    name: &str,
) -> Option<&'a chokkin::internals::IssueCandidate> {
    report.candidates.iter().find(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::internals::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

#[test]
fn unused_boto3_emits_chk002() {
    let report = reconcile_fixture("unused_boto3");
    let boto3 = candidate_for_distribution(&report, RuleId::Chk002, "boto3").expect("boto3 unused");
    assert_eq!(boto3.severity, Severity::Error);
    assert_eq!(boto3.confidence, Confidence::Certain);
    assert!(!has_dist_rule(&report, RuleId::Chk002, "requests"));
}

#[test]
fn missing_yaml_emits_chk003() {
    let report = reconcile_fixture("missing_yaml");
    let yaml = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, Severity::Error);
}

#[test]
fn missing_is_low_confidence_when_runtime_dependencies_are_unknown() {
    let report = reconcile_fixture("setup_py_unknown_runtime");
    let yaml = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, Severity::Info);
    assert_eq!(yaml.confidence, Confidence::Maybe);
}

#[test]
fn non_runtime_missing_is_suppressed_by_default_and_reported_in_strict_mode() {
    let default = reconcile_fixture("dev_missing");
    assert!(!default.candidates.iter().any(|candidate| {
        candidate.rule == RuleId::Chk003
            && (candidate.message.contains("pyyaml") || candidate.message.contains("boto3"))
    }));

    let strict = reconcile_fixture_with_strict("dev_missing", true);
    assert!(strict.candidates.iter().any(|candidate| {
        candidate.rule == RuleId::Chk003 && candidate.message.contains("pyyaml")
    }));
    assert!(strict.candidates.iter().any(|candidate| {
        candidate.rule == RuleId::Chk003 && candidate.message.contains("boto3")
    }));
}

#[test]
fn optional_missing_remains_an_informational_candidate() {
    let report = reconcile_fixture("optional_missing");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("pyyaml"))
        .expect("optional pyyaml missing");
    assert_eq!(candidate.severity, Severity::Info);
    assert_eq!(candidate.confidence, Confidence::Likely);
}

#[test]
fn platform_guard_missing_is_not_a_hard_error() {
    let report = reconcile_fixture("platform_guard_missing");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("tzdata"))
        .expect("platform-guarded tzdata missing");
    assert_eq!(candidate.severity, Severity::Info);
    assert_eq!(candidate.confidence, Confidence::Likely);
    assert!(candidate.message.contains("platform-guarded"));
}

#[test]
fn transitive_urllib3_emits_chk004() {
    let report = reconcile_fixture("transitive_urllib3");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk004 && candidate.message.contains("urllib3"))
        .expect("urllib3 transitive");
    assert_eq!(candidate.severity, Severity::Error);
}

#[test]
fn optional_transitive_import_prefers_chk004() {
    let report = reconcile_fixture("transitive_urllib3_optional");
    let rules = |needle: &str| {
        report
            .candidates
            .iter()
            .filter(|candidate| candidate.message.contains(needle))
            .map(|candidate| (candidate.rule, candidate.severity))
            .collect::<Vec<_>>()
    };
    assert_eq!(rules("urllib3"), [(RuleId::Chk004, Severity::Warning)]);
    assert_eq!(rules("pyyaml"), [(RuleId::Chk003, Severity::Info)]);
}

fn chk004_summary(name: &str) -> Vec<(String, Severity, Confidence)> {
    reconcile_fixture(name)
        .candidates
        .into_iter()
        .filter(|candidate| candidate.rule == RuleId::Chk004)
        .map(|candidate| (candidate.message, candidate.severity, candidate.confidence))
        .collect()
}

#[test]
fn lockfile_formats_match_uv_lock_chk004() {
    let expected = chk004_summary("transitive_urllib3");
    assert_eq!(expected.len(), 1);
    assert!(expected[0].0.contains("urllib3"));
    for (name, kind) in [
        (
            "transitive_urllib3_pylock",
            chokkin::internals::LockfileKind::Pylock,
        ),
        (
            "transitive_urllib3_poetry",
            chokkin::internals::LockfileKind::Poetry,
        ),
        (
            "transitive_urllib3_pdm",
            chokkin::internals::LockfileKind::Pdm,
        ),
    ] {
        let inputs = load_deps(&fixture(name), false);
        let source = inputs.manifest.sources.lockfile.expect(name);
        assert_eq!(source.kind, kind, "{name}");
        assert_eq!(chk004_summary(name), expected, "{name}");
    }
}

#[test]
fn workspace_member_root_declared_dependency_is_allowed_by_default() {
    let report = reconcile_fixture("workspace_member_strict");
    assert!(!report.candidates.iter().any(
        |candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("requests")
    ));
}

#[test]
fn strict_workspace_member_requires_member_local_dependency() {
    let report = reconcile_fixture_with_strict("workspace_member_strict", true);
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk003
                && candidate.message.contains("requests")
                && candidate.message.contains("workspace member api")
        })
        .expect("member-local requests declaration");
    assert_eq!(candidate.severity, Severity::Error);
}

#[test]
fn strict_workspace_member_reports_member_local_misplaced_dependency() {
    let report = reconcile_fixture_with_strict("workspace_member_misplaced", true);
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk005
                && candidate.message.contains("pytest")
                && candidate.message.contains("workspace member api")
        })
        .expect("member-local pytest context mismatch");
    assert_eq!(candidate.severity, Severity::Warning);
    assert_eq!(rules_mentioning(&report, "pytest"), vec![RuleId::Chk005]);
}

/// Issue #263: with no member entry the root declaration is the fallback, so a
/// root dev-only dependency used at runtime by a member is still CHK005.
#[test]
fn strict_workspace_member_falls_back_to_root_dev_only_declaration() {
    let report = reconcile_fixture_with_strict("workspace_member_root_dev", true);
    assert_eq!(rules_mentioning(&report, "requests"), vec![RuleId::Chk005]);
    let candidate = candidate_for_distribution(&report, RuleId::Chk005, "requests")
        .expect("root dev-only requests used at runtime by member");
    assert_eq!(candidate.workspace_member.as_deref(), Some("api"));
}

/// Issue #263: the member entry takes precedence over the root's runtime
/// declaration, so only CHK005 fires (no workspace CHK003 alongside it).
#[test]
fn strict_workspace_member_entry_shadows_root_declaration() {
    let report = reconcile_fixture_with_strict("workspace_member_dev_over_root", true);
    assert_eq!(rules_mentioning(&report, "pytest"), vec![RuleId::Chk005]);
}

#[test]
fn misplaced_pytest_emits_chk005() {
    let report = reconcile_fixture("misplaced_pytest");
    let pytest =
        candidate_for_distribution(&report, RuleId::Chk005, "pytest").expect("pytest misplaced");
    assert_eq!(pytest.severity, Severity::Warning);
}

#[test]
fn misplaced_confidence_follows_the_strongest_import() {
    let report = reconcile_fixture("misplaced_conditional");
    let summary = |name: &str| {
        let candidate = candidate_for_distribution(&report, RuleId::Chk005, name)
            .unwrap_or_else(|| panic!("{name} misplaced"));
        (candidate.severity, candidate.confidence)
    };
    assert_eq!(summary("polars"), (Severity::Info, Confidence::Likely));
    assert_eq!(summary("sympy"), (Severity::Warning, Confidence::Likely));
    // `importlib.import_module` inside a function is deferred too (#599).
    assert_eq!(summary("toolz"), (Severity::Warning, Confidence::Likely));
    // heavy.py is loaded only from inside a function, so its top-level
    // import is deferred; shared.py is also imported at module level (#610).
    assert_eq!(summary("dask"), (Severity::Warning, Confidence::Likely));
    assert_eq!(summary("attrs"), (Severity::Warning, Confidence::Certain));
    // The function-local import comes first; the later top-level one decides.
    assert_eq!(summary("xarray"), (Severity::Warning, Confidence::Certain));
    let xarray = candidate_for_distribution(&report, RuleId::Chk005, "xarray").expect("xarray");
    assert!(matches!(
        xarray.origins.as_slice(),
        [chokkin::internals::Origin::Import { line: 15, .. }]
    ));
    // An equally strong later import keeps the first origin.
    let sympy = candidate_for_distribution(&report, RuleId::Chk005, "sympy").expect("sympy");
    assert!(matches!(
        sympy.origins.as_slice(),
        [chokkin::internals::Origin::Import { line: 19, .. }]
    ));
}

#[test]
fn unlisted_pytest_binary_emits_chk008() {
    let report = reconcile_fixture("unlisted_pytest");
    let binary = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk008)
        .expect("pytest binary unlisted");
    assert_eq!(binary.severity, Severity::Warning);
    assert!(binary.message.contains("pytest"));
}

#[test]
fn duplicate_requests_emits_chk009() {
    let report = reconcile_fixture("duplicate_requests");
    let duplicate = candidate_for_distribution(&report, RuleId::Chk009, "requests")
        .expect("requests duplicate");
    assert_eq!(duplicate.severity, Severity::Warning);
    assert!(duplicate.message.contains("runtime"));
    assert!(duplicate.message.contains("dev"));
}

/// #507: a group declaration that adds extras the runtime one lacks refines
/// it instead of repeating it, even when the distribution is a path source.
#[test]
fn group_declaration_with_extra_extras_is_not_a_duplicate() {
    let report = reconcile_fixture("duplicate_refined_extras");
    assert!(!has_dist_rule(&report, RuleId::Chk009, "streamlit"));
    let duplicate = candidate_for_distribution(&report, RuleId::Chk009, "requests")
        .expect("requests duplicate");
    assert_eq!(
        duplicate.message,
        "requests is declared in multiple contexts: group:dev, runtime"
    );
}

/// #494: extras and groups do not duplicate each other and the project's own
/// extras are never duplicates; only the same list twice is.
#[test]
fn duplicates_across_extras_and_groups_are_not_reported() {
    let report = reconcile_fixture("duplicate_across_extras");
    for name in ["boto3", "acme", "mypy", "requests"] {
        assert!(!has_dist_rule(&report, RuleId::Chk009, name), "{name}");
    }
    let duplicate =
        candidate_for_distribution(&report, RuleId::Chk009, "pytest").expect("pytest duplicate");
    assert_eq!(
        duplicate.message,
        "pytest is declared more than once in group:test"
    );
    assert_eq!(duplicate.origins.len(), 2);
}

#[test]
fn marker_pywin32_emits_chk002_likely_in_strict_mode() {
    let report = reconcile_fixture_with_strict("marker_pywin32", true);
    let pywin32 = candidate_for_distribution(&report, RuleId::Chk002, "pywin32")
        .expect("pywin32 unused with marker");
    assert_eq!(pywin32.confidence, Confidence::Likely);
    assert_eq!(pywin32.severity, Severity::Error);
}

#[test]
fn marker_pywin32_suppressed_by_default() {
    let report = reconcile_fixture("marker_pywin32");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "pywin32"));
}

#[test]
fn used_distributions_tracks_runtime_imports() {
    let report = reconcile_fixture("unused_boto3");
    assert!(report.used_distributions.contains("requests"));
    assert!(!report.used_distributions.contains("boto3"));
}

#[test]
fn reachable_import_graph_is_consistent() {
    let inputs = load_deps(&fixture("unused_boto3"), false);
    let graph = &inputs.graph;
    let imports: Vec<_> = graph
        .edges()
        .iter()
        .filter_map(|edge| match edge {
            GraphEdge::FileImportsModule { file, module, line } => Some((*file, *module, *line)),
            GraphEdge::DistributionProvidesModule { .. } => None,
        })
        .collect();
    let main = graph.file_id("src/acme/main.py").expect("main.py");
    let requests = graph.module_id("requests").expect("requests module");
    assert_eq!(imports, [(main, requests, 1)]);

    // The entry point `acme.main:main` makes the importing file reachable, and
    // the same import reaches Step 10 as a third-party used module.
    assert!(inputs.reachability.reachable.contains(&main));
    assert_eq!(
        inputs.reachability.used_modules,
        [UsedModule {
            full_module: "requests".to_owned(),
            import_root: "requests".to_owned(),
            origin: ModuleOrigin::ThirdParty,
            file: "src/acme/main.py".to_owned(),
            line: 1,
        }]
    );
}

#[test]
fn map_alias_import_resolves_to_python_multipart() {
    let report = reconcile_fixture("map_alias");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "python-multipart"));
    assert!(report.used_distributions.contains("python-multipart"));
}

#[test]
fn self_extra_dependency_is_not_unused() {
    let report = reconcile_fixture("self_extra");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "self-extra"));
    assert!(report.used_distributions.contains("self-extra"));
}

#[test]
fn binary_tool_pyproject_marks_dev_tools_used() {
    let report = reconcile_fixture("binary_tool_pyproject");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "mypy"));
    assert!(!has_dist_rule(&report, RuleId::Chk002, "ruff"));
    assert!(report.used_distributions.contains("mypy"));
    assert!(report.used_distributions.contains("ruff"));
}

#[test]
fn binary_mkdocs_theme_marks_material_used() {
    let report = reconcile_fixture("binary_mkdocs_theme");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "mkdocs"));
    assert!(!has_dist_rule(&report, RuleId::Chk002, "mkdocs-material"));
    assert!(report.used_distributions.contains("mkdocs"));
    assert!(report.used_distributions.contains("mkdocs-material"));
}

#[test]
fn dev_group_only_suppresses_chk002_for_pytest() {
    let report = reconcile_fixture("dev_group_only");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "pytest"));
}

#[test]
fn pdm_dev_dependencies_suppress_chk002() {
    let report = reconcile_fixture("pdm_dev_deps");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "pytest"));
}

#[test]
fn optional_try_import_marks_brotli_used() {
    let report = reconcile_fixture("optional_try_import");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "brotli"));
    assert!(report.used_distributions.contains("brotli"));
}

#[test]
fn platform_guard_import_marks_tzdata_used() {
    let report = reconcile_fixture("platform_guard_import");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "tzdata"));
    assert!(report.used_distributions.contains("tzdata"));
}

fn assert_used(report: &chokkin::internals::DependencyReport, names: &[&str]) {
    for name in names {
        assert!(
            !has_dist_rule(report, RuleId::Chk002, name),
            "{name} flagged"
        );
        assert!(report.used_distributions.contains(*name), "{name} unused");
    }
}

#[test]
fn pdm_scripts_mark_commands_and_call_modules_used() {
    let report = reconcile_fixture("binary_pdm_scripts");
    assert_used(&report, &["alembic", "celery", "gunicorn", "httpx"]);
}

#[test]
fn makefile_recipes_mark_binaries_used_without_following_variables() {
    let report = reconcile_fixture("binary_makefile");
    assert_used(&report, &["alembic", "celery"]);
    assert!(has_dist_rule(&report, RuleId::Chk002, "coverage"));
    assert!(has_dist_rule(&report, RuleId::Chk002, "gunicorn"));
}

#[test]
fn justfile_recipes_mark_binaries_used_skipping_script_recipes() {
    let report = reconcile_fixture("binary_justfile");
    assert_used(&report, &["alembic", "celery"]);
    assert!(has_dist_rule(&report, RuleId::Chk002, "gunicorn"));
}

#[test]
fn dockerfiles_mark_run_cmd_and_entrypoint_binaries_used() {
    let report = reconcile_fixture("binary_dockerfile");
    assert_used(&report, &["alembic", "celery", "gunicorn", "uvicorn"]);
}

#[test]
fn procfile_marks_process_binaries_used() {
    let report = reconcile_fixture("binary_procfile");
    assert_used(&report, &["alembic", "celery", "gunicorn", "uvicorn"]);
}

#[test]
fn gitlab_ci_scripts_mark_binaries_used() {
    let report = reconcile_fixture("binary_gitlab_ci");
    assert_used(&report, &["alembic", "celery", "gunicorn", "uvicorn"]);
}

#[test]
fn pytest_addopts_mark_plugins_used() {
    let report = reconcile_fixture("pytest_addopts_plugins");
    assert_used(
        &report,
        &[
            "pytest-benchmark",
            "pytest-cov",
            "pytest-django",
            "pytest-timeout",
            "pytest-xdist",
        ],
    );
}

#[test]
fn pytest11_entry_points_mark_venv_plugins_used() {
    let report = reconcile_fixture("pytest11_venv_plugins");
    assert_used(&report, &["pytest-sugar"]);
}

#[test]
fn mypy_plugins_and_type_checker_configs_mark_tools_used() {
    let report = reconcile_fixture("mypy_plugins_typecheckers");
    assert_used(
        &report,
        &["basedpyright", "django-stubs", "mypy", "pydantic", "ty"],
    );
}

#[test]
fn uv_constraints_are_not_declarations() {
    let report = reconcile_fixture("uv_tool_constraints");
    for name in ["urllib3", "idna", "requests"] {
        assert!(!has_dist_rule(&report, RuleId::Chk002, name), "{name}");
    }
    assert!(!has_dist_rule(&report, RuleId::Chk009, "requests"));
}

#[test]
fn uv_path_source_resolves_without_venv() {
    let inputs = load_deps(&fixture("uv_path_source"), false);
    assert!(!inputs.resolution.warnings.iter().any(|warning| matches!(
        warning,
        chokkin::internals::ResolveWarning::UnresolvedImport { import, .. } if import == "mylib"
    )));
    let report = reconcile_fixture("uv_path_source");
    assert_eq!(rules_mentioning(&report, "mylib"), []);
    assert!(!has_dist_rule(&report, RuleId::Chk002, "my-lib"));
    assert!(report.used_distributions.contains("my-lib"));
}

/// Issue #499: an in-tree path source with its own `[project]` is read as a
/// workspace member, so its runtime declarations govern the code under it.
#[test]
fn in_tree_path_source_manifest_governs_its_tree() {
    let inputs = load_deps(&fixture("uv_path_source_member"), false);
    assert_eq!(inputs.workspace_inputs.len(), 1);
    let report = reconcile_fixture("uv_path_source_member");
    assert_eq!(rules_mentioning(&report, "requests"), []);
    assert_eq!(rules_mentioning(&report, "numpy"), []);
    assert_eq!(rules_mentioning(&report, "urllib3"), [RuleId::Chk004]);
}

/// Issue #509: `acme.cli:main` in `project.scripts` reaches the `acme` path
/// source with no import of it, and that still uses the dependency.
#[test]
fn path_source_reached_only_from_an_entry_point_is_used() {
    let report = reconcile_fixture("uv_path_source_member");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "acme"));
    assert!(report.used_distributions.contains("acme"));
}

/// Issue #553: `acme-core` from a path source ships `acme.core`, not
/// `acme_core`, so only its member tree can tie the import back to it. Runs
/// the full pipeline: `load_deps` leaves member layouts out, and `acme`
/// would then resolve without going through the member tree.
#[test]
fn path_source_is_used_through_its_member_tree() {
    let report = chokkin::analyze_project(
        &fixture("uv_path_source_renamed_module"),
        None,
        &RuntimeOverrides::default(),
        chokkin::internals::AnalyzeOptions {
            cache: chokkin::internals::CacheOptions::disabled(),
            ..chokkin::internals::AnalyzeOptions::default()
        },
    )
    .expect("analyze");
    assert_eq!(report.issues.issues, []);
}

/// Issue #508: the marker-scoped array form reads the same tree as a member.
#[test]
fn marker_scoped_path_source_manifest_governs_its_tree() {
    let inputs = load_deps(&fixture("uv_path_source_member_marker"), false);
    assert_eq!(inputs.workspace_inputs.len(), 1);
    let report = reconcile_fixture("uv_path_source_member_marker");
    assert_eq!(rules_mentioning(&report, "requests"), []);
    assert_eq!(rules_mentioning(&report, "numpy"), []);
    assert_eq!(rules_mentioning(&report, "urllib3"), [RuleId::Chk004]);
}

#[test]
fn uv_workspace_source_dependency_is_used() {
    let report = reconcile_fixture("uv_workspace_source");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "billing"));
    assert_eq!(rules_mentioning(&report, "billing"), []);
}

#[test]
fn build_plugin_declared_as_runtime_dep_notes_build_requires() {
    let report = reconcile_fixture("build_plugin_declared");
    let unused =
        candidate_for_distribution(&report, RuleId::Chk002, "hatch-vcs").expect("hatch-vcs CHK002");
    assert!(
        unused
            .explain
            .details
            .iter()
            .any(|detail| detail.contains("also in build-system.requires")),
        "details: {:?}",
        unused.explain.details
    );
    assert!(!has_dist_rule(&report, RuleId::Chk002, "hatchling"));
    assert_eq!(rules_mentioning(&report, "hatchling"), []);
    assert!(!has_dist_rule(&report, RuleId::Chk002, "requests"));
}

#[test]
fn include_group_is_checked_once_under_its_declaring_group() {
    let manifest = load_deps(&fixture("include_group"), false).manifest;
    assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
    let boto3 = manifest
        .dependencies
        .iter()
        .filter(|dep| dep.name == "boto3")
        .collect::<Vec<_>>();
    assert_eq!(boto3.len(), 1);
    assert_eq!(
        boto3[0].included_via,
        vec![vec!["server".to_owned(), "Shared_Libs".to_owned()]]
    );

    let report = reconcile_fixture("include_group");
    // httpx is only declared in a group, but `server` pulls it into runtime.
    assert!(!has_dist_rule(&report, RuleId::Chk005, "httpx"));
    assert!(!has_dist_rule(&report, RuleId::Chk002, "pytest"));
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.rule != RuleId::Chk009)
    );

    let unused = report
        .candidates
        .iter()
        .filter(|candidate| candidate.rule == RuleId::Chk002)
        .collect::<Vec<_>>();
    assert_eq!(unused.len(), 1);
    let boto3 = unused[0];
    assert!(matches!(
        &boto3.subject,
        chokkin::internals::IssueSubject::Distribution { name } if name == "boto3"
    ));
    assert!(matches!(
        boto3.origins.as_slice(),
        [chokkin::internals::Origin::Manifest(origin)] if origin.label == "dependency-groups.Shared_Libs[1]"
    ));
    assert!(
        boto3
            .explain
            .details
            .iter()
            .any(|line| line == "included via dependency-groups: server -> Shared_Libs")
    );
}

#[test]
fn library_public_modules_use_dependencies_without_other_entry_roots() {
    let report = reconcile_fixture("library_without_entry_roots");
    // requests: imported by the package `__init__.py`; httpx: by a submodule
    // nothing in the project imports; attrs: by a subpackage `__init__.py`
    // nothing imports.
    assert!(!has_dist_rule(&report, RuleId::Chk002, "requests"));
    assert!(!has_dist_rule(&report, RuleId::Chk002, "httpx"));
    assert!(!has_dist_rule(&report, RuleId::Chk002, "attrs"));
    assert!(has_dist_rule(&report, RuleId::Chk002, "mypy-extensions"));
    // An orphan outside the package may not ship, so its import is no missing
    // dependency.
    assert!(
        !report
            .candidates
            .iter()
            .any(|candidate| candidate.rule == RuleId::Chk003
                && candidate.message.contains("jinja2"))
    );

    for strict in [false, true] {
        let report = reconcile_production_fixture("library_without_entry_roots", strict);
        let unused = report
            .candidates
            .iter()
            .filter(|candidate| candidate.rule == RuleId::Chk002)
            .collect::<Vec<_>>();
        assert_eq!(
            unused,
            Vec::<&chokkin::internals::IssueCandidate>::new(),
            "strict={strict}"
        );
    }
}

#[test]
fn library_package_under_lib_root_uses_its_dependencies() {
    let report = reconcile_fixture("library_lib_package_root");
    assert!(!has_dist_rule(&report, RuleId::Chk002, "requests"));
}

#[test]
fn dev_group_declaration_does_not_hide_unused_setup_py_runtime_declaration() {
    for strict in [false, true] {
        let report = reconcile_fixture_with_strict("setup_py_runtime_behind_dev_group", strict);
        let httpx =
            candidate_for_distribution(&report, RuleId::Chk002, "httpx").expect("httpx CHK002");
        assert!(
            matches!(
                httpx.origins.as_slice(),
                [chokkin::internals::Origin::Manifest(origin)] if origin.file == "setup.py"
            ),
            "strict={strict}: {:?}",
            httpx.origins
        );
        assert!(has_dist_rule(&report, RuleId::Chk002, "requests"));
    }
}
