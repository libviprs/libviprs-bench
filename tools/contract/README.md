# The frozen contract

causl's dashboard, importer and renderer, vendored at a pinned revision, plus the config that
carries every libviprs difference. Nothing libviprs-specific is edited into a frozen file.

The reason for the pattern rather than a port is already in this repo once. `tools/charts/chart.mjs`
started as causl's `chart.ts` and is no longer one, so the two cannot be reconciled and a fix to
either does not reach the other.

```
UPSTREAM_REV            cd65c76
UPSTREAM.manifest       one `<blob sha1>  <name>` line per frozen file, as `git ls-tree`
                        reported it at the pin
upstream/               the nine frozen files, byte for byte
sync-contract.sh        --check / --write
config.json             every libviprs difference
config.schema.json      the schema, with prose on every field
parameterize.mjs        frozen dashboard.js + anchored edits -> generated/dashboard.js
generated/dashboard.js  what the page loads. Do not edit.
golden/                 the committed render, and the sha256 of each render the tests produce
lib/                    the resolver, the DOM shim, the render harness, the blob hasher
fixtures/               a history and a vocabulary, both derived from one archived run
```

## Running it

Everything runs in a container, arm64, never the host toolchain:

```
docker run --rm --platform linux/arm64 -v "$PWD":/repo -w /repo/tools/contract \
  node:22-bookworm-slim npm run gate
```

`npm run gate` is the sync check, the parameteriser's staleness check, the 34 contract tests and
causl's own 23 tests running against the vendored copy. `npm run regen` rebuilds `generated/` and
the goldens after a deliberate change.

`sync-contract.sh --check` runs two modes and at least one of them always runs. Manifest mode
re-hashes every frozen file and needs neither git nor a clone, so it works in CI and in the
container, where the causl repositories (private Gitea) are unreachable. Clone mode re-derives each
blob sha1 from a local causl-org clone, so a manifest doctored to match a doctored frozen file still
fails. A missing clone downgrades to manifest-only and says so; an unreadable manifest refuses with
exit 3 rather than reporting green.

## Why the dashboard is generated rather than edited

Every hard-coded constant in `upstream/dashboard.js` becomes `__cfg('<path>', <the same literal>)`.
With no config present every read returns the literal that is still sitting in the frozen file, so
causl's own page is unchanged. That is not an argument, it is a test: `golden-render.test.mjs`
renders both files over causl's sample history, causl's real history and the 404 fallback path and
requires the HTML to match byte for byte, and it carries a positive control that fails if loading
the config changes nothing.

Each of the 18 edits is anchored on an exact string and must match exactly once. Upstream moving one
of those lines is a refusal, not a patch landing somewhere else.

The config reaches the browser as a synchronous global, because the constants are evaluated the
moment the IIFE runs:

```html
<script>window.LIBVIPRS_BENCH_CONFIG = { ... }</script>
<script src="./dashboard.js"></script>
```

## Moving the pin

```
./sync-contract.sh --write --upstream ../../../causl/causl-org   # after editing UPSTREAM_REV
node parameterize.mjs --write
REGEN_GOLDEN=1 node --test golden-render.test.mjs
npm test
```

If an anchor moved, the parameteriser refuses and names the edit. Re-anchor it against the new
revision rather than loosening the match.

## What is in config.json and why

Read `config.schema.json` for the field-by-field version. Five entries are decisions rather than
transcription, each checked against the archived run
`20260914T145707Z-809ee8014d002518ce55edaceba698ca7a8b8a79-0bc00939` rather than guessed:

**`sections.scaleFrom` is `scale` only because `source` is in the scenario.** Tile count is not
unique across cells: `21851` is both `8192x8192@64+gradient` and `8192x8192@64+noise`. A section
keyed on tile count alone draws two different sources as one line and nothing on the page says so.

**`producer.outcomes` has three buckets, not two.** Upstream refuses a whole import if any cell's
outcome is `failed`, and seven cells in the real capture are `failed` for a structural reason (`a
directory tree has no root directory to decode`). Under upstream's rule that capture cannot be
imported at all. They go in `structural` and import as skips carrying their reason.

**`verdict.noise.onMissingSpread` is `unknown`, not `skip`.** A spread that was not measured is not
a spread, so the page refuses to rule rather than falling back to the thresholds. There is
deliberately no median-spread fallback: using 3.46% on the metric whose real spread is 53.3% gets
the dangerous answer confidently.

**`verdict.gate.field` triggers only on the literal `false`.** No cell in the archived capture
carries a `gated` field at all, so `producer.gatedFrom` is `null` and the importer writes `null`.
"Nobody said" and "not gated" are different facts.

**`samples.carry` exists because of a trap.** `groupSections` builds a fixed-shape chart point and
that object is the only thing `computeVerdict` ever sees, so a verdict rule reading a sample field
reads `undefined` until the field is carried across, which looks exactly like a rule switched off.

## Known gap

The frozen dashboard has no notion of metric direction. Ten of the thirty metric keys are
`higher-is-better`, and for those a cell that got faster renders as `regressed` because its number
went up. `sections.directionFrom` carries the fact and nothing at this pin reads it. There is a test
pinning the current behaviour so the gap turns up here rather than on the published page.
