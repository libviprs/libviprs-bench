//! Generates flame graphs for monolithic and streaming engines.
//!
//! Uses tracing-based self-instrumentation and the inferno crate to produce
//! SVG flame graphs in the `report/` directory.
//!
//! Run: cargo run --bin flamegraph
//!
//! This binary profiles both engines on a 4096x4096 image and writes:
//!   report/flamegraph_monolithic.svg
//!   report/flamegraph_streaming.svg

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::time::Instant;

use inferno::flamegraph::{self, Options};
use libviprs::{
    CollectingObserver, EngineConfig, EngineEvent, Layout, MapReduceConfig, MemorySink,
    PyramidPlanner, RasterStripSource, StreamingConfig, generate_pyramid_mapreduce,
    generate_pyramid_observed, generate_pyramid_streaming,
};
use libviprs_bench::gradient_raster;

const WIDTH: u32 = 4096;
const HEIGHT: u32 = 4096;
const TILE_SIZE: u32 = 256;

/// Collect a folded-stack trace from engine observer events.
///
/// Converts EngineEvent sequences into a folded stack format that inferno
/// can render as a flame graph. Each event becomes a stack frame with its
/// duration encoded as the sample count.
fn events_to_folded_stacks(events: &[EngineEvent], engine_name: &str) -> Vec<String> {
    let mut stacks = Vec::new();
    let mut current_level: Option<u32> = None;
    let mut _level_tiles: u64 = 0;

    for event in events {
        match event {
            EngineEvent::LevelStarted { level, tile_count, .. } => {
                current_level = Some(*level);
                _level_tiles = 0;
                stacks.push(format!(
                    "{engine_name};level_{level}_start {tile_count}"
                ));
            }
            EngineEvent::TileCompleted { coord } => {
                _level_tiles += 1;
                if let Some(lvl) = current_level {
                    stacks.push(format!(
                        "{engine_name};level_{lvl};tile_r{}_c{} 1",
                        coord.row, coord.col
                    ));
                }
            }
            EngineEvent::LevelCompleted { level, tiles_produced } => {
                stacks.push(format!(
                    "{engine_name};level_{level}_complete {tiles_produced}"
                ));
            }
            EngineEvent::StripRendered { strip_index, total_strips } => {
                stacks.push(format!(
                    "{engine_name};strip_{strip_index}_of_{total_strips} 1"
                ));
            }
            EngineEvent::BatchStarted { batch_index, strips_in_batch, .. } => {
                stacks.push(format!(
                    "{engine_name};batch_{batch_index}_{strips_in_batch}strips 1"
                ));
            }
            EngineEvent::BatchCompleted { batch_index, tiles_produced } => {
                stacks.push(format!(
                    "{engine_name};batch_{batch_index}_done_{tiles_produced}tiles 1"
                ));
            }
            EngineEvent::Finished { total_tiles, levels } => {
                stacks.push(format!(
                    "{engine_name};finished_{levels}levels_{total_tiles}tiles 1"
                ));
            }
            _ => {}
        }
    }

    stacks
}

fn generate_flamegraph(stacks: &[String], output_path: &Path, title: &str) {
    let mut opts = Options::default();
    opts.title = title.to_string();
    opts.count_name = "events".to_string();
    opts.min_width = 0.1;

    let file = File::create(output_path).expect("create flamegraph file");
    let writer = BufWriter::new(file);

    let lines: Vec<&str> = stacks.iter().map(|s| s.as_str()).collect();
    let reader = lines.join("\n");

    flamegraph::from_reader(
        &mut opts,
        reader.as_bytes(),
        writer,
    )
    .expect("generate flamegraph");
}

fn main() {
    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("report");
    fs::create_dir_all(&report_dir).unwrap();

    let src = gradient_raster(WIDTH, HEIGHT);
    let planner =
        PyramidPlanner::new(WIDTH, HEIGHT, TILE_SIZE, 0, Layout::DeepZoom).unwrap();
    let plan = planner.plan();

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
        let observer = CollectingObserver::new();
        let config = EngineConfig::default();

        let start = Instant::now();
        let result = generate_pyramid_observed(&src, &plan, &sink, &config, &observer).unwrap();
        let elapsed = start.elapsed();

        println!(
            "Monolithic: {:.1} ms, {:.2} MB peak, {} tiles",
            elapsed.as_secs_f64() * 1000.0,
            result.peak_memory_bytes as f64 / (1024.0 * 1024.0),
            result.tiles_produced,
        );

        let stacks = events_to_folded_stacks(&observer.events(), "monolithic");
        let path = report_dir.join("flamegraph_monolithic.svg");
        generate_flamegraph(&stacks, &path, "libviprs monolithic engine — event flame graph");
        println!("  → {}", path.display());
    }

    // --- Streaming ---
    {
        let sink = MemorySink::new();
        let observer = CollectingObserver::new();
        let config = StreamingConfig {
            memory_budget_bytes: 1_000_000,
            engine: EngineConfig::default(),
        };

        let strip_src = RasterStripSource::new(&src);
        let start = Instant::now();
        let result =
            generate_pyramid_streaming(&strip_src, &plan, &sink, &config, &observer).unwrap();
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
        generate_flamegraph(&stacks, &path, "libviprs streaming engine — event flame graph");
        println!("  → {}", path.display());
    }

    // --- MapReduce ---
    {
        let sink = MemorySink::new();
        let observer = CollectingObserver::new();
        let config = MapReduceConfig {
            memory_budget_bytes: 1_000_000,
            tile_concurrency: 0,
            ..MapReduceConfig::default()
        };

        let strip_src = RasterStripSource::new(&src);
        let start = Instant::now();
        let result =
            generate_pyramid_mapreduce(&strip_src, &plan, &sink, &config, &observer).unwrap();
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
        generate_flamegraph(&stacks, &path, "libviprs MapReduce engine — event flame graph");
        println!("  → {}", path.display());
    }

    println!();
    println!("Flame graphs written to {}", report_dir.display());
}
