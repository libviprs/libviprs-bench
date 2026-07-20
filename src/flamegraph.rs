//! Generates time-weighted flame graphs for all three libviprs engines.
//!
//! Drives each engine on a 4096x4096 image under a [`CollectingObserver`], then
//! folds the captured [`EngineEvent`](libviprs::EngineEvent) stream into
//! inferno's `stack <weight>` format via
//! [`libviprs_bench::flame::events_to_folded_stacks`] and renders an SVG per
//! engine. Frame WIDTH is TIME: each tile is weighted by the wall-clock gap since
//! the previous tile (in microseconds), not by a flat sample count, so a level or
//! tile that took longer draws wider.
//!
//! Run: cargo run --release --bin flamegraph
//!
//! Writes, into the `report/` directory:
//!   report/flamegraph_monolithic.svg
//!   report/flamegraph_streaming.svg
//!   report/flamegraph_mapreduce.svg

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::time::Instant;

use inferno::flamegraph::{self, Options};
use libviprs::streaming::BudgetPolicy;
use libviprs::{
    CollectingObserver, EngineBuilder, EngineConfig, EngineEvent, EngineKind, Layout, MemorySink,
    PyramidPlanner, RasterStripSource,
};
use libviprs_bench::flame::{FLAMEGRAPH_COUNT_NAME, events_to_folded_stacks};
use libviprs_bench::{gradient_raster, streaming_budget_for};
use std::sync::Arc;

const WIDTH: u32 = 4096;
const HEIGHT: u32 = 4096;
const TILE_SIZE: u32 = 256;

