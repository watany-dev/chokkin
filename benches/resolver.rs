//! Fixed cost of bundled and configured resolver maps.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
use chokkin::config::default_config;
use chokkin::resolver::{ImportMap, VenvIndex, build_binary_map};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_maps(c: &mut Criterion) {
    let config = default_config();
    let venv = VenvIndex::default();
    let mut group = c.benchmark_group("resolver_maps");
    group.bench_function("imports", |b| {
        b.iter(|| ImportMap::build(black_box(&config)));
    });
    group.bench_function("binaries", |b| {
        b.iter(|| build_binary_map(black_box(&config), &venv));
    });
    group.finish();
}
criterion_group!(benches, bench_maps);
criterion_main!(benches);
