<p align="center">
  <img src="https://raw.githubusercontent.com/libviprs/libviprs/main/images/libviprs-logo-claws.svg" alt="libviprs" width="200">
</p>

<h1 align="center">libviprs-bench</h1>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust" alt="Rust 1.85+">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License">
</p>

Benchmark harness for [libviprs](../libviprs), comparing its three pyramid engines against [libvips](https://www.libvips.org/) `dzsave` under identical inputs.

This crate is kept in a separate repository so the library crate stays free of heavy benchmark-only dependencies (criterion, plotters, libvips FFI).

> Flag reference and runnable Rust examples for every knob the benchmarks
> tune live at <https://libviprs.org/cli/>.

## What this is

`libviprs-bench` measures how libviprs scales as image size and concurrency change, and how it compares to libvips on the same workload. Both libraries are linked into a single process so neither side gets a filesystem-I/O advantage.

The harness produces:

- **Wall time** and **peak memory** per engine, per image size
- **Throughput** (tiles/second) and **memory efficiency** (tiles/second per MB)
- **Resource cost** (MB-seconds per tile) — useful for comparing engines in environments where memory and CPU time are both billed
- **SVG charts** of all of the above, plus version-history trend lines across releases
- **Flame graphs** built from engine observer events
- **Criterion** statistical reports with violin plots

## Engines under test

| Engine | Source kind | Description |
|---|---|---|
| **Monolithic** | in-memory `Raster` | Decodes the full canvas, downscales level-by-level. Highest peak memory, fastest at small sizes. |
| **Streaming** | strip source | Sequential strip pipeline bounded by a memory budget. Memory scales with strip width, not image area. |
| **MapReduce** | strip source | Parallel strip pipeline. Same strip-bounded model as streaming, with `K` in-flight strips trading memory for throughput. |
| **libvips** | PNG / raw memory | External baseline via `dzsave`. Either spawned as a CLI or, with the `libvips` feature, called in-process through FFI. |

| Bench file | Targets |
|---|---|
| `benches/engine_comparison.rs` | Criterion micro-benchmarks for monolithic / streaming / MapReduce, plus a head-to-head group across image sizes. |
| `src/scalability.rs` (`scalability` bin) | Scalability sweep from 0.2 MP to 47 MP, comparing all four engines on a 1.42:1 aspect ratio matching `43551_California_South.pdf`. |
| `src/report.rs` (`report` bin) | Full comparison matrix across image sizes and concurrency levels, with versioned history. |
| `src/flamegraph.rs` (`flamegraph` bin) | Event-based flame graphs for all three libviprs engines on a 4096x4096 image. |

## Running benchmarks

```bash
# Scalability sweep (default) — writes report/scalability_*.svg + scalability_results.json
./run-bench.sh

# Full comparison matrix — writes report/chart_*.svg + benchmark_results.json + benchmark_history.json
./run-bench.sh report

# Force architecture
./run-bench.sh --arch arm
./run-bench.sh --arch amd64

# Container memory limit (MB, default 4096)
./run-bench.sh --memory 2048

# Run locally without Docker (requires libvips-dev + pkg-config installed)
./run-bench.sh --no-build
```

Output is written to `report/`. Each run of the `report` command appends an entry to `benchmark_history.json`; once two or more entries exist, the binary also produces trend charts (`chart_history_*.svg`) showing wall time and peak memory across versions.

The criterion micro-benchmarks are run separately:

```bash
cargo bench
open target/criterion/report/index.html
```

Flame graphs are produced by their own binary:

```bash
cargo run --release --bin flamegraph
# writes report/flamegraph_{monolithic,streaming,mapreduce}.svg
```

## Benchmark scenarios and the flags they exercise

Each scenario tunes a knob the [libviprs CLI](https://libviprs.org/cli/)
also exposes. The links below jump straight to the flag picker entry — it
includes a description, defaults, and a runnable Rust snippet.

### Scalability sweep (`src/scalability.rs`)

Sweeps a gradient raster from 512x360 to 8192x5760 and runs all four
engines at each size. Three knobs are pinned per run:

- **Streaming budget** — `STREAMING_BUDGET = 4_000_000` bytes is fed to
  both the streaming and MapReduce engines, forcing strip-bounded
  behaviour. Equivalent CLI flag:
  [`--memory-budget`](https://libviprs.org/cli/#flag-memory-budget).
- **MapReduce in-flight strips** — `tile_concurrency = 4`. Equivalent CLI
  flag: [`--concurrency`](https://libviprs.org/cli/#flag-concurrency).
- **Tile size** — `TILE_SIZE = 256`. The CLI exposes the same
  [pyramid](https://libviprs.org/cli/#pyramid) layout/tile knobs.

The Docker memory cap on `run-bench.sh --memory <MB>` corresponds to the
process-level [`--memory-limit`](https://libviprs.org/cli/#flag-memory-limit)
in the CLI: the engine's own
[`--memory-budget`](https://libviprs.org/cli/#flag-memory-budget) must be
chosen to fit beneath it.

### Criterion micro-benchmarks (`benches/engine_comparison.rs`)

Four benchmark groups, each parameterised on image size:

- `monolithic` — single-thread vs `EngineConfig::default().with_concurrency(4)`.
  See [`--parallel`](https://libviprs.org/cli/#flag-parallel) /
  [`--concurrency`](https://libviprs.org/cli/#flag-concurrency).
- `streaming` — single-thread vs 4-thread, both at 1 MB
  [`--memory-budget`](https://libviprs.org/cli/#flag-memory-budget).
- `mapreduce` — `tile_concurrency = 0` vs `4`, exercising
  [`--concurrency`](https://libviprs.org/cli/#flag-concurrency) under the
  same 1 MB budget.
- `head_to_head` — monolithic, streaming, mapreduce, and `mapreduce_4t` at
  matched sizes, isolating the
  [`--memory-budget`](https://libviprs.org/cli/#flag-memory-budget) /
  [`--concurrency`](https://libviprs.org/cli/#flag-concurrency) tradeoff.

### `pdf_engine_bench` (`src/pdf_engine_bench.rs`)

Renders one PDF page through one engine and writes a JSON report. The CLI
flags it accepts mirror the libviprs CLI:

| `pdf_engine_bench` flag | CLI equivalent                                              |
|-------------------------|-------------------------------------------------------------|
| `--engine mono\|stream` | engine selection (see CLI flag picker)                      |
| `--budget-bytes`        | [`--memory-budget`](https://libviprs.org/cli/#flag-memory-budget) |
| `--tile-size`           | [pyramid layout](https://libviprs.org/cli/#pyramid)         |
| `--dpi`, `--page`, `--layout` | matched 1:1 in the CLI                                |

Streaming runs additionally honour an internal strip-buffer sizing knob
analogous to [`--buffer-size`](https://libviprs.org/cli/#flag-buffer-size);
the bench fixes it via `compute_strip_height` against the budget so the
two engines see byte-identical input dimensions.

## Docker

`run-bench.sh` builds a Docker image with libvips, PDFium, and both crates side-by-side, then runs the chosen binary inside it with a memory limit. This is the recommended path because it pins the libvips version and isolates the host from the benchmark.

You can also drive Docker directly:

```bash
# From the workspace root (parent of libviprs/ and libviprs-bench/)
docker build -f libviprs-bench/Dockerfile -t libviprs-bench .

# Default: scalability binary
docker run --rm --memory=4096m \
    -v "$(pwd)/libviprs-bench/report:/src/libviprs-bench/report" \
    libviprs-bench

# Run the full report binary
docker run --rm --memory=4096m \
    -v "$(pwd)/libviprs-bench/report:/src/libviprs-bench/report" \
    libviprs-bench \
    cargo run --release --features libvips --bin report
```

The `report/` directory is mounted into the container so charts persist after it exits.

## Cargo features

| Feature | Default | Description |
|---|---|---|
| `libvips` | off | Enables in-process libvips FFI via `libvips-rs`. Without it, libvips is invoked through the `vips` CLI when present. |

## Output layout

```
report/
├── scalability_wall_time.svg
├── scalability_peak_memory.svg
├── scalability_throughput.svg
├── scalability_efficiency.svg
├── scalability_resource_cost.svg
├── scalability_results.json
├── chart_wall_time.svg
├── chart_peak_memory.svg
├── chart_throughput.svg
├── chart_efficiency.svg
├── chart_resource_cost.svg
├── chart_history_<size>_c<n>_time.svg
├── chart_history_<size>_c<n>_memory.svg
├── benchmark_results.json
├── benchmark_history.json
├── comparison_table.txt
└── flamegraph_{monolithic,streaming,mapreduce}.svg
```

## Requirements

- Rust 1.85+ (edition 2024)
- Docker (recommended, used by `run-bench.sh`)
- For `--no-build`: `libvips-dev` and `pkg-config` on the host

## See also

- [libviprs CLI flag reference](https://libviprs.org/cli/) — every knob
  the benchmarks tune, with defaults and runnable Rust examples.
- [`#pyramid`](https://libviprs.org/cli/#pyramid) — tile size, layout,
  level count.
- [`#flag-memory-budget`](https://libviprs.org/cli/#flag-memory-budget) —
  engine-level budget driving strip height.
- [`#flag-memory-limit`](https://libviprs.org/cli/#flag-memory-limit) —
  process-level cap; what `run-bench.sh --memory` enforces via Docker.
- [`#flag-parallel`](https://libviprs.org/cli/#flag-parallel) /
  [`#flag-concurrency`](https://libviprs.org/cli/#flag-concurrency) —
  thread-count knobs measured by the Criterion groups.
- [`#flag-buffer-size`](https://libviprs.org/cli/#flag-buffer-size) —
  streaming strip buffer.

## Related Crates

| Crate | Description |
|---|---|
| [libviprs](../libviprs) | The pyramid engine being benchmarked |
| [libviprs-cli](../libviprs-cli) | Command-line interface (`viprs` binary) |
| [libviprs-tests](../libviprs-tests) | Integration tests and fixtures |
