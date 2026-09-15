# The engines archive

Archived `engines` runs: monolithic against streaming against mapreduce, one
sealed document per sweep.

```
archive/engines/
  index.json                      one row per archived run
  <runId>.json                    the sealed document
```

Everything about how a run gets in here — the run id, the four digests, the
canonicalisation rules they depend on, the full refusal table, and the three
ordinary ways a commit comes back null — is in
[`../storage/README.md`](../storage/README.md) and is **identical** for both
families. There is one `storage-aggregate`, one `admit`, one `seal` and one set
of rules; this file only records the two things that are this family's own.

## Why this is a separate directory

Both families derive a run id from the same fields: the document's `startedAt`,
the measured library's commit, and a hash over the environment that decides
whether two runs are comparable. None of those is the family. So a `storage` and
an `engines` sweep started in the same second against the same commit on the
same host derive the **same id**, and in one directory the second would be
refused as a collision with the first — a true statement about ids and a useless
one about the runs.

Putting the family into the id would be the other fix and it is the wrong one:
the id is what a page keys an era on, and a family is not part of an era.

## What a cell in here is

A `(engine, canvas, thread budget)` cell, published six times over: `wall`,
`peak_rss_mb`, `tracked_memory_mb`, `tiles_per_second`,
`tiles_per_second_per_mb` and `resource_cost`. Each row carries every timed
repetition in `samples[]` and computes its median, IQR, bootstrap interval and
`confidence` from them.

`peak_rss_mb` here is a **per-run** peak. Every repetition of every engine is a
fresh child process and the parent takes that child's `ru_maxrss` through
`wait4`, so the watermark is scoped to one engine's own address space. The sweep
this replaces ran all three engines in one process against
`getrusage(RUSAGE_SELF)`, a monotonic process-wide high-water mark, and published
byte-identical peak RSS for all three engines in twenty of twenty groups.

## The invariants, and the one that is not

`tiles_produced`, `output_bytes`, `filesystem_entries` and `directories` are
exact. They reproduce across repetitions and agree across all three engines,
because three engines walking one plan through one PNG codec write the same
bytes. They are published as invariants with an equality verdict, and a cell
whose repetitions disagree about one of them is refused with the field named
rather than averaged: within one commit that is a defect, not a delta.

`allocated_bytes` is measured and not published. It is `st_blocks * 512`, so it
answers to the filesystem's allocator rather than to the engine. It reproduced
in every observation this family has taken so far, and the `storage` family's
first full capture caught it moving between two repetitions of one generation
and refused those two cells. A claim that a block size can falsify is not a
claim about the engine.

## Attestation

A cell may carry `attested: true` only when the engine it names was observed to
have written a pyramid, to have written the same one every repetition, and to
agree with at least one other engine in the same cell about the per-level tile
grid and the tile count. The evidence is a walk of the real sink directory, done
in the child that ran the engine.

One engine measured alone is never attested. It has nothing to agree with, and
the whole reason the grid is worth checking is that three engines walking one
plan must produce the same tiles.
