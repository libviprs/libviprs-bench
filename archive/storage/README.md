# The storage archive

Every number the libviprs benchmark page draws comes from a file in this
directory, and a file gets in here only if the run it describes can say what
produced it. Nothing is charted, quoted or compared that is not archived by
digest.

That rule exists because of a specific artefact. `docs/pmtiles-benchmarks.md` in
the engine repository publishes 480 lines of numbers and records no platform at
all. The issue that produced them describes its own run as being in "the amd64
Linux container"; the machine is Apple Silicon, `DOCKER_DEFAULT_PLATFORM` there
is `linux/amd64` and Docker Desktop has Rosetta on, so those numbers were almost
certainly instruction-translated. Nothing in the artefact can settle it. The
silence is the defect, not the emulation, and everything here is built to make
that particular silence impossible.

## What is in here

```
archive/storage/
  index.json                      one row per archived run
  <runId>.json                    the sealed document
```

`index.json` is a sorted array of `{runId, documentDigest, startedAt,
libraryCommit, emulated, fsType, file}`. It is an index and not a source of
truth: every field in it is also in the document it points at, and the document
is the thing the digests cover.

## The run id

```
<startedAt>-<library commit>-<host8>
20260913T214500Z-0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6-1a2b3c4d
```

`startedAt` is the document's own, with the punctuation and any fractional
seconds stripped so the id is a legal filename everywhere and stable against a
producer that starts or stops printing milliseconds. `host8` is the first eight
hex characters of a sha256 over `os`, `arch`, `cpuModel`, `rustc`, the scratch
filesystem type and the emulation verdict, NUL-separated, which is the set of
things that decide whether two runs are comparable at all.

Nothing in the id reads the clock. With `SystemTime::now()` in it, archiving one
document twice would file it twice, a re-run of the archiver would silently
double a series, and the page would draw one measurement as two points on a
trend line.

## The four digests

A sealed document carries `integrity: {cells, runners, measurements, document}`,
each `sha256:` over the canonical JSON of its block. `document` covers the whole
document with `integrity` and `combinedAt` removed, so sealing does not change
what was sealed.

Four rather than one so that `--verify` can say *what* moved. A single
whole-file hash tells a reader something changed and leaves them to diff 480
lines of numbers; these four turn that into "the cells moved and the runner did
not", which is the difference between a re-measured sweep and a tampered-with
one.

## Canonicalisation

The digests must reproduce, byte for byte, what causl's JavaScript produces over
the same document (`causl-bench/tools/suite/regression-gate.mjs`,
`canonicalJson`). Four rules, three of which are places where a Rust port
silently disagrees:

1. **Key order.** Object keys ascending, arrays untouched. JavaScript sorts by
   UTF-16 code unit and Rust by UTF-8 byte, and those orders differ:
   `"\u{10000}"` is the surrogate pair `D800 DC00`, so JavaScript puts it before
   `"\u{FFFD}"` and Rust puts it after. They agree on ASCII, so a non-ASCII key
   is **refused** rather than silently digested two ways.
2. **Absent is not null.** An absent key is not emitted; an explicit `null` is
   emitted as `null`, and the two are different documents with different
   digests. The consequence for a producer is hard and load-bearing: **no
   `#[serde(skip_serializing_if)]` anywhere in a storage document**.
3. **Numbers.** A value with no fractional part prints as a plain decimal
   integer whatever Rust type it arrived in, because `JSON.stringify(1.0)` is
   `"1"` and `serde_json` writes `1.0`. `-0.0` prints as `0`. Everything else
   prints as the shortest decimal that round-trips, which is what both languages
   produce *in plain notation*; outside that range JavaScript writes `1e+21` and
   `1e-7` where Rust writes the digits in full, so any value at or above `1e21`
   or strictly below `1e-6` is **refused**, as is any integer beyond `2^53-1`
   and anything non-finite.
4. **Strings.** RFC 8259 escaping: `\"`, `\\`, the five two-character control
   escapes, `\u00xx` for the remaining C0 controls, everything else literal.

A digest is `sha256:` followed by 64 lowercase hex characters over the UTF-8
bytes of the canonical string.

## What gets refused

`storage-aggregate --check <document.json>` reports every reason, never the
first, because a sweep that was measured under emulation on a dirty tree with a
debug build has three problems and finding them one re-run at a time turns a
forty-minute sweep into an afternoon.

