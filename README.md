<p align="center">
  <img src="https://raw.githubusercontent.com/libviprs/libviprs/main/images/libviprs-logo-claws.svg" alt="libviprs" width="200">
</p>

<h1 align="center">libviprs-bench</h1>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.97%2B-orange?logo=rust" alt="Rust 1.97+">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License">
</p>

Benchmark harness for [libviprs](../libviprs): how its three pyramid engines compare to each other, and how that moves across releases. A comparison against [libvips](https://www.libvips.org/) `dzsave` is kept too, behind the `libvips` cargo feature.

This crate is kept in a separate repository so the library crate stays free of heavy benchmark-only dependencies (criterion, libvips FFI).

> Flag reference and runnable Rust examples for every knob the benchmarks
> tune live at <https://libviprs.org/cli/>.

## What this is

`libviprs-bench` measures how libviprs scales as image size and concurrency change. Every run measures one **family**, and the family is what the runner, the report and the charts are keyed on.

| Family | What it measures | Needs |
|---|---|---|
| `engines` | monolithic vs streaming vs mapreduce | nothing: no cargo features, no libvips anywhere. **The default.** |
| `storage` | PMTiles vs a directory tree | its own `storage` binary and its own `storage` image stage. No cargo features, no libvips. Takes `--storage-profile` rather than a command |
| `vips` | the libvips `dzsave` comparison | `--features libvips`, and the pinned libvips the Docker image builds |

libviprs-only is the default build and the default output. The comparison is a
second question this harness can also answer, not the frame the first one hangs
off: a run of `engines` measures the same three engines whether or not the
machine it runs on has libvips installed, because the engine set is a function
of the family and of nothing else.

Each family writes into its own `report/<family>/` directory, so two families
can never append to each other's history or overwrite each other's charts. A
snapshot records the family it came out of, and appending one family's snapshot
to another's history is refused.

To keep the cross-engine comparison apples-to-apples, every engine writes its tiles as PNG files to a real on-disk sink under the same DeepZoom layout, so neither side gets an in-RAM-sink or tile-codec advantage. That holds for the libviprs engines and for libvips `dzsave` alike, and `tests/encoding_claim.rs` proves it from a real run and then holds this sentence to it.

The harness produces:

- **Wall time** per engine, per image size
- **Memory** in two clearly separated columns — an engine-tracked working set (a per-run, libviprs-internal figure; `0` for libvips, which exposes no equivalent) and **peak RSS** (the OS-level high-water mark, measured the same way for every engine). Cross-engine comparison uses the common RSS basis; the two are never conflated.
- **Throughput** (tiles/second) and **memory efficiency** (tiles/second per RSS-MB)
- **Resource cost** (RSS-MB-seconds per tile) — useful for comparing engines in environments where memory and CPU time are both billed
- **SVG charts** of all of the above, plus version-history trend lines across releases
- **Flame graphs** built from engine observer events
- **Criterion** statistical reports with violin plots

## Engines under test

| Engine | Source kind | Description |
|---|---|---|
| **Monolithic** | in-memory `Raster` | Decodes the full canvas, downscales level-by-level. Highest peak memory, fastest at small sizes. |
| **Streaming** | strip source | Sequential strip pipeline bounded by a memory budget. Memory scales with strip width, not image area. |
| **MapReduce** | strip source | Parallel strip pipeline. Same strip-bounded model as streaming, with `K` in-flight strips trading memory for throughput. |
| **libvips** | in-memory `VipsImage` | External baseline via `dzsave`, writing PNG tiles to the same on-disk sink as the libviprs engines. Either spawned as a CLI or, with the `libvips` feature, called in-process through FFI. **`vips` family only** — no other family will measure it, installed or not. |

| Bench file | Targets |
|---|---|
| `benches/engine_comparison.rs` | Criterion micro-benchmarks for monolithic / streaming / MapReduce, plus a head-to-head group across image sizes. |
| `src/scalability.rs` (`scalability` bin) | Scalability sweep from 0.2 MP to 280 MP over the family's engines, on a 1.42:1 aspect ratio matching `43551_California_South.pdf`. Takes `--family`. |
| `src/report.rs` (`report` bin) | Full matrix across image sizes and concurrency levels for the family's engines, with versioned history. Takes `--family`. |
| `src/flamegraph.rs` (`flamegraph` bin) | Time-weighted flame graphs (frame width = µs) for all three libviprs engines on a 4096x4096 image. Per-tile widths are faithful for the serial monolithic engine; for the strip-based streaming/MapReduce engines read them at level/root granularity (per-tile widths are emission cadence). |

## Running benchmarks

```bash
# Scalability sweep of the engines family (the default) — writes
# report/engines/scalability_results.json, then renders the SVGs from it
./run-bench.sh

# Full matrix for the engines family — writes benchmark_results.json +
# benchmark_history.json under report/engines/, then renders the charts
./run-bench.sh report

# The libvips comparison. Builds the pinned libvips stage and runs with
# --features libvips; everything lands under report/vips/
./run-bench.sh report --family vips

# Force architecture
./run-bench.sh --arch arm
./run-bench.sh --arch amd64

# Container memory limit (MB, default 4096)
./run-bench.sh --memory 2048

# Run locally without Docker (--family vips additionally needs libvips-dev + pkg-config)
./run-bench.sh --no-build
```

The binaries take the same flag directly, and a family they cannot run is refused with a non-zero exit rather than measured as something else:

```bash
cargo run --release --bin report                              # engines, no features
cargo run --release --features libvips --bin report -- --family vips
cargo run --release --bin report -- --family vips             # refused: names the feature
cargo run --release --bin report -- --help                    # lists the families
```

Output is written to `report/<family>/`. Each run of the `report` command appends an entry to that family's `benchmark_history.json`; once two or more entries exist, the trend charts (`chart_history_*.svg`) showing wall time and peak memory across versions are rendered from it.

### Charts

Every SVG — the grouped-bar comparison charts (`chart_*.svg`), the history-trend charts (`chart_history_*.svg`), and the scalability charts (`scalability_*.svg`) — is rendered by a small JS chart library (`tools/charts/`), not by the Rust binaries. `run-bench.sh` invokes it automatically after the harness writes its JSON, so a normal run refreshes the SVGs for you. This requires **Node** (any recent LTS) on the host; if Node is missing the run still completes and leaves the JSON, printing the command to render later.

Running a binary directly (`cargo run --bin scalability` / `--bin report`) emits **JSON only**. To (re)render the SVGs from JSON already on disk:

```bash
node tools/charts/render.mjs --report-dir report/engines            # log-log scalability axes (default)
node tools/charts/render.mjs --report-dir report/engines --linear   # linear scalability axes
node tools/charts/render.mjs --report-dir report/engines --zoom 20  # + large-image scalability_*_zoom.svg (>= 20 MP)
```

`render.mjs` is deterministic (same JSON → byte-identical SVGs) and idempotent — it re-renders the whole report from whatever JSON is present. Point `--report-dir` at a family directory (`report/engines`, `report/vips`) to draw that family. Its own tests run with `node --test '*.test.mjs'` from `tools/charts/` (or `npm test` there).

The grouped-bar comparison charts read `benchmark_results.json`, the history trends read `benchmark_history.json`, and the scalability charts read `scalability_results.json`. A committed golden set under `tools/charts/fixtures/` mirrors the exact serde shape the Rust serializers emit; `render.mjs`'s shape probes plus the Rust `tests/chart_shape_drift.rs` guard catch producer/consumer field drift between the two.

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

- `monolithic` — single-thread vs `EngineConfig::default.with_concurrency(4)`.
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

### `pdfium_strip_source_bench` (`src/pdfium_strip_source_bench.rs`, `pdfium` feature)

Compares the two `PdfiumStripSource` constructors — cached (`new`) versus
streaming (`new_streaming`) — over a `(dpi, strip_count)` sweep on a PDF,
emitting one newline-delimited JSON record per `(mode, dpi, strips)` triple. The
numbers back the "vector-heavy PDFs scale ~linearly with N, raster-heavy
approach 1×" doc comment on `PdfiumStripSource::new_streaming` with data instead
of assertion. Gated behind the `pdfium` feature.

| `pdfium_strip_source_bench` flag | Meaning |
|----------------------------------|--------------------------------------------------------------------------|
| `--pdf <path>` | PDF to bench (default: the committed `fixtures/cc_licenses_mapping.pdf`) |
| `--page <N>` | 1-based page index |
| `--dpis 72,150,300` | comma-separated render DPIs to sweep |
| `--strip-counts 4,16,64` | comma-separated strip counts to sweep |
| `--output <file.jsonl>` | write JSONL here instead of stdout |

```bash
cargo run --release --features pdfium --bin pdfium_strip_source_bench -- \
    --pdf /path/to/blueprint.pdf --dpis 72,150,300 --strip-counts 4,16,64
```

## Docker

`run-bench.sh` builds the Docker image the family needs, then runs the chosen binary inside it with a memory limit. This is the recommended path because it isolates the host from the benchmark, and for the `vips` family it also pins the libvips version.

There are two stages, because the families need different machines:

| Stage | For | What is in it |
|---|---|---|
| `--target engines` | `engines` | Rust and the two crates. No libvips headers, no `vips` binary, no cargo features. Builds in a fraction of the time. |
| `--target storage` | `storage` | The same, plus only what the storage sweep needs. This is the stage `run-bench.sh --family storage` selects; pointing the storage family at `engines` builds a binary that stage does not contain. |
| default target | `vips` | the above plus libvips compiled from a pinned upstream source tarball, plus PDFium |

The absence in the first one is the point: a family that measures three libviprs engines must not be able to find a fourth.

You can also drive Docker directly:

```bash
# From the workspace root (parent of libviprs/ and libviprs-bench/)
docker build --target engines -f libviprs-bench/Dockerfile -t libviprs-bench:engines .

# Default: scalability binary over the engines family
docker run --rm --memory=4096m \
    -v "$(pwd)/libviprs-bench/report:/src/libviprs-bench/report" \
    libviprs-bench:engines

# The libvips comparison
docker build -f libviprs-bench/Dockerfile -t libviprs-bench .
docker run --rm --memory=4096m \
    -v "$(pwd)/libviprs-bench/report:/src/libviprs-bench/report" \
    libviprs-bench \
    cargo run --release --features libvips --bin report -- --family vips
```

The `report/` directory is mounted into the container so charts persist after it exits.

## The `storage` family

PMTiles against a directory tree, the comparison the PMTiles work was actually
for. It is libviprs only: no libvips, no cargo features, its own binary and its
own image stage.

```bash
./run-bench.sh --family storage                          # the ci profile
./run-bench.sh --family storage --storage-profile full   # the publishable one
cargo run --release --bin storage -- --profile ci --out report/storage/storage-results.json
```

`--storage-profile` takes `ci`, `full` or `xl`, and it replaces the command
argument the other families take rather than adding to it.

| profile | what it is for |
|---|---|
| `ci` | proves the harness runs. One small cell, seconds. **Never published**, and the binary says so on stderr. |
| `full` | the publishable sweep: every cell, both backends, every scenario. |
| `xl` | `full` plus the largest cells, for a machine that has the time. |

The output is one JSON document per sweep rather than a chart set, because the
family's claim is a comparison between two backends on identical work and the
page draws it from an archived history rather than from the last run.

## The archive, and what refuses a run

A benchmark number is worth keeping only if the document carrying it can say
what produced it. `storage-aggregate` is the door:

```bash
cargo run --release --bin storage-aggregate -- --provenance          # before a sweep
cargo run --release --bin storage-aggregate -- --check   run.json
cargo run --release --bin storage-aggregate -- --archive run.json
cargo run --release --bin storage-aggregate -- --verify  archive/storage/<runId>.json
```

Exit 0 means admissible or verified, 1 means refused, 2 means the invocation was
wrong. A refusal is a 1 rather than a 2 because it is an answer, not a failure to
run.

`--provenance` is the one to run first: everything it warns about is also a
refusal, and learning it after forty minutes of measuring is the expensive way to
find out.

It refuses, rather than reports with a footnote. A run measured under emulation,
from a tree with no commit, on a filesystem the document does not name, or with a
cell nobody observed, does not get averaged in with a caveat: it does not get in.
The full refusal table, the four digests, the canonicalisation rules the digests
depend on, and the three ordinary ways a commit comes back null are in
[`archive/storage/README.md`](archive/storage/README.md).

Two refusals can be cleared and neither by making the problem go away.
`--allow-dirty` records the dirt and stamps it onto every cell; a profile that
declares tmpfs records that the run means to measure RAM.

## Cargo features

| Feature | Default | Description |
|---|---|---|
| `libvips` | off | Enables in-process libvips FFI via `libvips-rs`, and with it the `vips` family. Without it the `vips` family is refused, naming the feature, rather than run empty. |
| `pdfium` | off | The rasterized-PDF workload (`pdfium_strip_source_bench`, and the `streaming-pdf` scalability series). |
| `polars` | off | The `cross_version` columnar analysis binary. |

The default feature set is empty, and that is load-bearing: the `engines` family builds, runs and charts with no features at all, so `libvips-rs` is never in the normal dependency graph and the cheap CI cell never needs libvips installed.

## Output layout

One directory per family, identical inside:

```
report/
├── engines/                 # the default family
│   ├── scalability_wall_time_c<n>.svg      # one set per thread budget (#156)
│   ├── scalability_peak_memory_c<n>.svg
│   ├── scalability_throughput_c<n>.svg
│   ├── scalability_efficiency_c<n>.svg
│   ├── scalability_resource_cost_c<n>.svg
│   ├── scalability_results.json
│   ├── chart_wall_time.svg
│   ├── chart_peak_memory.svg
│   ├── chart_tracked_memory.svg
│   ├── chart_throughput.svg
│   ├── chart_efficiency.svg
│   ├── chart_resource_cost.svg
│   ├── chart_history_<size>_c<n>_time.svg
│   ├── chart_history_<size>_c<n>_memory.svg
│   ├── benchmark_results.json
│   ├── benchmark_history.json
│   ├── comparison_table.txt
│   └── verdict_table.txt
├── vips/                    # same shape, plus the libvips row and the PSNR spot-check
├── storage/                 # the storage family: one document per sweep, not charts
│   └── storage-results.json
└── flamegraph_{monolithic,streaming,mapreduce}.svg
```

An archived storage run lives outside `report/`, because `report/` is output a
re-run overwrites and an archive is not:

```
archive/storage/
├── index.json                     # one row per archived run, sorted by id
└── <runId>.json                   # the sealed document

```

## Requirements

- Rust 1.97+ (edition 2024) — the floor the measured `libviprs` core declares
- Docker (recommended, used by `run-bench.sh`)
- For `--no-build`: `libvips-dev` and `pkg-config` on the host
- **Node** (recent LTS) on the host to render the history/scalability SVGs (`tools/charts/render.mjs`). Optional for a benchmark run — without it the run still writes its JSON — but **required to run the test suite**, because `tests/engines_family_end_to_end.rs` asserts the charts are actually drawn and a chart assertion that quietly does not run is the same colour as one that passed

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
