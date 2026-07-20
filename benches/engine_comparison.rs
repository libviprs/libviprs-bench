//! Criterion benchmarks comparing monolithic vs streaming vs MapReduce
//! pyramid engines.
//!
//! Measures wall-clock time with statistical analysis across image sizes
//! and concurrency levels. HTML reports with violin plots are generated
//! in `target/criterion/`.
//!
//! Run: cargo bench
//! View: open target/criterion/report/index.html
//!
//! Note: these criterion micro-benchmarks time the in-process engines with
//! an in-memory sink; they complement — and are separate from — the
//! cross-engine `report`/`scalability` harness, which child-isolates every
//! cell and writes real PNG tiles for a fair libvips comparison.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use libviprs::streaming::BudgetPolicy;
use libviprs::{
    EngineBuilder, EngineConfig, EngineKind, Layout, MemorySink, PyramidPlanner, RasterStripSource,
};
use libviprs_bench::gradient_raster;

const TILE_SIZE: u32 = 256;
const STREAMING_BUDGET: u64 = 1_000_000; // 1 MB

fn bench_monolithic_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("monolithic");
    group.sample_size(10);

    for &(w, h) in &[(512, 512), (1024, 1024), (2048, 2048), (4096, 4096)] {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

        for (name, conc) in [("single_thread", 0usize), ("4_threads", 4)] {
            group.bench_with_input(
                BenchmarkId::new(name, format!("{w}x{h}")),
                &(w, h),
                |b, _| {
                    b.iter(|| {
                        let sink = MemorySink::new();
                        EngineBuilder::new(&src, plan.clone(), &sink)
                            .with_engine(EngineKind::Monolithic)
                            .with_config(EngineConfig::default().with_concurrency(conc))
                            .run()
                            .unwrap();
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_streaming_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming");
    group.sample_size(10);

    for &(w, h) in &[(512, 512), (1024, 1024), (2048, 2048), (4096, 4096)] {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

        for (name, conc) in [("single_thread", 0usize), ("4_threads", 4)] {
            group.bench_with_input(
                BenchmarkId::new(name, format!("{w}x{h}")),
                &(w, h),
                |b, _| {
                    b.iter(|| {
                        let sink = MemorySink::new();
                        let strip_src = RasterStripSource::new(&src);
                        EngineBuilder::new(strip_src, plan.clone(), &sink)
                            .with_engine(EngineKind::Streaming)
                            .with_config(EngineConfig::default().with_concurrency(conc))
                            .with_memory_budget(STREAMING_BUDGET)
                            .with_budget_policy(BudgetPolicy::Error)
                            .run()
                            .unwrap();
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_mapreduce_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("mapreduce");
    group.sample_size(10);

    for &(w, h) in &[(512, 512), (1024, 1024), (2048, 2048), (4096, 4096)] {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

        for (name, conc) in [("single_thread", 0usize), ("4_threads", 4)] {
            group.bench_with_input(
                BenchmarkId::new(name, format!("{w}x{h}")),
                &(w, h),
                |b, _| {
                    b.iter(|| {
                        let sink = MemorySink::new();
                        let strip_src = RasterStripSource::new(&src);
                        EngineBuilder::new(strip_src, plan.clone(), &sink)
                            .with_engine(EngineKind::MapReduce)
                            .with_config(EngineConfig::default().with_concurrency(conc))
                            .with_memory_budget(STREAMING_BUDGET)
                            .with_budget_policy(BudgetPolicy::Error)
                            .run()
                            .unwrap();
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_head_to_head(c: &mut Criterion) {
    let mut group = c.benchmark_group("head_to_head");
    group.sample_size(10);

    for &(w, h) in &[(1024, 1024), (2048, 2048), (4096, 4096)] {
        let src = gradient_raster(w, h);
        let planner = PyramidPlanner::new(w, h, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
        let plan = planner.plan();

        group.bench_with_input(
            BenchmarkId::new("monolithic", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    EngineBuilder::new(&src, plan.clone(), &sink)
                        .with_engine(EngineKind::Monolithic)
                        .with_config(EngineConfig::default())
                        .run()
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("streaming", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let strip_src = RasterStripSource::new(&src);
                    EngineBuilder::new(strip_src, plan.clone(), &sink)
                        .with_engine(EngineKind::Streaming)
                        .with_config(EngineConfig::default())
                        .with_memory_budget(STREAMING_BUDGET)
                        .with_budget_policy(BudgetPolicy::Error)
                        .run()
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("mapreduce", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let strip_src = RasterStripSource::new(&src);
                    EngineBuilder::new(strip_src, plan.clone(), &sink)
                        .with_engine(EngineKind::MapReduce)
                        .with_config(EngineConfig::default())
                        .with_memory_budget(STREAMING_BUDGET)
                        .with_budget_policy(BudgetPolicy::Error)
                        .run()
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("mapreduce_4t", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let strip_src = RasterStripSource::new(&src);
                    EngineBuilder::new(strip_src, plan.clone(), &sink)
                        .with_engine(EngineKind::MapReduce)
                        .with_config(EngineConfig::default().with_concurrency(4))
                        .with_memory_budget(STREAMING_BUDGET)
                        .with_budget_policy(BudgetPolicy::Error)
                        .run()
                        .unwrap();
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_monolithic_engine,
    bench_streaming_engine,
    bench_mapreduce_engine,
    bench_head_to_head
);
criterion_main!(benches);
