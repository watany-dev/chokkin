//! Warm-cache benchmarks for Phase 2 cache/performance work.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::hint::black_box;
use std::time::Duration;

use chokkin::{
    CacheOptions, ParseCacheStore, discover_project_root, discover_sources, extract_manifest,
    load_config, parse_project_sources_with_cache, resolve_target_version,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

fn bench_parse_cache_warm(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_cache_warm");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(10));

    for n_files in [100u64, 1_000, 5_000, 10_000] {
        let project = support::synth_src_project(n_files);
        let root = discover_project_root(project.path()).expect("discover root");
        let config = load_config(&root).expect("load config");
        let manifest = extract_manifest(&root, &config).expect("extract manifest");
        let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
        let target = resolve_target_version(&config.effective, &manifest);
        let cache_options = CacheOptions::default();
        let mut cache = ParseCacheStore::new();

        parse_project_sources_with_cache(
            &root,
            &sources,
            &target,
            Some(&mut cache),
            Some(&cache_options),
        )
        .expect("warm cache");

        group.throughput(Throughput::Elements(n_files));
        group.bench_with_input(BenchmarkId::new("src", n_files), &n_files, |b, _| {
            b.iter(|| {
                parse_project_sources_with_cache(
                    black_box(&root),
                    black_box(&sources),
                    black_box(&target),
                    Some(&mut cache),
                    Some(&cache_options),
                )
                .expect("parse with warm cache")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_parse_cache_warm);
criterion_main!(benches);
