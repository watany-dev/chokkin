//! Reachability analysis over a realistic synthetic project.
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

fn bench_reachability(c: &mut Criterion) {
    let mut group = c.benchmark_group("reachability_2kb");
    group.sample_size(30);
    for n in [1_000, 10_000] {
        let project = support::synth_realistic_project(n);
        let report = analyze_project(
            project.path(),
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions::default(),
        )
        .expect("setup");
        let config = chokkin::load_config(&report.probe.root).expect("config");
        let sources = &report.probe.sources;
        let manifest = &report.probe.manifest;
        let target = resolve_target_version(&config.effective, manifest);
        let parse = parse_project_sources_with_cache(
            &report.probe.root,
            sources,
            &target,
            None,
            Some(&CacheOptions::default()),
        )
        .expect("parse");
        let plugins =
            extract_plugin_hints(&report.probe.root, &config, sources, manifest).expect("plugins");
        group.bench_function(BenchmarkId::new("analyze", n), |b| {
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
    }
    group.finish();
}
criterion_group!(benches, bench_reachability);
criterion_main!(benches);
