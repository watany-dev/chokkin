//! I/O-leaning benchmarks: source file discovery (pipeline step 4) at
//! several project sizes and layouts. Fixtures live in a `TempDir` and a
//! warm-up pass fills the page cache before measurement; cold-cache walks
//! are not reproducible enough to benchmark.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yokei::{
    LoadedConfig, LoadedManifest, ProjectRoot, discover_project_root, discover_sources,
    extract_manifest, load_config,
};

fn pipeline_inputs(root_dir: &Path) -> (ProjectRoot, LoadedConfig, LoadedManifest) {
    let root = discover_project_root(root_dir).expect("discover root");
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    // Warm the page cache so iterations measure the walk, not cold reads.
    discover_sources(&root, &config, &manifest).expect("warm up");
    (root, config, manifest)
}

fn bench_src_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("discover_sources");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(10));

    for n_files in [100u64, 1_000, 5_000] {
        let project = support::synth_src_project(n_files);
        let (root, config, manifest) = pipeline_inputs(project.path());
        group.throughput(Throughput::Elements(n_files));
        group.bench_with_input(BenchmarkId::new("src", n_files), &n_files, |b, _| {
            b.iter(|| discover_sources(black_box(&root), &config, &manifest).expect("discover"));
        });
    }
    group.finish();
}

fn bench_layout_variants(c: &mut Criterion) {
    let mut group = c.benchmark_group("discover_sources");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(10));

    {
        let project = support::synth_flat_project(2_000);
        let (root, config, manifest) = pipeline_inputs(project.path());
        group.bench_function("flat_2000", |b| {
            b.iter(|| discover_sources(black_box(&root), &config, &manifest).expect("discover"));
        });
    }

    {
        let project = support::synth_src_project(1_000);
        let (root, mut config, manifest) = pipeline_inputs(project.path());
        config.effective.respect_gitignore = false;
        discover_sources(&root, &config, &manifest).expect("warm up");
        group.bench_function("src_1000_no_gitignore", |b| {
            b.iter(|| discover_sources(black_box(&root), &config, &manifest).expect("discover"));
        });

        config.effective.respect_gitignore = true;
        config.effective.production = true;
        discover_sources(&root, &config, &manifest).expect("warm up");
        group.bench_function("src_1000_production", |b| {
            b.iter(|| discover_sources(black_box(&root), &config, &manifest).expect("discover"));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_src_scaling, bench_layout_variants);
criterion_main!(benches);
