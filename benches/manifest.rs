//! CPU-leaning benchmarks: manifest extraction (pipeline step 3) per input
//! format, plus single regression cases for root discovery (step 1) and
//! config load (step 2). Inputs are few but large files, so after warm-up
//! the measurement is dominated by parsing rather than I/O.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::hint::black_box;
use std::path::Path;

use chokkin::{LoadedConfig, ProjectRoot, discover_project_root, extract_manifest, load_config};
use criterion::{Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

fn pipeline_inputs(root_dir: &Path) -> (ProjectRoot, LoadedConfig) {
    let root = discover_project_root(root_dir).expect("discover root");
    let config = load_config(&root).expect("load config");
    (root, config)
}

fn bench_extract(c: &mut Criterion, name: &str, project: &TempDir) {
    let (root, config) = pipeline_inputs(project.path());
    // Warm the page cache so iterations measure parsing, not cold reads.
    extract_manifest(&root, &config).expect("warm up");
    c.bench_function(name, |b| {
        b.iter(|| extract_manifest(black_box(&root), black_box(&config)).expect("extract"));
    });
}

fn bench_manifest(c: &mut Criterion) {
    bench_extract(
        c,
        "extract_manifest/pyproject_200deps",
        &support::synth_pyproject_project(200),
    );
    bench_extract(
        c,
        "extract_manifest/requirements_2000lines",
        &support::synth_requirements_project(2_000),
    );
    bench_extract(
        c,
        "extract_manifest/setup_py_300deps",
        &support::synth_setup_py_project(300),
    );
    bench_extract(
        c,
        "extract_manifest/setup_cfg_1000deps",
        &support::synth_setup_cfg_project(1_000),
    );
}

fn bench_root_and_config(c: &mut Criterion) {
    let project = support::synth_src_project(10);
    let start = project.path().join("src/bench_acme");

    c.bench_function("discover_project_root/depth_2", |b| {
        b.iter(|| discover_project_root(black_box(&start)).expect("discover root"));
    });

    let root = discover_project_root(project.path()).expect("discover root");
    c.bench_function("load_config/pyproject_only", |b| {
        b.iter(|| load_config(black_box(&root)).expect("load config"));
    });
}

criterion_group!(benches, bench_manifest, bench_root_and_config);
criterion_main!(benches);
