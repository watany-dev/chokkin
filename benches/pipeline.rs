//! Realistic source sizes and disk-cache paths; setup and cleanup are untimed.
#![expect(
    clippy::expect_used,
    reason = "bench setup aborts on a failed fixture step"
)]
mod support;
use chokkin::internals::analyze_reachability;
use chokkin::internals::{
    AnalyzeOptions, CacheOptions, parse_project_sources_with_cache, resolve_target_version,
};
use chokkin::internals::{PluginExtractRequest, extract_plugin_hints_with_parse};
use chokkin::{RuntimeOverrides, analyze_project};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn clear_cache(root: &std::path::Path) {
    let path = root.join(".chokkin/cache");
    if path.exists() {
        std::fs::remove_dir_all(path).expect("clear cache");
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one benchmark group per pipeline step, sharing one fixture"
)]
fn bench_pipeline(c: &mut Criterion) {
    let sizes = if std::env::var_os("CHOKKIN_BENCH_LARGE").is_some() {
        vec![1_000, 5_000, 10_000]
    } else {
        vec![1_000]
    };
    let mut group = c.benchmark_group("pipeline_2kb");
    group.sample_size(20);
    for n in sizes {
        let project = support::synth_realistic_project(n);
        let root = project.path();
        let overrides = RuntimeOverrides::default();
        let report =
            analyze_project(root, None, &overrides, AnalyzeOptions::default()).expect("setup");
        let uncached = analyze_project(
            project.path(),
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions {
                cache: CacheOptions::disabled(),
                ..AnalyzeOptions::default()
            },
        )
        .expect("uncached setup");
        assert_eq!(
            report.issues, uncached.issues,
            "cache must not change findings"
        );
        drop(uncached);
        let config = chokkin::internals::load_config(&report.probe.root).expect("config");
        let sources = &report.probe.sources;
        let manifest = &report.probe.manifest;
        let target = resolve_target_version(&config.effective, manifest);
        let cache = CacheOptions::default();
        let disabled = CacheOptions::disabled();
        let parse =
            parse_project_sources_with_cache(&report.probe.root, sources, &target, Some(&cache))
                .expect("parse");
        let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
            root: &report.probe.root,
            config: &config,
            sources,
            manifest,
            parse: &parse,
            cache: None,
            dockerfiles: &[],
        })
        .expect("plugins");
        for warm in [false, true] {
            let name = if warm { "analyze_warm" } else { "analyze_cold" };
            if warm {
                analyze_project(root, None, &overrides, AnalyzeOptions::default())
                    .expect("populate");
            }
            group.bench_function(BenchmarkId::new(name, n), |b| {
                b.iter_batched(
                    || {
                        if !warm {
                            clear_cache(root);
                        }
                    },
                    |()| {
                        analyze_project(
                            black_box(root),
                            None,
                            &overrides,
                            AnalyzeOptions::default(),
                        )
                        .expect("analyze")
                    },
                    BatchSize::PerIteration,
                );
            });
        }
        for (name, options) in [
            ("parse_cold_no_cache", &disabled),
            ("parse_cold_with_disk_cache", &cache),
            ("parse_disk_warm", &cache),
        ] {
            if name == "parse_disk_warm" {
                parse_project_sources_with_cache(
                    &report.probe.root,
                    sources,
                    &target,
                    Some(&cache),
                )
                .expect("populate");
            }
            group.bench_function(BenchmarkId::new(name, n), |b| {
                b.iter_batched(
                    || {
                        if name != "parse_disk_warm" {
                            clear_cache(root);
                        }
                    },
                    |()| {
                        parse_project_sources_with_cache(
                            black_box(&report.probe.root),
                            sources,
                            &target,
                            Some(options),
                        )
                        .expect("parse")
                    },
                    BatchSize::PerIteration,
                );
            });
        }
        group.bench_function(BenchmarkId::new("reachability", n), |b| {
            b.iter_batched_ref(
                || report.graph.clone(),
                |graph| {
                    analyze_reachability(
                        black_box(graph),
                        sources,
                        &report.entry,
                        &plugins,
                        &parse,
                        false,
                    )
                    .expect("reachability")
                },
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("discover_populated_cache", n), |b| {
            b.iter(|| {
                chokkin::internals::discover_sources(
                    black_box(&report.probe.root),
                    &config,
                    manifest,
                )
                .expect("discover")
            });
        });
    }
    group.finish();
}

fn bench_workspace(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_members");
    group.sample_size(10);
    let n = 200;
    let project = support::synth_workspace_project(n);
    let root = project.path();
    let overrides = RuntimeOverrides::default();
    let options = AnalyzeOptions {
        cache: CacheOptions::disabled(),
        ..AnalyzeOptions::default()
    };
    let report = analyze_project(root, None, &overrides, options.clone()).expect("setup");
    assert!(
        report.probe.workspace_members.len() >= usize::try_from(n).expect("member count"),
        "every nested pyproject must be detected as a member"
    );
    group.bench_function(BenchmarkId::new("analyze_no_cache", n), |b| {
        b.iter(|| {
            analyze_project(black_box(root), None, &overrides, options.clone()).expect("analyze")
        });
    });
    // Every string literal shaped like a module name is resolved against
    // every member (#1080), which the members above, holding none, miss.
    let (members, refs) = (80, 20);
    let project = support::synth_string_ref_workspace(members, refs);
    let root = project.path();
    let report = analyze_project(root, None, &overrides, options.clone()).expect("setup");
    assert!(
        report.probe.workspace_members.len() >= usize::try_from(members).expect("member count"),
        "every nested pyproject must be detected as a member"
    );
    assert!(
        report.probe.sources.files.len() > usize::try_from(members).expect("member count"),
        "every member's files must be discovered"
    );
    group.bench_function(BenchmarkId::new("string_module_refs", members), |b| {
        b.iter(|| {
            analyze_project(black_box(root), None, &overrides, options.clone()).expect("analyze")
        });
    });
    // Every conf takes hints naming directories of every member, and each
    // import found nowhere is looked up in all of them (#1080).
    let project = support::synth_sys_path_hint_workspace(members, refs);
    let root = project.path();
    let report = analyze_project(root, None, &overrides, options.clone()).expect("setup");
    assert!(
        report.probe.workspace_members.len() >= usize::try_from(members).expect("member count"),
        "every nested pyproject must be detected as a member"
    );
    group.bench_function(BenchmarkId::new("sys_path_hints", members), |b| {
        b.iter(|| {
            analyze_project(black_box(root), None, &overrides, options.clone()).expect("analyze")
        });
    });
    group.finish();
}
criterion_group!(benches, bench_pipeline, bench_workspace);
criterion_main!(benches);