| code | what it means |
|---|---|
| `emulated` | `provenance.emulated` is not exactly `false`. `true`, `"unknown"`, `null` and absent are all refused: an unobserved run is not a native one, and "absent" is the state the published PMTiles numbers are in |
| `commit` | either tree has no commit, or an empty one |
| `dirty` | either tree has no dirty flag. A missing flag is not a clean tree |
| `dirty-not-allowed` | a tree is dirty and `allowDirty` is not set |
| `dirty-not-stamped` | `allowDirty` is set but some cell does not carry `dirty: true`, so a reader quoting one cell would not know |
| `debug-build` | debug assertions are on, or the profile is not `release` |
| `perturbing-rustflags` | `RUSTFLAGS` carries `-C instrument-coverage` or a `-Z` flag, which changes the code being timed |
| `filesystem` | no `fsType`, or no `scratchDir` to attach it to |
| `tmpfs` | the scratch directory is on tmpfs and the profile does not declare it |
| `invocation` | no `argv`, `command`, `cwd` or `resolved` |
| `scenarios-unresolved` | `resolved.scenarios` still says `"all"`, which means a different set on every day the suite grows |
| `reps-disagree` | some cell's `reps` is not the number the invocation resolved to |
| `no-cells` | an empty reading is a refusal, not a result |
| `unattested-cell` | a cell claims `outcome: ok` without `storageAttested: true` |
| `outcome-without-reason` | a non-`ok` cell with no reason |
| `integrity` | the document carries digests that no longer hold |

Two refusals can be cleared, and neither of them by making the problem go away.
`--allow-dirty` records the dirt and forces it onto every cell. A profile that
declares tmpfs records that the run means to measure RAM. Everything else is a
refusal with no flag behind it.

## Attestation is observed, never asserted

A cell may only claim `storageAttested: true` on two observations taken in the
measuring process:

- the archive's root directory is walked and its entries classified, and the
  regime that is *seen* is compared against the regime the cell *declares*. A
  root full of tile entries with no leaf directories is the `root` regime, one
  directory read per lookup; a root full of pointers to leaf directories is the
  `leaf` regime, two reads per lookup, and the regime the leaf cache exists for.
  An empty root directory is an unobserved archive, not a small one;
- a seeded sample of at least 64 coordinates is read back from both backends and
  compared byte for byte. Two backends that disagree about what a tile contains
  are not two measurements of one workload.

Neither observation reads the cell's own label. That is the mistake the whole
mechanism exists to prevent.

## The emulation probe

`provenance.emulated` is `true`, `false` or `"unknown"`, with
`emulationEvidence[]` naming every source consulted and what each one saw. The
sources, and what they are actually worth as measured on an Apple Silicon Mac
with Docker Desktop:

| source | under `--platform linux/amd64` | under `--platform linux/arm64` |
|---|---|---|
| `/proc/self/maps` | carries `/run/rosetta/rosetta` | clean |
| `/proc/sys/fs/binfmt_misc` | present and **empty** | present and empty |
| `BENCH_DAEMON_ARCH` vs binary arch | `arm64` against `x86_64` | `arm64` against `aarch64` |

So `binfmt_misc` is blind inside a Docker Desktop container and never returns a
native verdict. `/proc/self/maps` is the primary evidence and is not blind:
Rosetta maps its translator into every process it translates and the path
survives even though `/run/rosetta` cannot be opened through the container's
mount namespace. The runner-supplied daemon architecture is the fallback for a
host where `/proc` says nothing.

A probe that always answered `false` would pass every Rust unit test anyone
could write for it, so the evidence is a shell script:

```
./tools/probe-emulation.sh control
```

It compiles the probe with bare `rustc` under both platforms on one machine and
asserts opposite answers. Run it whenever the probe changes.

## When the commit comes back null

Three ordinary environments produce `commit: null` or a spurious `dirty: true`,
and none of them is the harness's fault. All three are fixed from the
environment, which is why the build script detects each one and says which it is
rather than swallowing it:

1. **A linked git worktree bind-mounted into a container.** Its `.git` is a
   *file* holding `gitdir: <path>` that points outside the mount, so git answers
   "not a git repository". Mount the real gitdir at the same absolute path.
2. **A tree that trips `safe.directory`**, which is what a share read as root
   looks like: every git call exits 128 with "dubious ownership". The build
   script passes `-c safe.directory=<abs path>` for the one directory it is
   asking about.
3. **A share that forces mode 0777**, so every file reads as a mode change and a
   tree nobody edited comes back dirty. The build script runs `git status` both
   with and without file-mode comparison and reports the difference, rather than
   quietly masking it.

## Running it

```
cargo run --release --bin storage-aggregate -- --provenance     # before a sweep
cargo run --release --bin storage-aggregate -- --check   run.json
cargo run --release --bin storage-aggregate -- --archive run.json
cargo run --release --bin storage-aggregate -- --verify  archive/storage/<runId>.json
```

Exit 0 means admissible or verified, 1 means refused, 2 means the invocation was
wrong. A refusal is a 1 and not a 2 because it is an answer, not a failure to
run.

`--provenance` is the one to run first. Everything it warns about is also a
refusal, and learning it after forty minutes of measuring is the expensive way.