fn generate_flamegraph(stacks: &[String], output_path: &Path, title: &str) {
    if stacks.is_empty() {
        // inferno's `from_reader` errors on empty input, which the `.expect`
        // below would turn into a panic. A zero-tile run (no `TileCompleted`
        // events) legitimately folds to no frames — skip the SVG with a notice
        // rather than abort. Latent for the fixed 4096² workload (which always
        // produces tiles), but #25 dropped the marker frames that used to
        // guarantee non-empty output, so guard the regression (#25 review).
        eprintln!("  (no tiles produced — skipping {})", output_path.display());
        return;
    }

    let mut opts = Options::default();
    opts.title = title.to_string();
    // The sample unit is microseconds of wall time (see the flame module), not a
    // count of events, so inferno's hover/legend reads in time.
    opts.count_name = FLAMEGRAPH_COUNT_NAME.to_string();
    opts.min_width = 0.1;

    let file = File::create(output_path).expect("create flamegraph file");
    let writer = BufWriter::new(file);

    let lines: Vec<&str> = stacks.iter().map(|s| s.as_str()).collect();
    let reader = lines.join("\n");

    flamegraph::from_reader(&mut opts, reader.as_bytes(), writer).expect("generate flamegraph");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    fs::create_dir_all(&report_dir).unwrap();

    let src = gradient_raster(WIDTH, HEIGHT);
    let planner = PyramidPlanner::new(WIDTH, HEIGHT, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();

    // Size the streaming/mapreduce budget to admit the worst-case tile-aligned
    // strip at this 4096-wide canvas: a flat 1 MB trips `BudgetPolicy::Error`'s
    // `BudgetExceeded` up front (worst-case strip = 4096·512·3 = 6.29 MB) and
    // the engine `.run()?` below would surface it as an error — the exact
    // failure mode issue #38 sizes around. RGB8 gradient, so bpp = 3; shared
    // with the report/scalability paths.
    let budget = streaming_budget_for(1_000_000, plan.canvas_width, TILE_SIZE, 3);

    println!("Image: {WIDTH}x{HEIGHT}, tile_size={TILE_SIZE}");
    println!(
        "Plan: {} levels, {} tiles",
        plan.level_count(),
        plan.total_tile_count()
    );
    println!();

    // --- Monolithic ---
    {
        let sink = MemorySink::new();
        let observer = Arc::new(CollectingObserver::new());

        let start = Instant::now();
        let result = EngineBuilder::new(&src, plan.clone(), &sink)
            .with_engine(EngineKind::Monolithic)
            .with_config(EngineConfig::default())
            .with_observer_arc(observer.clone())
            .run()?;
        let elapsed = start.elapsed();

        println!(
            "Monolithic: {:.1} ms, {:.2} MB peak, {} tiles",
            elapsed.as_secs_f64() * 1000.0,
            result.peak_memory_bytes as f64 / (1024.0 * 1024.0),
            result.tiles_produced,
        );

        let stacks = events_to_folded_stacks(&observer.events(), "monolithic");
        let path = report_dir.join("flamegraph_monolithic.svg");
        generate_flamegraph(
            &stacks,
            &path,
            "libviprs monolithic engine — time-weighted flame graph \
             (per-tile width = µs; serial engine, faithful per tile)",
        );
        println!("  → {}", path.display());
    }

    // --- Streaming ---
    {
        let sink = MemorySink::new();
        let observer = Arc::new(CollectingObserver::new());

        let strip_src = RasterStripSource::new(&src);
        let start = Instant::now();
        let result = EngineBuilder::new(strip_src, plan.clone(), &sink)
            .with_engine(EngineKind::Streaming)
            .with_config(EngineConfig::default())
            .with_memory_budget(budget)
            .with_budget_policy(BudgetPolicy::Error)
            .with_observer_arc(observer.clone())
            .run()?;
        let elapsed = start.elapsed();

        let strip_count = observer
            .events()
            .iter()
            .filter(|e| matches!(e, EngineEvent::StripRendered { .. }))
            .count();

        println!(
            "Streaming:  {:.1} ms, {:.2} MB peak, {} tiles, {} strips",
            elapsed.as_secs_f64() * 1000.0,
            result.peak_memory_bytes as f64 / (1024.0 * 1024.0),
            result.tiles_produced,
            strip_count,
        );

        let stacks = events_to_folded_stacks(&observer.events(), "streaming");
        let path = report_dir.join("flamegraph_streaming.svg");
        generate_flamegraph(
            &stacks,
            &path,
            "libviprs streaming engine — time-weighted flame graph \
             (width = µs; read at level/root — per-tile widths are strip-emission cadence)",
        );
        println!("  → {}", path.display());
    }

    // --- MapReduce ---
    {
        let sink = MemorySink::new();
        let observer = Arc::new(CollectingObserver::new());

        let strip_src = RasterStripSource::new(&src);
        let start = Instant::now();
        let result = EngineBuilder::new(strip_src, plan.clone(), &sink)
            .with_engine(EngineKind::MapReduce)
            .with_config(EngineConfig::default().with_concurrency(0))
            .with_memory_budget(budget)
            .with_budget_policy(BudgetPolicy::Error)
            .with_observer_arc(observer.clone())
            .run()?;
        let elapsed = start.elapsed();

        let events = observer.events();
        let strip_count = events
            .iter()
            .filter(|e| matches!(e, EngineEvent::StripRendered { .. }))
            .count();
        let batch_count = events
            .iter()
            .filter(|e| matches!(e, EngineEvent::BatchStarted { .. }))
            .count();

        println!(
            "MapReduce:  {:.1} ms, {:.2} MB peak, {} tiles, {} strips, {} batches",
            elapsed.as_secs_f64() * 1000.0,
            result.peak_memory_bytes as f64 / (1024.0 * 1024.0),
            result.tiles_produced,
            strip_count,
            batch_count,
        );

        let stacks = events_to_folded_stacks(&events, "mapreduce");
        let path = report_dir.join("flamegraph_mapreduce.svg");
        generate_flamegraph(
            &stacks,
            &path,
            "libviprs MapReduce engine — time-weighted flame graph \
             (width = µs; read at level/root — per-tile widths are emission cadence)",
        );
        println!("  → {}", path.display());
    }

    println!();
    println!("Flame graphs written to {}", report_dir.display());

    Ok(())
}
