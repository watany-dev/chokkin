//! Realistic source sizes and disk-cache paths; setup and cleanup are untimed.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod support;
use chokkin::plugins::extract_plugin_hints;
use chokkin::reachability::analyze_reachability;
use chokkin::{
    AnalyzeOptions, CacheOptions, RuntimeOverrides, analyze_project,
    parse_project_sources_with_cache, resolve_target_version,
};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn clear_cache(root: &std::path::Path) {
    let path = root.join(".chokkin/cache");
    if path.exists() {
        std::fs::remove_dir_all(path).expect("clear cache");
    }
}

#[allow(clippy::too_many_lines)]
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
        let config = chokkin::load_config(&report.probe.root).expect("config");
        let sources = &report.probe.sources;
        let manifest = &report.probe.manifest;
        let target = resolve_target_version(&config.effective, manifest);
        let cache = CacheOptions::default();
        let disabled = CacheOptions::disabled();
        let parse = parse_project_sources_with_cache(
            &report.probe.root,
            sources,
            &target,
            None,
            Some(&cache),
        )
        .expect("parse");
        let plugins =
            extract_plugin_hints(&report.probe.root, &config, sources, manifest).expect("plugins");
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
                    None,
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
                            None,
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
                        &report.entry_mode,
                        false,
                    )
                    .expect("reachability")
                },
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("discover_populated_cache", n), |b| {
            b.iter(|| {
                chokkin::discover_sources(black_box(&report.probe.root), &config, manifest)
                    .expect("discover")
            });
        });
    }
    group.finish();
}
criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
