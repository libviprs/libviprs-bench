//! Criterion benchmarks comparing monolithic vs streaming pyramid engines.
//!
//! Measures wall-clock time with statistical analysis across image sizes
//! and concurrency levels. HTML reports with violin plots are generated
//! in `target/criterion/`.
//!
//! Run: cargo bench
//! View: open target/criterion/report/index.html

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use libviprs::{
    EngineConfig, Layout, MapReduceConfig, MemorySink, PyramidPlanner, RasterStripSource,
    StreamingConfig, generate_pyramid, generate_pyramid_mapreduce, generate_pyramid_streaming,
    observe::NoopObserver,
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

        group.bench_with_input(
            BenchmarkId::new("single_thread", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    generate_pyramid(&src, &plan, &sink, &EngineConfig::default()).unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("4_threads", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = EngineConfig::default().with_concurrency(4);
                    generate_pyramid(&src, &plan, &sink, &config).unwrap();
                });
            },
        );
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

        group.bench_with_input(
            BenchmarkId::new("single_thread", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = StreamingConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        engine: EngineConfig::default(),
                        budget_policy: libviprs::streaming::BudgetPolicy::Error,
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_streaming(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
                    .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("4_threads", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = StreamingConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        engine: EngineConfig::default().with_concurrency(4),
                        budget_policy: libviprs::streaming::BudgetPolicy::Error,
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_streaming(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
                    .unwrap();
                });
            },
        );
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

        group.bench_with_input(
            BenchmarkId::new("single_thread", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = MapReduceConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        tile_concurrency: 0,
                        ..MapReduceConfig::default()
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_mapreduce(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
                    .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("4_threads", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = MapReduceConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        tile_concurrency: 4,
                        ..MapReduceConfig::default()
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_mapreduce(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
                    .unwrap();
                });
            },
        );
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
                    generate_pyramid(&src, &plan, &sink, &EngineConfig::default()).unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("streaming", format!("{w}x{h}")),
            &(w, h),
            |b, _| {
                b.iter(|| {
                    let sink = MemorySink::new();
                    let config = StreamingConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        engine: EngineConfig::default(),
                        budget_policy: libviprs::streaming::BudgetPolicy::Error,
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_streaming(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
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
                    let config = MapReduceConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        tile_concurrency: 0,
                        ..MapReduceConfig::default()
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_mapreduce(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
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
                    let config = MapReduceConfig {
                        memory_budget_bytes: STREAMING_BUDGET,
                        tile_concurrency: 4,
                        ..MapReduceConfig::default()
                    };
                    let strip_src = RasterStripSource::new(&src);
                    generate_pyramid_mapreduce(
                        &strip_src,
                        &plan,
                        &sink,
                        &config,
                        &NoopObserver,
                    )
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
