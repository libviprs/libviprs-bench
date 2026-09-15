/* ============================================================
   Series-id vocabulary.
   ------------------------------------------------------------
   Every chart element this renderer emits is tagged with the
   series it represents, and a series id is exactly the runner
   directory name `causl-bench` published the cell under. There
   is no derivation step and no profile or repo axis to collapse:
   `history.json` carries the id, this file draws it.

   ONE causl-wasm series exists, and it took some deleting to get
   there. Three wasm-labelled series were once charted here:

     causl-wasm      withdrawn, DELETED
     causl-wasm-all  withdrawn, DELETED
     causl-wasm-ts   attested, RENAMED to causl-wasm

   The first two were the same TypeScript engine measured under a
   wasm label. `causl-bench/docs/benchmark.md` records that their
   engine label was asserted rather than observed, and their
   generator was deleted in causl-bench#110. Deleting the
   generator stops new numbers; it does not retract the ones this
   feed already serves, so those rows were removed from
   `history.json` outright. They were not relabelled: relabelling
   them would have attached a Rust-engine claim to a
   TypeScript-engine measurement, which is the original defect
   with a fresh coat of paint.

   The surviving series is the one whose every cell carries a
   runtime attestation that the Rust engine executed the graph
   that was timed. It measures the `@causl/causl-wasm-ts` client
   on the `causl-core-rs` engine, and it is charted under the id
   `causl-wasm` because that is what it always claimed to be.
   Note the id is `causl-wasm` while the package is
   `@causl/causl-wasm-ts`; the two are not interchangeable.

   Each emission writes both `data-series` and `data-library`
   with the same value. `data-series` is the load-bearing seam;
   `data-library` is kept because external tooling pinned to it
   before the rename and costs nothing to mirror.
   ============================================================ */

/* ============================================================
   causl.org/benchmarks: full dashboard renderer (#707, #769,
   restructured by #1247).

   Pure-vanilla JS, no dependencies. Reads `./history.json` (an
   array of HistoryEntry objects that `import-run.mjs` writes from
   an archived `causl-bench` sweep) and renders ONE inline-SVG
   chart per (scenario × scale) section, with each of the six
   series stacked as a separate line on the same axes for direct
   visual comparison.

   Each chart shows:
     - median (ms)  → solid library-coloured line per framework,
       smoothed with a Catmull-Rom-style cubic interpolation, plus
       dots at every datapoint
     - p95   (ms)   → translucent library-coloured band per
       framework (median ↔ p95) with the same smoothing applied
     - per-line verdict badges in the inline legend strip:
       pass / regressed / improved / noisy; verdict computed per
       framework by comparing the latest entry to the second-to-
       latest and by the CoV proxy on the latest entry
     - hover tooltips: a thin invisible SVG <rect> covers each
       datapoint (per series) and exposes a tooltip with the run's
       date, version, median, p95 so the user can hover any run on
       any series, not just the latest, and read its values
     - profile-artifact links per framework in the legend chips
       (placeholder until #709's nightly publish lands)

   --------------------------------------------------------------
   Why one chart per section, not one per (library × scenario × scale)?

   #1247 reframed the dashboard from a "cell grid" (one card per
   series) to a "section grid" (one card per scenario + scale,
   every series stacked). The cell grid made it hard to spot e.g.
   "causl is 5x faster than redux-toolkit at scale 10k on
   linear-chain" because the panels lived next to each other on
   different y-axes. Stacking the series on one chart with a
   shared y-axis solves that immediately.

   The original per-cell verdict logic is preserved per-series:
   each framework gets its own verdict, surfaced as a coloured
   chip in the section's legend strip. The per-cell border tint
   that used to advertise verdict has been retired (a single
   section can have mixed verdicts across its series, so it no
   longer makes sense to tint the whole card by one verdict).

   D3 / Plotly decision (#769) still stands. STAY ON VANILLA SVG:

     1. Lighthouse Performance budget. Lazy-loading any extra
        third-party JS is non-zero TBT (Total Blocking Time) and
        non-zero LCP-after-hydration. A static page whose entire
        purpose is to render N small charts is exactly the case
        where shipping a charting framework would be net-negative
        on Lighthouse.
     2. The features the issue wanted (cubic smoothing, "nice"
        axis ticks, hover tooltips, multi-series rendering) are
        80 to 120 lines of vanilla code each, not a 70 KB dependency.
     3. No build step on causl.org/. The site is a flat directory
        served as static files. Adding a bundler just to import
        d3-shape + d3-scale + d3-axis would be a much larger
        architectural change than the issue scopes.
     4. Plotly is not a serious option: 3.5 MB minified for a
        per-section chart is two orders of magnitude over budget.
   --------------------------------------------------------------

   Filters at the top let the user toggle which libraries +
   scenarios + scales appear. The library filter now hides
   individual series within a section (rather than hiding whole
   cards). The default view is the headline "causl engines + best
   competitor at the 10k scale": causl-ts + causl-wasm + mobx at
   scale 10000, the cells the regression-gate watches most closely.
   Toggling re-renders the grid in place.

   The script first attempts ./history.json; if that 404s it falls
   back to ./history.sample.json (a checked-in sample so the page
   is never blank). The fallback is announced in the meta strip.
   ============================================================ */

(() => {
  'use strict'

  const HISTORY_URL = './history.json'
  const SAMPLE_URL = './history.sample.json'

  /** Canonical series order. Six ids, one per runner directory the
   *  `causl-bench` sweeps publish, with the two causl engines first so
   *  the eye finds them before the comparators.
   *
   *  `causl-ts` is the TypeScript engine (`@causlts/core`). `causl-wasm`
   *  is the `@causl/causl-wasm-ts` client on the Rust `causl-core-rs`
   *  engine, and every one of its cells carries a runtime attestation
   *  that Rust executed the graph that was timed. It is the ONLY wasm
   *  series: the earlier `causl-wasm` and `causl-wasm-all` series were
   *  the TypeScript engine under a wasm label and their rows are deleted
   *  from `history.json`, not relabelled.
   *
   *  `redux-rtk` and `redux-toolkit` are the same library under two
   *  runner ids, and they stay separate because the two harnesses
   *  measure a different unit (the current one times a world built fresh
   *  per step, in its own process), so one line through both would not
   *  be continuous. */
  const LIBRARY_ORDER = [
    'causl-ts',
    'causl-wasm',
    'jotai',
    'redux-toolkit',
    'redux-rtk',
    'mobx',
  ]

  /** Pretty legend labels, applied via getLibraryLabel(). A series id
   *  is what `history.json` carries and what `data-series` emits, so an
   *  override here changes only what the legend prints. Most ids read
   *  fine as-is; `redux-rtk` needs one because "redux-rtk" and
   *  "redux-toolkit" are indistinguishable to a reader who does not
   *  already know they are two runners over one library. */
  const LIBRARY_LABEL = {
    'redux-rtk': 'redux-toolkit (rtk runner)',
  }
  function getLibraryLabel(lib) {
    return LIBRARY_LABEL[lib] ?? lib
  }

  /** Per-series annotation surfaced as a tooltip on the legend chip.
   *  Empty today: the entries that lived here flagged the two withdrawn
   *  wasm series, and those series are deleted rather than annotated. A
   *  caveat that has to be attached to a series to keep it honest is a
   *  reason to stop charting the series. The map stays because the
   *  legend renderer reads it, and the next series that genuinely needs
   *  a footnote goes here. */
  const LIBRARY_ANNOTATION = {}

  /** Library colours: competitor identity colours, NOT state
   *  semantics. The brand palette in css/site.css reserves Async
   *  Cyan / Commit Green / Conflict Amber / Rollback Red for
   *  state meaning (link / success / warning / error). If we
   *  paint a competitor with rollback-red, every chart silently
   *  broadcasts "competitor = failure", which confuses the
   *  graph's literal message (which is a per-cell ratio of
   *  wall-times, not a verdict on the library). Per #1259 review
   *  T2.2, competitor swatches are pulled from the brand's
   *  *secondary* identity colours (Copper Wire / Mutation Violet
   *  / Trace Ash) so the state-semantic colours stay free for
   *  the verdict pill, the regression badge, and the legend dots.
   *  Causl keeps Async Cyan since the brand owns the chip.
   *
   *  Contrast against the dashboard surface (≈ #11182A): every one
   *  passes WCAG 4.5:1 by inspection on `#070A0F` (the spec floor).
   */
  const LIBRARY_COLOR = {
    'causl-ts': '#11D9FF',     // Async Cyan: brand owner (TS engine).
    'causl-wasm': '#5EE6A8',   // Commit-mint: the attested Rust engine,
                               //   distinct from causl-ts so the two
                               //   causl axes read as separate series.
    jotai: '#C8743D',          // Copper Wire: neutral identity.
    'redux-toolkit': '#7C4DFF', // Mutation Violet: neutral identity.
    'redux-rtk': '#7C4DFF',    // Same library, same colour; the dashed stroke
                               //   below is what distinguishes the runner.
    mobx: '#8FA2AA',           // Trace Ash: neutral identity.
  }

  /** Per-library SVG `stroke-dasharray`: absent = solid line. Only
   *  `redux-rtk` is dashed, so the two redux runners share a colour
   *  without sharing a stroke. Both causl series draw solid. */
  const LIBRARY_DASH = {
    'redux-rtk': '6 3',
  }

  /** Default-view filter: both causl engines plus the best competitor
   *  (`mobx`) at the 10k scale. The wasm engine's bars are much larger
   *  than the rest and the adjacent callout exists to explain exactly
   *  that, so the bars MUST be on screen for the callout to do its job.
   *
   *  Users can still toggle any series via the filter UI: nothing is
   *  hidden, only un-defaulted. */
  const DEFAULT_LIBRARIES = new Set([
    'causl-ts',
    'causl-wasm',
    'mobx',
  ])
  const DEFAULT_SCALES = new Set([10000])

  /** Verdict thresholds: match the spec in #707. */
  const REGRESSION_PCT = 0.10 // > 10% slower
  const IMPROVED_PCT = 0.10 // > 10% faster
  const PASS_PCT = 0.05 // within 5%
  /** True CoV is samples-stdev / mean. We only have median + p95, so
   *  use `(p95 - median) / median` as a (much wider) dispersion proxy.
   *  For a roughly normal distribution `(p95 - μ) / μ ≈ 1.645 × CoV`,
   *  so a 5% true CoV would correspond to ≈ 8% on this proxy. To
   *  keep the badge from firing on every slightly-noisy cell, we
   *  set the noisy gate at 50% (proxy), which roughly maps to a
   *  true CoV in the 30%+ range, the band where the cell really
   *  isn't trustable. The spec's 5%-CoV criterion can't be applied
   *  literally without per-iteration sample arrays in HistorySample. */
  const NOISY_PROXY = 0.50

  /** Verdict palette: high-contrast against the card background. */
  const VERDICT_COLOR = {
    pass: '#5EE6A8', // success-mint
    regressed: '#FF646E', // conflict-coral
    improved: '#4F7CFF', // signal-blue
    noisy: '#FFB347', // commit-amber
    unknown: '#A9B5C9', // fog
  }

  /** ----------------------------------------------------------------
   *  Skip-reason classification (#1304, task 18).
   *
   *  Companion to the #1302 section-level "Library limitations" text
   *  block: the dashboard chart itself renders an in-place .skip-box
   *  for every (scenario × library × scale) cell the runner couldn't
   *  measure, so an adopter scanning a chart for "where is library X?"
   *  gets an immediate, in-context answer instead of a silent gap.
   *
   *  `classifySkip(reason)` collapses the runner's free-form reason
   *  string into a 1-3-word taxonomy label + a CSS class. The lookup
   *  is intentionally small (3 buckets + a catch-all) so the badge
   *  reads as a label, not a sentence. The matchers are case-
   *  insensitive substrings keyed on the phrasing the retired
   *  expansion-stub harness emitted, plus the V8 stack-overflow
   *  message. That harness is gone: the current sweeps go through the
   *  typed pipeline below, and this classifier survives only for the
   *  pre-2026-07 entries in `history.json` whose reasons are still
   *  free-form. New reasons fall through to the `library limit`
   *  catch-all so the box still renders, just with the generic label.
   *  ---------------------------------------------------------------- */
  const SKIP_TAXONOMY = [
    {
      match: (r) => /stack/i.test(r) || /v8 call stack/i.test(r),
      label: 'stack overflow',
      cls: 'stack-overflow',
    },
    {
      // Match the exact phrase "wasm-only" (word-boundary) instead of
      // co-occurring substrings. The looser predicate
      // (`/wasm/i && /boundary|only/i`) false-positived on reasons
      // like "floor-only … crosses the WASM boundary" (see #11) and
      // mislabelled causl-ts skips as "WASM-only". The typed-skip
      // pipeline (`TYPED_SKIP_META`) is the preferred path; this
      // legacy classifier remains only for back-compat with
      // un-migrated free-form reasons.
      match: (r) => /\bwasm-only\b/i.test(r),
      label: 'WASM-only',
      cls: 'wasm-only',
    },
    {
      match: (r) =>
        /not architecturally meaningful/i.test(r) ||
        /not expressible/i.test(r) ||
        /public-api gap/i.test(r) ||
        /acceptance gate/i.test(r),
      label: 'API not expressible',
      cls: 'api-gap',
    },
  ]

  function classifySkip(reason) {
    const r = String(reason ?? '')
    for (const bucket of SKIP_TAXONOMY) {
      if (bucket.match(r)) return { label: bucket.label, cls: bucket.cls }
    }
    return { label: 'library limit', cls: 'library-limit' }
  }

  /** ----------------------------------------------------------------
   *  Typed skip taxonomy (causl-bench#32, persisted by #37).
   *
   *  After causl-bench#37, every SKIP cell in `history.json`'s new
   *  `skipped[]` array carries a structured `reason` drawn from a
   *  fixed enum (the `SKIP_REASONS` set the `causl-bench` runners
   *  share) PLUS a `status: 'SKIP'` discriminator and a
   *  free-form `detail` string. The dashboard renders these as typed
   *  chips with per-reason colour drawn from the existing brand
   *  palette (no new colours introduced):
   *
   *    artefact-missing     → fog          (muted warm gray)
   *                           "wasm bridge artefact wasn't built":
   *                           rebuild + the cell resumes.
   *    feature-not-wired    → causal-cyan  (muted cool blue / TODO)
   *                           "scenario's setup path not routed
   *                           through `makeCauslAsync`": a future
   *                           wiring PR returns the cell to OK.
   *    engine-incompatible  → signal-blue  (muted violet, semantic)
   *                           "sync-only LibraryHarness seam the
   *                           wasm engine can't honour today". The
   *                           gap is structural; the chip surfaces it.
   *    timeout              → commit-amber (muted amber)
   *                           "wall-clock budget exceeded": same
   *                           class as the existing DNF row's
   *                           wall-clock kill but emitted up-front.
   *    oom                  → conflict-coral (muted red)
   *                           "V8 heap exhaustion (exit 134)":
   *                           distinct glyph from the wall-clock
   *                           bucket so the operator-facing chip
   *                           reads the right resource axis.
   *
   *  Legacy free-form skip entries (history rows captured before
   *  #37 landed) fall through `classifySkip()` above, preserving
   *  the previous render path so the dashboard still reads off
   *  unmigrated history without re-baselining.
   *  ---------------------------------------------------------------- */
  const TYPED_SKIP_REASONS = Object.freeze([
    'artefact-missing',
    'feature-not-wired',
    'engine-incompatible',
    'timeout',
    'oom',
  ])

  const TYPED_SKIP_META = {
    'artefact-missing': {
      label: 'artefact missing',
      cls: 'artefact-missing',
    },
    'feature-not-wired': {
      label: 'feature not wired',
      cls: 'feature-not-wired',
    },
    'engine-incompatible': {
      label: 'engine incompatible',
      cls: 'engine-incompatible',
    },
    timeout: { label: 'timeout', cls: 'timeout' },
    oom: { label: 'OOM', cls: 'oom' },
  }

  function isTypedSkipReason(r) {
    return typeof r === 'string' && TYPED_SKIP_REASONS.includes(r)
  }

  /** Classify a typed SKIP cell (`status: 'SKIP'` + enum `reason`).
   *  Returns `{ label, cls, typed: true }` when the reason matches
   *  the taxonomy; returns `null` for legacy (free-form) entries so
   *  the caller can fall through to `classifySkip()`. */
  function classifyTypedSkip(reason) {
    if (!isTypedSkipReason(reason)) return null
    const meta = TYPED_SKIP_META[reason]
    return { label: meta.label, cls: meta.cls, typed: true }
  }

  /** ----------------------------------------------------------------
   *  Filter state: managed as a single mutable object that the
   *  filter UI mutates and the renderer reads. Re-renders are
   *  triggered by `applyFilters()`.
   *  ---------------------------------------------------------------- */
  /** capturedAt of the first run in the comparable era, or null when
   *  the whole history is one era. Read by the chart renderer to draw
   *  the boundary rule when the earlier runs are switched on. */
  let eraBoundaryCapturedAt = null

  const filterState = {
    libraries: null, // Set<string>, populated on first render
    scenarios: null, // Set<string>
    scales: null, // Set<number>
    /** Show history from before the current library set existed.
     *  Off by default: see comparableEraStart(). */
    fullHistory: false,
  }

  /** ----------------------------------------------------------------
   *  The comparable era.
   *
   *  `history.json` reaches back to 2026-05, and the wasm engine was
   *  not measured until 2026-07-29. Every entry before that carries
   *  causl-ts, jotai, mobx and redux-toolkit and NO causl-wasm, because
   *  there was no attested Rust-engine measurement to carry: the two
   *  wasm-labelled series that once sat there were deleted outright
   *  (see the series-id note at the top of this file) rather than
   *  relabelled, and back-filling them now would re-commit the defect
   *  that deletion fixed.
   *
   *  Charted across the whole range, that reads as a defect in the
   *  suite rather than a fact about when the engine started being
   *  measured: causl-ts draws a line across 32 runs and causl-wasm a
   *  stub across the last 4. The same is true of redux, which is
   *  `redux-toolkit` for the old era and `redux-rtk` for the new.
   *
   *  So the default window is the longest SUFFIX of history over which
   *  the measured library set does not change, which is the range over
   *  which the series are actually comparable. Nothing is hidden: the
   *  earlier runs are one checkbox away, and turning them on marks the
   *  boundary on every chart rather than blending the two eras.
   *
   *  Derived from the data, never hard-coded to a date, so it stays
   *  correct the next time a library is added or retired.
   *
   *  @param {Array} history
   *  @returns {number} index of the first entry in the comparable era
   *  ---------------------------------------------------------------- */
  function comparableEraStart(history) {
    if (!Array.isArray(history) || history.length === 0) return 0
    const libsOf = (e) => {
      const set = new Set()
      for (const smp of e.samples ?? []) if (smp.library) set.add(smp.library)
      return set
    }
    const sameLibs = (a, b) => a.size === b.size && [...a].every((l) => b.has(l))
    // Walk back from the newest run while the library set is unchanged.
    const latest = libsOf(history[history.length - 1])
    if (latest.size === 0) return 0
    let start = history.length - 1
    while (start > 0) {
      const prev = history[start - 1]
      // Entries with no samples at all carry no library claim; step
      // over them rather than letting one end the era.
      const prevLibs = libsOf(prev)
      if (prevLibs.size > 0 && !sameLibs(prevLibs, latest)) break
      start -= 1
    }
    return start
  }

  /** ----------------------------------------------------------------
   *  Pure helpers
   *  ---------------------------------------------------------------- */

  /** Format a number for tick labels: never depends on locale. */
  function formatNumber(n) {
    if (!Number.isFinite(n)) return 'n/a'
    if (Math.abs(n) >= 100) return Math.round(n).toString()
    return Number.parseFloat(n.toFixed(2)).toString()
  }

  /** Format a byte total with thousands separators ("12,345 bytes"). */
  function formatBytesCount(n) {
    if (typeof n !== 'number' || !Number.isFinite(n) || n < 0) return null
    return `${Math.round(n).toLocaleString('en-US')} bytes`
  }

  /**
   * #33 follow-up: render the boundary-crossings + boundary-bytes
   * tooltip rows for a per-cell history point. The publisher preserves
   * three distinct states for these fields:
   *
   *   1. number-on-both:                 wasm sample with measurements.
   *      Render both rows + a derived bytes-per-crossing average. The
   *      bytes-per-crossing line is the load-bearing one: per-crossing
   *      cost varies ~1000× by payload size, so the average bytes/event
   *      is the real cost proxy the dashboard is here to surface.
   *
   *   2. crossings-only:                 wasm sample captured before the
   *      `boundaryBytes` column was added (historical 39-cell sweep).
   *      Render just the crossings row to match the pre-#33 tooltip;
   *      no fabricated byte count, no fabricated average. Missing data
   *      stays missing: honest absence over a made-up number.
   *
   *   3. neither (or boundaryBytes === null):   non-wasm library
   *      (jotai/mobx/redux/causl-ts). The FFI boundary is structurally
   *      absent. Render nothing: degrade to the pre-existing median/
   *      p95-only tooltip so non-wasm libraries' tooltips are
   *      unchanged.
   *
   * Returns a leading-newline string (or '' for state 3). The caller
   * appends it to the median/p95 tooltip line so the formatting flows
   * `median · p95\ncrossings: …\nbytes: …\navg: …` without a trailing
   * empty line when nothing renders.
   */
  function formatBoundaryTooltipLines(point) {
    const crossings = point.boundaryCrossings
    const bytes = point.boundaryBytes
    const hasCrossings =
      typeof crossings === 'number' && Number.isFinite(crossings)
    const hasBytes =
      typeof bytes === 'number' && Number.isFinite(bytes) && bytes >= 0
    // State 3: neither applies, OR boundaryBytes is explicitly null
    // (non-wasm library) and there are no crossings to render either.
    // Crossings === 0 is still a real measurement (the JS-backed
    // causl-ts leg legitimately reports 0), but state 3 here means
    // the field is absent from the sample, not zero. We branch on
    // existence, not truthiness.
    if (!hasCrossings && !hasBytes) return ''
    let out = ''
    if (hasCrossings) {
      out += `\nboundary crossings: ${crossings.toLocaleString('en-US')}`
    }
    if (hasBytes) {
      out += `\nboundary bytes: ${formatBytesCount(bytes)}`
      // Average bytes-per-crossing: only meaningful when BOTH fields
      // are present AND crossings > 0 (else the divisor is zero / NaN).
      // This is the cost-per-byte view the dashboard is being extended
      // to surface; per-crossing cost varies ~1000× by payload size.
      if (hasCrossings && crossings > 0) {
        const avg = bytes / crossings
        const avgRounded =
          avg >= 100
            ? Math.round(avg).toLocaleString('en-US')
            : avg.toFixed(1)
        out += `\navg ${avgRounded} bytes / crossing`
      }
    }
    return out
  }

  /** Tooltip-friendly ISO date → "May 06". */
  function formatShortDate(iso) {
    const d = new Date(iso)
    if (Number.isNaN(d.getTime())) return iso
    const month = d.toLocaleString('en-US', { month: 'short' })
    return `${month} ${d.getUTCDate().toString().padStart(2, '0')}`
  }

  /** Escape any text we splat into innerHTML: defence-in-depth even
   *  though the upstream JSON is checked-in / authored. */
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;')
  }

  /** Group a HistoryEntry[] into a Map keyed by `scenario|scale`,
   *  each value a section descriptor:
   *    {
   *      scenario, scale,
   *      runs:    [ { capturedAt, version }, … ]   // run order
   *      series:  Map<library, Array<{capturedAt, version, medianMs, p95Ms}>>
   *      skipped: Map<library, { reason, capturedAt, version }>
   *    }
   *
   *  `runs` is the union of capturedAt timestamps across every
   *  series so the x-axis can be shared. Per-series points are
   *  stored in run order; if a particular run didn't include a
   *  sample for some framework, that framework's series simply
   *  has fewer points (and the missing slot is left empty; the
   *  chart renderer interpolates a gap by skipping non-finite
   *  values, same as it did in the per-cell version).
   *
   *  In practice the bench harness emits a HistorySample for every
   *  library on every run, so the series stay aligned. The
   *  union-of-timestamps approach handles the edge case where a
   *  library was added or removed mid-history without breaking the
   *  shared-axis assumption, which is exactly what the deletion of the
   *  two withdrawn wasm series did to the 2026-05 entries.
   *
   *  #1293: each HistoryEntry may also carry a `skipped` array of
   *  (library × scenario × scale) cells the runner attempted but
   *  couldn't measure (stack overflow, architectural gap, timeout).
   *  We attach those to the matching section's `skipped` map keyed by
   *  library so the legend chip can render a strikethrough + ✗ +
   *  tooltip instead of silently dropping the framework from the
   *  chart. The latest run's reason wins if multiple runs disagree,
   *  consistent with how `series` values are appended chronologically
   *  and the legend reads the latest sample. */
  function groupSections(history) {
    const sections = new Map()
    // Track unique runs per section so the x-axis can be shared.
    const runSetBySection = new Map()
    const ensureSection = (scenario, scale) => {
      const key = `${scenario}|${scale}`
      let section = sections.get(key)
      if (!section) {
        section = {
          scenario,
          scale,
          runs: [],
          series: new Map(),
          skipped: new Map(),
        }
        sections.set(key, section)
        runSetBySection.set(key, new Set())
      }
      return { section, key }
    }
    for (const entry of history) {
      for (const sample of entry.samples) {
        const { section, key } = ensureSection(sample.scenario, sample.scale)
        const runSet = runSetBySection.get(key)
        if (!runSet.has(entry.capturedAt)) {
          runSet.add(entry.capturedAt)
          section.runs.push({
            capturedAt: entry.capturedAt,
            version: entry.version,
          })
        }
        let series = section.series.get(sample.library)
        if (!series) {
          series = []
          section.series.set(sample.library, series)
        }
        series.push({
          capturedAt: entry.capturedAt,
          version: entry.version,
          medianMs: sample.medianMs,
          p95Ms: sample.p95Ms,
          // #33 follow-up: surface boundary crossings + bytes in the
          // per-point tooltip when both are present (wasm-backed
          // samples only). The publisher preserves `null` for non-wasm
          // libraries to mean "not applicable to this engine", and
          // historical samples captured before the field existed have
          // it absent (undefined). The tooltip renderer treats null
          // and undefined the same, degrading to the crossing-only
          // tooltip (or, when crossings is also absent, the original
          // median/p95-only tooltip). NO new chart series: bytes is
          // tooltip-only; a dedicated chart line is a separate PR.
          boundaryCrossings: sample.boundaryCrossings,
          boundaryBytes: sample.boundaryBytes,
        })
      }
      // Attach per-entry skipped cells to the matching section so the
      // legend can mark the framework's chip with a strikethrough + ✗
      // + tooltip carrying the reason (#1293). A skipped cell does NOT
      // count as a run timestamp: the chart axis stays driven by the
      // succeeded-cell runs to preserve x-axis stability.
      const skipped = Array.isArray(entry.skipped) ? entry.skipped : []
      for (const cell of skipped) {
        const { section } = ensureSection(cell.scenario, cell.scale)
        // causl-bench#37 introduced a typed schema:
        //   { status: 'SKIP', reason: SkipReason, detail: string }
        // alongside the legacy `{ reason: <free-form string> }` rows
        // already in history. We lift BOTH shapes into one normalized
        // section descriptor:
        //   - `typedReason` is set ONLY when the row matches the
        //     typed taxonomy (status === 'SKIP' AND reason is in the
        //     enum). The renderer branches on its presence to pick
        //     the typed-chip palette.
        //   - `detail` is the human-readable elaboration (typed
        //     schema's `detail`, or '' for legacy rows).
        //   - `reason` keeps the original string (typed enum value
        //     OR free-form legacy text) so the legacy `classifySkip`
        //     fallback + the existing tooltip-shows-reason behaviour
        //     keep working unchanged for un-migrated history rows.
        // Latest-wins so the freshest skip reason surfaces if a
        // framework's failure mode changes between runs.
        const rawReason = cell.reason
        const typedReason =
          cell.status === 'SKIP' && isTypedSkipReason(rawReason)
            ? rawReason
            : null
        section.skipped.set(cell.library, {
          reason: String(rawReason ?? ''),
          typedReason,
          detail: typeof cell.detail === 'string' ? cell.detail : '',
          status: typeof cell.status === 'string' ? cell.status : null,
          capturedAt: entry.capturedAt,
          version: entry.version,
        })
      }
    }
    // Sort runs ascending by capturedAt so the x-axis reads
    // left-to-right; sort each series the same way so points line
    // up with the runs array by index.
    const byTime = (a, b) =>
      new Date(a.capturedAt).getTime() - new Date(b.capturedAt).getTime()
    for (const section of sections.values()) {
      section.runs.sort(byTime)
      for (const series of section.series.values()) series.sort(byTime)
    }
    return sections
  }

  /** Sort sections by scenario alpha, then scale ascending. */
  function sortSections(sections) {
    const arr = Array.from(sections.values())
    arr.sort((a, b) => {
      if (a.scenario !== b.scenario) return a.scenario < b.scenario ? -1 : 1
      return a.scale - b.scale
    })
    return arr
  }

  /** ----------------------------------------------------------------
   *  Verdict computation (per series)
   *
   *  We don't have per-iteration samples in HistoryEntry: each
   *  HistorySample is a pre-aggregated (medianMs, p95Ms). The CoV
   *  check therefore uses the gap between p95 and the median as a
   *  proxy for sample dispersion: `(p95 - median) / median`. This
   *  is a coarse approximation that runs strictly bigger than the
   *  true CoV (a p95-spread is wider than one stdev), so it errs
   *  on the side of marking borderline cells noisy, the right
   *  failure mode for a regression dashboard.
   *  ---------------------------------------------------------------- */
  function computeVerdict(points) {
    if (!points || points.length === 0) return { kind: 'unknown', detail: 'no data' }

    const last = points[points.length - 1]
    if (!Number.isFinite(last.medianMs)) {
      return { kind: 'unknown', detail: 'last median is non-finite' }
    }

    // Noisy check fires regardless of trend: a high-variance latest
    // run isn't trustable as either pass or regression.
    if (Number.isFinite(last.p95Ms) && last.medianMs > 0) {
      const proxy = (last.p95Ms - last.medianMs) / last.medianMs
      if (proxy > NOISY_PROXY) {
        return {
          kind: 'noisy',
          detail: `p95 ${formatNumber(last.p95Ms)} ms is ${formatNumber(proxy * 100)}% above median (latest run)`,
        }
      }
    }

    if (points.length < 2) {
      return { kind: 'unknown', detail: 'only one run, no trend yet' }
    }

    const prev = points[points.length - 2]
    if (!Number.isFinite(prev.medianMs) || prev.medianMs <= 0) {
      return { kind: 'unknown', detail: 'previous median is non-finite' }
    }

    const delta = (last.medianMs - prev.medianMs) / prev.medianMs
    const detail = `${delta >= 0 ? '+' : ''}${formatNumber(delta * 100)}% vs prior run (${formatNumber(prev.medianMs)} → ${formatNumber(last.medianMs)} ms)`

    if (delta > REGRESSION_PCT) return { kind: 'regressed', detail }
    if (delta < -IMPROVED_PCT) return { kind: 'improved', detail }
    if (Math.abs(delta) <= PASS_PCT) return { kind: 'pass', detail }
    // 5% to 10% drift: not a regression but not strictly within-pass.
    // We still call it pass because the spec only carves "regressed"
    // and "improved" out at the 10% line.
    return { kind: 'pass', detail }
  }

  /** Build the placeholder profile-artifact URL for a (library,
   *  scenario, scale) tuple. The path scheme matches the
   *  `profiles/${lib}-${scn}-${n}/` layout the perf-snapshot
   *  workflow uploads (currently disabled; see
   *  `.github/workflows-disabled/perf-snapshot.yml`). The `latest/`
   *  segment is the symlink the publish step will point at the
   *  most recent date+sha directory once CI is restored. */
  function profileUrlFor(library, scenario, scale) {
    const slug = `${library}-${scenario}-${scale}`
    return `./profiles/latest/${slug}/`
  }

  /** ----------------------------------------------------------------
   *  Cubic-Hermite interpolation (D3-equivalent smoothing).
   *
   *  d3-shape's `curveCatmullRom` is ~30 lines of math; we inline
   *  the same algorithm so we don't need d3-shape. Given a series
   *  of (x,y) points we emit an SVG path with cubic Bézier segments
   *  whose tangents come from the Catmull-Rom formula at α=0.5
   *  (centripetal: handles non-uniformly-spaced x values without
   *  overshoot). For < 2 points we just render a moveTo.
   *  ---------------------------------------------------------------- */
  function catmullRomPath(points) {
    if (points.length === 0) return ''
    if (points.length === 1) {
      const [p] = points
      return `M ${p[0].toFixed(2)} ${p[1].toFixed(2)}`
    }
    const cmds = [`M ${points[0][0].toFixed(2)} ${points[0][1].toFixed(2)}`]
    for (let i = 0; i < points.length - 1; i += 1) {
      const p0 = points[i - 1] ?? points[i]
      const p1 = points[i]
      const p2 = points[i + 1]
      const p3 = points[i + 2] ?? p2
      // Catmull-Rom → Bézier tangent magnitudes (α = 0.5, centripetal).
      const c1x = p1[0] + (p2[0] - p0[0]) / 6
      const c1y = p1[1] + (p2[1] - p0[1]) / 6
      const c2x = p2[0] - (p3[0] - p1[0]) / 6
      const c2y = p2[1] - (p3[1] - p1[1]) / 6
      cmds.push(
        `C ${c1x.toFixed(2)} ${c1y.toFixed(2)} ` +
          `${c2x.toFixed(2)} ${c2y.toFixed(2)} ` +
          `${p2[0].toFixed(2)} ${p2[1].toFixed(2)}`,
      )
    }
    return cmds.join(' ')
  }

  /** ----------------------------------------------------------------
   *  "Nice" axis ticks (D3-equivalent d3-scale.ticks).
   *
   *  Given a domain [min, max] and a target tick count, return ≤
   *  `count + 1` "round" tick values whose step is in {1, 2, 5} ×
   *  10^k. Mirrors d3-scale.ticks so visiting the chart "feels" the
   *  same as a D3 chart, without the dependency.
   *  ---------------------------------------------------------------- */
  function niceTicks(min, max, count) {
    if (!Number.isFinite(min) || !Number.isFinite(max) || count <= 0) return []
    if (min === max) return [min]
    const span = max - min
    const step0 = span / count
    const power = Math.floor(Math.log10(step0))
    const base = Math.pow(10, power)
    const ratio = step0 / base
    let step
    if (ratio >= 5) step = 10 * base
    else if (ratio >= 2) step = 5 * base
    else if (ratio >= 1) step = 2 * base
    else step = base
    const start = Math.ceil(min / step) * step
    const ticks = []
    // Guard against fp drift: limit to count+2 ticks max.
    for (let i = 0; i < count + 2; i += 1) {
      const v = start + i * step
      if (v > max + step * 1e-9) break
      // Round to kill fp dust like 0.30000000000000004.
      const rounded = Math.round(v * 1e6) / 1e6
      ticks.push(rounded)
    }
    return ticks
  }

  /** ----------------------------------------------------------------
   *  Section chart (SVG): one chart, N stacked series.
   *
   *  Conventions:
   *    - viewBox + xmlns for clean inline embedding
   *    - <path> with cubic-Hermite smoothing for each median series
   *    - <path> with same smoothing for each p95 band envelope
   *    - "nice" axis ticks (1/2/5 × 10^k) on the y-axis (shared)
   *    - per-point invisible hit-rect with <title> for full hover
   *      coverage (every run is hoverable, not just the latest),
   *      drawn per-series so hover messages name the library
   *
   *  Series share x-axis (run index ↔ section.runs index) and
   *  y-axis (ms). The y-domain is computed across all visible
   *  series + their p95 envelopes so every line fits in the
   *  drawable area.
   *  ---------------------------------------------------------------- */
  /** Per-section Y-axis fill fraction (zoom). `DEFAULT_FILL` places
   *  the highest plotted value at 80% of the chart's drawable height
   *  (20% headroom so the peak label is not jammed against the frame)
   *  and stretches the rest of the series across the lower 80%,
   *  instead of squishing everything into a sliver with empty
   *  whitespace above. Mouse-WHEEL over a chart mutates this per
   *  section (wheel up → larger fill / zoom in, wheel down → zoom
   *  out); double-click resets. */
  const DEFAULT_FILL = 0.8
  const MIN_FILL = 0.08
  const MAX_FILL = 6
  const Y_FILL = new Map()
  const sectionKey = (s) => `${s.scenario}|${s.scale}`
  const clampFill = (f) => Math.min(MAX_FILL, Math.max(MIN_FILL, f))
  const yFillFor = (s) => Y_FILL.get(sectionKey(s)) ?? DEFAULT_FILL

  /** Per-section vertical pan offset, in SVG pixels (+down / −up).
   *  Pointer-DRAG on a chart mutates this so the whole plot (every
   *  series line AND the y-axis ticks/labels, since both go through
   *  `yAt`) translates together. Its purpose: lift a line that hugs
   *  the 0 baseline up off the chart floor for a closer look. Not
   *  clamped (drag as far as you like; the viewBox clips); double-
   *  click resets it. */
  const Y_OFFSET = new Map()
  const yOffsetFor = (s) => Y_OFFSET.get(sectionKey(s)) ?? 0

  function renderSectionChart(section, visibleLibraries) {
    const W = 640
    const H = 220
    const PAD_L = 50
    const PAD_R = 16
    const PAD_T = 14
    const PAD_B = 34

    const runs = section.runs
    if (runs.length === 0) {
      return (
        `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" ` +
        `role="img" aria-label="no data for ${escapeHtml(section.scenario)} at scale ${section.scale}">` +
        `<text x="${W / 2}" y="${H / 2}" text-anchor="middle" font-size="12" fill="#A9B5C9">no data</text>` +
        `</svg>`
      )
    }

    // Build a quick index from capturedAt → run index for series
    // lookups (per-series points use their own capturedAt to map
    // back to the section's shared run timeline).
    const runIndex = new Map()
    runs.forEach((r, i) => runIndex.set(r.capturedAt, i))

    // Restrict to visible libraries (in canonical order so colours
    // and legend ordering are stable across renders).
    const libsToPlot = LIBRARY_ORDER
      .filter((l) => section.series.has(l) && visibleLibraries.has(l))
      .concat(
        // Any unknown libraries (defensive, for fixtures that ship
        // a non-canonical library name) come after the known order.
        Array.from(section.series.keys())
          .filter((l) => !LIBRARY_ORDER.includes(l) && visibleLibraries.has(l))
          .sort(),
      )

    if (libsToPlot.length === 0) {
      return (
        `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" ` +
        `role="img" aria-label="no libraries enabled for ${escapeHtml(section.scenario)} at scale ${section.scale}">` +
        `<text x="${W / 2}" y="${H / 2}" text-anchor="middle" font-size="12" fill="#A9B5C9">no libraries enabled</text>` +
        `</svg>`
      )
    }

    // Y-domain is anchored at 0: every chart shares the same floor.
    // We deliberately do NOT auto-fit the bottom to the data minimum:
    // that glued the fastest library to the chart floor and made a
    // "12 ms" line on one chart look the same height as a "0.4 ms"
    // line on another. With a fixed 0 baseline a fast library reads
    // as a genuinely low line and cross-chart heights are comparable.
    // Only the top (yMax, across every visible median + p95 envelope)
    // is data-driven; the fill headroom below keeps the peak off the
    // frame, and the pan offset lets you lift a floor-hugging line up.
    const yMin = 0
    let yMax = Number.NEGATIVE_INFINITY
    for (const lib of libsToPlot) {
      for (const p of section.series.get(lib)) {
        if (Number.isFinite(p.medianMs)) yMax = Math.max(yMax, p.medianMs)
        if (Number.isFinite(p.p95Ms)) yMax = Math.max(yMax, p.p95Ms)
      }
    }
    if (!Number.isFinite(yMax) || yMax <= 0) yMax = 1

    const innerW = W - PAD_L - PAD_R
    const innerH = H - PAD_T - PAD_B

    const xAt = (i) => {
      if (runs.length === 1) return PAD_L + innerW / 2
      return PAD_L + (innerW * i) / (runs.length - 1)
    }
    // y = baseline(0) at PAD_T+innerH, scaled by `fill` (wheel-zoom)
    // and then translated by `panY` (drag-pan). `t` is the fraction
    // of the 0→yMax domain; at the 0.8 default fill the peak sits at
    // 80% height (20% headroom) and 0 sits exactly on the chart
    // floor. Neither term is clamped into the box: zooming/panning
    // intentionally lets data run past the frame (the viewBox clips)
    // so a floor-hugging line can be lifted up for inspection.
    // `panY` is added last and unconditionally (even for the
    // non-finite fallback) so the gridlines and labels, which also
    // call yAt, pan in lockstep with the series.
    const fill = clampFill(yFillFor(section))
    const panY = yOffsetFor(section)
    const yAt = (v) => {
      if (!Number.isFinite(v)) return PAD_T + innerH + panY
      const t = (v - yMin) / (yMax - yMin)
      return PAD_T + innerH * (1 - t * fill) + panY
    }

    // Y-axis ticks: drawn first so series lines sit on top.
    let yTickValues = niceTicks(yMin, yMax, 4)
    if (yTickValues.length < 2) yTickValues = [yMin, yMax]
    const tick = (v) => {
      const y = yAt(v)
      const label = formatNumber(v)
      return (
        `<g class="tick">` +
        `<line x1="${PAD_L}" y1="${y.toFixed(2)}" ` +
        `x2="${W - PAD_R}" y2="${y.toFixed(2)}" ` +
        `stroke="rgba(169,181,201,0.16)" stroke-width="1" />` +
        `<text x="${(PAD_L - 6).toFixed(2)}" y="${(y + 3).toFixed(2)}" ` +
        `text-anchor="end" font-size="10" fill="#A9B5C9">${label}</text>` +
        `</g>`
      )
    }
    const yTicks = yTickValues.map(tick).join('')

    // Era boundary. Only drawn when the pre-boundary runs are switched
    // on: to the left of this rule the library set is different, so a
    // line that begins AT the rule began being measured there and is
    // not a series that dropped out earlier. Marking it beats blending
    // the two eras silently, which is what made the older runs read as
    // a gap in the newer series.
    let eraRule = ''
    if (filterState.fullHistory && eraBoundaryCapturedAt) {
      const bIdx = runIndex.get(eraBoundaryCapturedAt)
      if (Number.isFinite(bIdx) && bIdx > 0) {
        const bx = xAt(bIdx - 0.5)
        eraRule =
          `<g class="era-boundary" data-captured-at="${escapeHtml(eraBoundaryCapturedAt)}">` +
          `<line x1="${bx.toFixed(2)}" y1="${PAD_T}" x2="${bx.toFixed(2)}" ` +
          `y2="${(PAD_T + innerH).toFixed(2)}" stroke="rgba(169,181,201,0.55)" ` +
          `stroke-width="1" stroke-dasharray="3 3" />` +
          `<text x="${(bx + 4).toFixed(2)}" y="${(PAD_T + 10).toFixed(2)}" ` +
          `font-size="9" fill="#A9B5C9">library set changes</text>` +
          `</g>`
      }
    }

    // Per-series bands + lines + dots. Order matters: bands first
    // (so lines draw on top of all bands and aren't visually
    // covered by a later band), then median lines, then dots.
    let allBands = ''
    let allLines = ''
    let allDots = ''
    let allHitRects = ''
    for (const lib of libsToPlot) {
      const color = LIBRARY_COLOR[lib] ?? '#A9B5C9'
      const series = section.series.get(lib)
      // Convert series points → (xAt(runIndex), yAt(value)) tuples.
      // Points whose run isn't in the section's runs map (defensive)
      // or whose values are non-finite are dropped from path data.
      const medianRaw = []
      const upperRaw = []
      const lowerRaw = []
      for (const p of series) {
        const idx = runIndex.get(p.capturedAt)
        if (idx === undefined) continue
        const x = xAt(idx)
        if (Number.isFinite(p.medianMs)) {
          medianRaw.push([x, yAt(p.medianMs)])
        }
        if (Number.isFinite(p.medianMs) && Number.isFinite(p.p95Ms)) {
          upperRaw.push([x, yAt(p.p95Ms)])
          lowerRaw.push([x, yAt(p.medianMs)])
        }
      }

      if (upperRaw.length >= 2) {
        const upPath = catmullRomPath(upperRaw)
        const downPts = lowerRaw.slice().reverse()
        const downCmds = downPts
          .map((p) => `L ${p[0].toFixed(2)} ${p[1].toFixed(2)}`)
          .join(' ')
        allBands +=
          `<path d="${upPath} ${downCmds} Z" fill="${color}" ` +
          `fill-opacity="0.14" stroke="none" data-series="${escapeHtml(lib)}" data-library="${escapeHtml(lib)}" />`
      }

      if (medianRaw.length > 0) {
        const dash = LIBRARY_DASH[lib]
        const dashAttr = dash ? ` stroke-dasharray="${dash}"` : ''
        allLines +=
          `<path d="${catmullRomPath(medianRaw)}" fill="none" ` +
          `stroke="${color}" stroke-width="1.8" stroke-linecap="round" ` +
          `stroke-linejoin="round"${dashAttr} data-series="${escapeHtml(lib)}" data-library="${escapeHtml(lib)}" />`
      }

      // Dots per datapoint with embedded tooltip.
      for (const p of series) {
        const idx = runIndex.get(p.capturedAt)
        if (idx === undefined || !Number.isFinite(p.medianMs)) continue
        const tooltip =
          `${escapeHtml(getLibraryLabel(lib))} · ${formatShortDate(p.capturedAt)} · ${escapeHtml(p.version)}\n` +
          `median ${formatNumber(p.medianMs)} ms · p95 ${formatNumber(p.p95Ms)} ms` +
          formatBoundaryTooltipLines(p) +
          (LIBRARY_ANNOTATION[lib]
            ? `\n${escapeHtml(LIBRARY_ANNOTATION[lib])}`
            : '')
        allDots +=
          `<circle cx="${xAt(idx).toFixed(2)}" cy="${yAt(p.medianMs).toFixed(2)}" ` +
          `r="2.8" fill="${color}" stroke="rgba(11,16,32,0.6)" stroke-width="1" ` +
          `data-series="${escapeHtml(lib)}" data-library="${escapeHtml(lib)}">` +
          `<title>${tooltip}</title>` +
          `</circle>`
      }

      // Per-series invisible hit rects: narrow vertical strips
      // centred on each datapoint x. They overlap across series
      // (one rect per (lib, run)); SVG hit-testing prefers the
      // topmost element so the last-drawn library "wins" hover for
      // a given x, but each library contributes its own tooltip
      // because the rects are stacked at different y-bands
      // (median ± a small radius). We keep them full-height so the
      // user can hover any column and read at least one library's
      // values. For fine-grained per-series tooltips the dot
      // tooltips above remain authoritative.
      if (runs.length >= 2) {
        const colW = innerW / (runs.length - 1)
        for (const p of series) {
          const idx = runIndex.get(p.capturedAt)
          if (idx === undefined) continue
          const cx = xAt(idx)
          const x0 = Math.max(PAD_L, cx - colW / 2)
          const x1 = Math.min(W - PAD_R, cx + colW / 2)
          // Stack rects by library so each one has a narrow
          // vertical slice it owns, so that every series'
          // tooltip is reachable somewhere in the column. We give
          // each library a 1/N-of-innerH band, ordered top-down by
          // canonical library order.
          const libIdx = libsToPlot.indexOf(lib)
          const bandH = innerH / libsToPlot.length
          const y0 = PAD_T + libIdx * bandH
          const tooltip =
            `${escapeHtml(lib)} · ${formatShortDate(p.capturedAt)} · ${escapeHtml(p.version)}\n` +
            `median ${formatNumber(p.medianMs)} ms · p95 ${formatNumber(p.p95Ms)} ms` +
            formatBoundaryTooltipLines(p)
          allHitRects +=
            `<rect x="${x0.toFixed(2)}" y="${y0.toFixed(2)}" ` +
            `width="${(x1 - x0).toFixed(2)}" height="${bandH.toFixed(2)}" ` +
            `fill="transparent" pointer-events="all" data-series="${escapeHtml(lib)}" data-library="${escapeHtml(lib)}">` +
            `<title>${tooltip}</title>` +
            `</rect>`
        }
      }
    }

    // X-axis baseline + first/middle/last labels: shared across
    // every series so the eye anchors to the same run column.
    const xAxisY = (PAD_T + innerH).toFixed(2)
    const xAxisLine =
      `<line x1="${PAD_L}" y1="${xAxisY}" x2="${W - PAD_R}" y2="${xAxisY}" ` +
      `stroke="rgba(169,181,201,0.32)" stroke-width="1" />`
    let xTicks = ''
    if (runs.length >= 2) {
      const first = formatShortDate(runs[0].capturedAt)
      const last = formatShortDate(runs[runs.length - 1].capturedAt)
      xTicks =
        `<text x="${PAD_L}" y="${H - 10}" font-size="10" fill="#A9B5C9">${escapeHtml(first)}</text>` +
        `<text x="${W - PAD_R}" y="${H - 10}" text-anchor="end" font-size="10" fill="#A9B5C9">${escapeHtml(last)}</text>`
      if (runs.length >= 3) {
        const midIdx = Math.floor((runs.length - 1) / 2)
        const midLabel = formatShortDate(runs[midIdx].capturedAt)
        const midX = xAt(midIdx)
        if (midX > PAD_L + 40 && midX < W - PAD_R - 40) {
          xTicks +=
            `<text x="${midX.toFixed(2)}" y="${H - 10}" text-anchor="middle" ` +
            `font-size="10" fill="#A9B5C9">${escapeHtml(midLabel)}</text>`
        }
      }
    } else {
      xTicks =
        `<text x="${PAD_L + innerW / 2}" y="${H - 10}" text-anchor="middle" ` +
        `font-size="10" fill="#A9B5C9">${escapeHtml(formatShortDate(runs[0].capturedAt))}</text>`
    }

    const yCaption = `<text x="6" y="${PAD_T + 9}" font-size="10" fill="#A9B5C9">ms</text>`

    const ariaLabel =
      `${section.scenario} at scale ${section.scale}: ` +
      `${libsToPlot.length} framework${libsToPlot.length === 1 ? '' : 's'} ` +
      `over ${runs.length} run${runs.length === 1 ? '' : 's'}`

    return (
      `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" ` +
      `role="img" aria-label="${escapeHtml(ariaLabel)}">` +
      yTicks +
      eraRule +
      allBands +
      allLines +
      xAxisLine +
      allDots +
      xTicks +
      yCaption +
      allHitRects +
      `</svg>`
    )
  }

  /** ----------------------------------------------------------------
   *  Section card DOM: one card per (scenario × scale) with the
   *  stacked chart on top and a legend strip beneath, one chip per
   *  framework. Each chip shows: library colour swatch, library
   *  name, latest median + p95, and a per-series verdict badge
   *  (the same verdict logic that used to live on per-cell cards).
   *  A "profile" link per chip provides the artifact-snapshot
   *  affordance the original cell-card design exposed.
   *  ---------------------------------------------------------------- */
  function renderSectionCard(section, visibleLibraries) {
    const card = document.createElement('article')
    card.className = 'bench-section-card'
    card.dataset.scenario = section.scenario
    card.dataset.scale = String(section.scale)

    const skippedMap =
      section.skipped instanceof Map ? section.skipped : new Map()

    // Plot order is canonical first, then any unknown libraries
    // alphabetically. Libraries with a `series` slot AND visible go
    // first; libraries that are skipped at this (scenario × scale)
    // *also* get a legend chip (rendered with strikethrough + ✗ +
    // tooltip per #1293) so the reader never sees a cell that
    // silently omitted a competitor.
    const liveLibs = new Set()
    for (const l of section.series.keys()) {
      if (visibleLibraries.has(l)) liveLibs.add(l)
    }
    for (const l of skippedMap.keys()) {
      if (visibleLibraries.has(l)) liveLibs.add(l)
    }
    const libsToPlot = LIBRARY_ORDER.filter((l) => liveLibs.has(l)).concat(
      Array.from(liveLibs)
        .filter((l) => !LIBRARY_ORDER.includes(l))
        .sort(),
    )

    // Header: scenario name + scale chip.
    const header = document.createElement('header')
    header.className = 'bench-section-head'
    header.innerHTML =
      `<h3 class="bench-section-title">${escapeHtml(section.scenario)}</h3>` +
      `<span class="bench-section-scale">scale ${section.scale.toLocaleString('en-US')}</span>`
    card.appendChild(header)

    // Chart.
    const chartWrap = document.createElement('div')
    chartWrap.className = 'bench-section-chart'
    chartWrap.innerHTML = renderSectionChart(section, visibleLibraries)

    // Pan (pointer-drag) + zoom (wheel) on the chart. Pan lives in
    // Y_OFFSET, zoom in Y_FILL, both keyed by section. A drag
    // anywhere on the card, whether on empty space or directly on a
    // series line, pans the whole plot (lines and y-axis ticks move
    // together because both resolve through yAt). Listeners sit on
    // the stable chartWrap div (not the SVG, which the re-render
    // replaces) so a gesture survives every redraw. Redraw is
    // synchronous: the SVG is a small string and rebuilding it per
    // pointermove/wheel is cheap and avoids requestAnimationFrame
    // throttling on a backgrounded tab that would drop updates.
    // Double-click resets pan AND zoom to the auto-fit default.
    {
      let dragging = false
      let startY = 0
      let startOffset = 0
      const rerender = () => {
        chartWrap.innerHTML = renderSectionChart(
          section,
          visibleLibraries,
        )
      }
      chartWrap.addEventListener('pointerdown', (e) => {
        dragging = true
        startY = e.clientY
        startOffset = yOffsetFor(section)
        chartWrap.classList.add('is-panning')
        try {
          chartWrap.setPointerCapture?.(e.pointerId)
        } catch {
          /* synthetic/invalid pointerId: capture is best-effort */
        }
        e.preventDefault()
      })
      chartWrap.addEventListener('pointermove', (e) => {
        if (!dragging) return
        // 1:1 with the pointer: drag UP (clientY decreases) moves the
        // plot UP (negative SVG-y offset); DOWN moves it down. Not
        // clamped: pan a floor-hugging line as far up as you want.
        Y_OFFSET.set(
          sectionKey(section),
          startOffset + (e.clientY - startY),
        )
        rerender()
      })
      const endDrag = (e) => {
        if (!dragging) return
        dragging = false
        chartWrap.classList.remove('is-panning')
        try {
          chartWrap.releasePointerCapture?.(e.pointerId)
        } catch {
          /* best-effort */
        }
      }
      chartWrap.addEventListener('pointerup', endDrag)
      chartWrap.addEventListener('pointercancel', endDrag)
      // Wheel = zoom. Multiplicative so the feel is even across the
      // scale range (~530 px of wheel ≈ e¹ ≈ 2.7×). Non-passive: we
      // preventDefault so the page doesn't scroll while zooming a
      // chart under the cursor.
      chartWrap.addEventListener(
        'wheel',
        (e) => {
          e.preventDefault()
          const next = clampFill(
            clampFill(yFillFor(section)) * Math.exp(-e.deltaY / 530),
          )
          Y_FILL.set(sectionKey(section), next)
          rerender()
        },
        { passive: false },
      )
      chartWrap.addEventListener('dblclick', () => {
        Y_FILL.delete(sectionKey(section))
        Y_OFFSET.delete(sectionKey(section))
        rerender()
      })
    }

    // #1304: in-place skip boxes. For every visible library that
    // has no measured points in this section but DOES appear in
    // the section's skipped map, append a horizontal .skip-box
    // strip *inside the chart wrap* so the missing-data row is
    // visible at the exact point the eye expects a bar/line.
    //
    // The chart itself is a smoothed time-series line per library;
    // there is no per-library "bar slot" on the x-axis. The boxes
    // therefore span the chart's full width (the run timeline) and
    // stack vertically, one per skipped library, in canonical order.
    // Each box is fixed-height (28 px), independent of the y-axis
    // domain, so adding/removing skipped libraries does not
    // re-scale the chart.
    //
    // Width matches the chart-area so the strip reads as "the cell
    // this framework would occupy if it had measurements"; the
    // chart's left padding (axis labels) is left clear.
    const skipBoxList = document.createElement('ul')
    skipBoxList.className = 'bench-section-skip-boxes'
    skipBoxList.setAttribute(
      'aria-label',
      `Libraries skipped at ${section.scenario} scale ${section.scale}`,
    )
    let renderedSkipBoxes = 0
    for (const lib of libsToPlot) {
      const skipInfo = skippedMap.get(lib)
      if (!skipInfo) continue
      // Mutex with the chart line: only render the in-chart skip-box
      // when the *latest* event for this library is the skip itself
      // (i.e. no measured sample exists with capturedAt >= the skip's
      // capturedAt). Mirrors the legend chip mutex at the top of the
      // legend loop below. Previously this strip was rendered for
      // every skipped (lib × scenario × scale) cell regardless of
      // whether a more recent measured sample existed, which produced
      // the contradiction reported in #10: chart line + typed-skip
      // chip for the same engine in the same cell.
      const livePoints = section.series.get(lib) ?? []
      const lastLive = livePoints.length
        ? livePoints[livePoints.length - 1]
        : null
      if (
        lastLive &&
        new Date(lastLive.capturedAt) >= new Date(skipInfo.capturedAt)
      ) {
        continue
      }
      const reason = skipInfo.reason || 'skipped: no reason recorded'
      // Typed (causl-bench#37) reasons take precedence over the
      // legacy free-form classifier so the strip reads the chip
      // colour from the new taxonomy when the row carries one. The
      // chip label drops the legacy "library limit" phrasing in
      // favour of the typed reason word ("feature not wired",
      // "engine incompatible", etc.). Legacy rows still flow through
      // classifySkip() unchanged.
      const typed = classifyTypedSkip(skipInfo.typedReason)
      const { label, cls } = typed ?? classifySkip(reason)
      // Tooltip text: typed rows surface the human-readable `detail`
      // (the publisher's "why it skipped" string), then the library
      // tag on a second line so the operator sees both pieces at a
      // glance. Legacy rows fall back to the original free-form
      // reason since `detail` is absent.
      const tooltip =
        typed && skipInfo.detail
          ? `${skipInfo.detail}\nlibrary: ${getLibraryLabel(lib)}`
          : reason
      const color = LIBRARY_COLOR[lib] ?? '#A9B5C9'
      const li = document.createElement('li')
      li.className = `skip-box skip-class-${cls}`
      if (typed) li.classList.add('skip-box--typed')
      li.dataset.library = lib
      li.dataset.skipClass = cls
      if (typed) li.dataset.typedReason = skipInfo.typedReason
      li.setAttribute('role', 'img')
      li.setAttribute('tabindex', '0')
      li.setAttribute(
        'aria-label',
        `${lib} skipped at ${section.scenario} scale ${section.scale}: ${tooltip.replace(/\n/g, '; ')}`,
      )
      li.title = tooltip
      li.style.setProperty('--lib-color', color)
      li.innerHTML =
        `<span class="skip-box-lib" aria-hidden="true">${escapeHtml(getLibraryLabel(lib))}</span>` +
        `<span class="skip-box-label" aria-hidden="true">${escapeHtml(label)}</span>`
      skipBoxList.appendChild(li)
      renderedSkipBoxes += 1
    }
    if (renderedSkipBoxes > 0) chartWrap.appendChild(skipBoxList)

    card.appendChild(chartWrap)

    // Legend strip: one chip per framework currently visible.
    const legend = document.createElement('ul')
    legend.className = 'bench-section-legend'
    legend.setAttribute('aria-label', 'Framework series legend with per-line verdicts')
    for (const lib of libsToPlot) {
      const series = section.series.get(lib) ?? []
      const skipInfo = skippedMap.get(lib) ?? null
      const last = series.length > 0 ? series[series.length - 1] : null
      const color = LIBRARY_COLOR[lib] ?? '#A9B5C9'
      const profileUrl = profileUrlFor(lib, section.scenario, section.scale)

      const item = document.createElement('li')
      item.className = 'bench-legend-item'
      item.dataset.library = lib
      item.style.setProperty('--lib-color', color)

      // #1293: if this library has no data point but IS in the
      // skipped payload for this (scenario × scale), render the chip
      // with a strikethrough on the library name, a small ✗ badge in
      // the verdict slot, and the runner's reason as a tooltip on
      // both the chip and the badge. The chip stays in canonical
      // order so the reader sees ALL four libraries even when one
      // can't run the workload at all.
      if (!last && skipInfo) {
        item.classList.add('bench-legend-item--skipped')
        item.dataset.skipped = 'true'
        item.dataset.verdict = 'skipped'
        item.style.setProperty('--verdict-color', VERDICT_COLOR.unknown)
        const reason = skipInfo.reason || 'skipped: no reason recorded'
        // causl-bench#37: typed SKIP rows render the verdict slot as
        // a per-reason chip (colour + label drawn from the typed
        // taxonomy) instead of the generic "skipped" pill. The
        // skipped state classes still set the dashed border + muted
        // surface; the verdict chip carries the structural reason.
        // Legacy rows render the original generic chip (back-compat
        // for un-migrated history).
        const typed = classifyTypedSkip(skipInfo.typedReason)
        const tooltip =
          typed && skipInfo.detail
            ? `${skipInfo.detail}\nlibrary: ${getLibraryLabel(lib)}`
            : reason
        const tooltipAttr = escapeHtml(tooltip)
        const skippedLabel = getLibraryLabel(lib)
        const verdictChipClass = typed
          ? `bench-legend-verdict bench-legend-verdict--skip-typed bench-legend-skip-reason-${typed.cls}`
          : 'bench-legend-verdict bench-legend-verdict--skipped'
        const verdictBadgeGlyph = typed ? '▢' : '✗'
        const verdictLabel = typed ? typed.label : 'skipped'
        if (typed) item.dataset.typedReason = skipInfo.typedReason
        item.innerHTML =
          `<span class="bench-legend-swatch" aria-hidden="true"></span>` +
          `<span class="bench-legend-lib bench-legend-lib--strike" ` +
          `title="${tooltipAttr}">` +
          `<s>${escapeHtml(skippedLabel)}</s>` +
          `</span>` +
          `<span class="bench-legend-stats bench-legend-stats--muted">` +
          `<span class="bench-legend-median">n/a ms</span>` +
          `<span class="bench-legend-p95">p95 n/a</span>` +
          `</span>` +
          `<span class="${verdictChipClass}" ` +
          `title="${tooltipAttr}" aria-label="skipped: ${tooltipAttr}">` +
          `<span class="bench-legend-verdict-badge" aria-hidden="true">${verdictBadgeGlyph}</span>` +
          `<span class="bench-legend-verdict-label">${escapeHtml(verdictLabel)}</span>` +
          `</span>` +
          `<a class="bench-legend-profile-link" href="${profileUrl}" rel="noopener" ` +
          `aria-label="Profile artifacts for ${escapeHtml(lib)} ${escapeHtml(section.scenario)} at scale ${section.scale} (skipped, see legend tooltip)" ` +
          `title="${tooltipAttr}">⌕</a>`
        legend.appendChild(item)
        continue
      }

      const verdict = computeVerdict(series)
      const verdictColor = VERDICT_COLOR[verdict.kind] ?? VERDICT_COLOR.unknown
      const lastMedian = last ? formatNumber(last.medianMs) : 'n/a'
      const lastP95 = last ? formatNumber(last.p95Ms) : 'n/a'
      const label = getLibraryLabel(lib)
      const annotation = LIBRARY_ANNOTATION[lib]
      const libNameTitle = annotation
        ? ` title="${escapeHtml(annotation)}"`
        : ''
      const libNameMarker = annotation
        ? ' bench-legend-lib--annotated'
        : ''
      item.dataset.verdict = verdict.kind
      item.style.setProperty('--verdict-color', verdictColor)
      if (annotation) {
        item.dataset.annotated = 'true'
        item.dataset.annotation = annotation
      }
      item.innerHTML =
        `<span class="bench-legend-swatch" aria-hidden="true"></span>` +
        `<span class="bench-legend-lib${libNameMarker}"${libNameTitle}>` +
        `${escapeHtml(label)}` +
        (annotation
          ? ` <span class="bench-legend-lib-note" aria-hidden="true">ⓘ</span>`
          : '') +
        `</span>` +
        `<span class="bench-legend-stats">` +
        `<span class="bench-legend-median">${lastMedian} ms</span>` +
        `<span class="bench-legend-p95">p95 ${lastP95}</span>` +
        `</span>` +
        `<span class="bench-legend-verdict" title="${escapeHtml(verdict.detail)}">` +
        `<span class="bench-legend-verdict-dot" aria-hidden="true"></span>` +
        `<span class="bench-legend-verdict-label">${verdict.kind}</span>` +
        `</span>` +
        `<a class="bench-legend-profile-link" href="${profileUrl}" rel="noopener" ` +
        `aria-label="Profile artifacts for ${escapeHtml(label)} ${escapeHtml(section.scenario)} at scale ${section.scale} (placeholder, populated by #709 once CI is restored)" ` +
        `title="profile snapshot → ${profileUrl} (placeholder until #709 nightly publish lands)">⌕</a>`
      legend.appendChild(item)
    }
    card.appendChild(legend)

    return card
  }

  /** Group sections by scenario for visual grouping in the page:
   *  every (scenario × scale) belongs to its scenario's group. */
  function groupByScenario(sections) {
    const groups = new Map()
    for (const section of sections) {
      let bucket = groups.get(section.scenario)
      if (!bucket) {
        bucket = []
        groups.set(section.scenario, bucket)
      }
      bucket.push(section)
    }
    return groups
  }

  /** ----------------------------------------------------------------
   *  Filter UI
   *  ---------------------------------------------------------------- */

  /** Discover the universe of (library, scenario, scale) values and
   *  seed the filterState with the default-view subset. */
  function seedFilterState(allSections) {
    const allLibs = new Set()
    const allScenarios = new Set()
    const allScales = new Set()
    for (const s of allSections) {
      allScenarios.add(s.scenario)
      allScales.add(s.scale)
      for (const lib of s.series.keys()) allLibs.add(lib)
      // #1293: skipped libraries also count toward the universe so
      // the filter UI offers a checkbox for them (a framework that's
      // ONLY ever skipped at the visible scales would otherwise be
      // unreachable from the filter strip).
      if (s.skipped instanceof Map) {
        for (const lib of s.skipped.keys()) allLibs.add(lib)
      }
    }

    // Defaults: causl-ts + causl-wasm + mobx (where present), 10000
    // scale, all scenarios on. Fall back gracefully if the universe
    // is smaller than the spec defaults (e.g. no 10k-scale fixture).
    const defaultLibs = new Set(
      [...DEFAULT_LIBRARIES].filter((l) => allLibs.has(l)),
    )
    if (defaultLibs.size === 0) {
      // Nothing in the default set survived, so open the gate and let
      // the user see something rather than an empty grid.
      for (const l of allLibs) defaultLibs.add(l)
    }

    const defaultScales = new Set(
      [...DEFAULT_SCALES].filter((s) => allScales.has(s)),
    )
    if (defaultScales.size === 0) {
      for (const s of allScales) defaultScales.add(s)
    }

    filterState.libraries = defaultLibs
    filterState.scenarios = new Set(allScenarios)
    filterState.scales = defaultScales

    // Stash the universes on the state object so the UI can render
    // checkboxes for them.
    filterState._allLibraries = LIBRARY_ORDER.filter((l) => allLibs.has(l))
      .concat([...allLibs].filter((l) => !LIBRARY_ORDER.includes(l)).sort())
    filterState._allScenarios = [...allScenarios].sort()
    filterState._allScales = [...allScales].sort((a, b) => a - b)
  }

  /** Apply current filterState to the universe of sections. We
   *  filter by scenario + scale at the section level; library
   *  filtering happens inside the section renderer (it controls
   *  which lines are drawn, not which sections appear). */
  function applyFilters(allSections) {
    return allSections.filter((s) =>
      filterState.scenarios.has(s.scenario) &&
      filterState.scales.has(s.scale),
    )
  }

  /** Build the filter UI. The caller wires the `onChange` callback
   *  to the renderer so toggling a checkbox triggers a re-render. */
  function renderFilterUI(host, onChange) {
    host.innerHTML = ''

    const fieldset = (legendText, ariaLabel, items, getKey, getLabel, stateSet) => {
      const fs = document.createElement('fieldset')
      fs.className = 'filter-group'
      fs.setAttribute('aria-label', ariaLabel)

      const legend = document.createElement('legend')
      legend.className = 'filter-legend'
      legend.textContent = legendText
      fs.appendChild(legend)

      const list = document.createElement('div')
      list.className = 'filter-list'

      for (const item of items) {
        const key = getKey(item)
        const label = document.createElement('label')
        label.className = 'filter-chip'
        const cb = document.createElement('input')
        cb.type = 'checkbox'
        cb.value = String(key)
        cb.checked = stateSet.has(key)
        cb.setAttribute(
          'aria-label',
          `${legendText.toLowerCase()} ${getLabel(item)}`,
        )
        cb.addEventListener('change', () => {
          if (cb.checked) stateSet.add(key)
          else stateSet.delete(key)
          onChange()
        })
        const span = document.createElement('span')
        span.className = 'filter-chip-text'
        span.textContent = getLabel(item)
        label.appendChild(cb)
        label.appendChild(span)
        list.appendChild(label)
      }

      fs.appendChild(list)
      return fs
    }

    const libFs = fieldset(
      'Library',
      'Filter by library (controls which lines render on each chart)',
      filterState._allLibraries,
      (l) => l,
      (l) => getLibraryLabel(l),
      filterState.libraries,
    )

    const scnFs = fieldset(
      'Scenario',
      'Filter by scenario',
      filterState._allScenarios,
      (s) => s,
      (s) => s,
      filterState.scenarios,
    )

    const scaleFs = fieldset(
      'Scale',
      'Filter by scale (node count)',
      filterState._allScales,
      (s) => s,
      (s) => s.toLocaleString('en-US'),
      filterState.scales,
    )

    host.appendChild(libFs)
    host.appendChild(scnFs)
    host.appendChild(scaleFs)

    // The era switch. A single checkbox rather than a fieldset: it is
    // one boolean about the x-axis, not a set membership question.
    if (eraBoundaryCapturedAt) {
      const eraFs = document.createElement('fieldset')
      eraFs.className = 'filter-group'
      eraFs.setAttribute('aria-label', 'Show runs from before the current library set')
      const eraLegend = document.createElement('legend')
      eraLegend.className = 'filter-legend'
      eraLegend.textContent = 'History'
      eraFs.appendChild(eraLegend)
      const eraList = document.createElement('div')
      eraList.className = 'filter-list'
      const eraLabel = document.createElement('label')
      eraLabel.className = 'filter-chip'
      const eraCb = document.createElement('input')
      eraCb.type = 'checkbox'
      eraCb.checked = filterState.fullHistory
      eraCb.addEventListener('change', () => {
        filterState.fullHistory = eraCb.checked
        onChange()
      })
      const eraText = document.createElement('span')
      eraText.textContent = `include runs before ${formatShortDate(eraBoundaryCapturedAt)}`
      eraLabel.title =
        'Runs before this date measured a different set of libraries: the wasm ' +
        'engine was not measured until then, and redux was published under a ' +
        'different runner id. Their series are real, they are just not ' +
        'comparable across the boundary, so the charts leave them out by ' +
        'default and mark the boundary when you switch them on.'
      eraLabel.appendChild(eraCb)
      eraLabel.appendChild(eraText)
      eraList.appendChild(eraLabel)
      eraFs.appendChild(eraList)
      host.appendChild(eraFs)
    }

    // "Reset" / "All on" affordances: small text buttons so power
    // users can flip the gate without clicking through every chip.
    const actions = document.createElement('div')
    actions.className = 'filter-actions'

    const allBtn = document.createElement('button')
    allBtn.type = 'button'
    allBtn.className = 'filter-action'
    allBtn.textContent = 'All cells on'
    allBtn.setAttribute('aria-label', 'Enable every library, scenario, and scale')
    allBtn.addEventListener('click', () => {
      for (const l of filterState._allLibraries) filterState.libraries.add(l)
      for (const s of filterState._allScenarios) filterState.scenarios.add(s)
      for (const s of filterState._allScales) filterState.scales.add(s)
      onChange()
      renderFilterUI(host, onChange)
    })

    const defaultBtn = document.createElement('button')
    defaultBtn.type = 'button'
    defaultBtn.className = 'filter-action'
    defaultBtn.textContent = 'Default view (causl-ts + causl-wasm + mobx @ 10k)'
    defaultBtn.setAttribute(
      'aria-label',
      'Restore the default view: causl-ts, causl-wasm and mobx at scale 10000',
    )
    defaultBtn.addEventListener('click', () => {
      filterState.libraries = new Set(
        [...DEFAULT_LIBRARIES].filter((l) => filterState._allLibraries.includes(l)),
      )
      filterState.scenarios = new Set(filterState._allScenarios)
      filterState.scales = new Set(
        [...DEFAULT_SCALES].filter((s) => filterState._allScales.includes(s)),
      )
      onChange()
      renderFilterUI(host, onChange)
    })

    actions.appendChild(defaultBtn)
    actions.appendChild(allBtn)
    host.appendChild(actions)
  }

  /** ----------------------------------------------------------------
   *  Top-level renderer
   *  ---------------------------------------------------------------- */
  function renderGrid(gridHost, allSections) {
    gridHost.innerHTML = ''
    const visible = applyFilters(allSections)
    if (visible.length === 0) {
      const empty = document.createElement('p')
      empty.className = 'dashboard-empty'
      empty.textContent =
        'No sections match the current filters. Use the toggles above to widen the view.'
      gridHost.appendChild(empty)
      return
    }

    const groups = groupByScenario(visible)
    for (const [scenario, scenarioSections] of groups) {
      const groupEl = document.createElement('section')
      groupEl.className = 'bench-section-group'
      groupEl.setAttribute('aria-label', `Scenario: ${scenario}`)

      const title = document.createElement('h2')
      title.className = 'bench-section-group-title'
      title.textContent = scenario
      groupEl.appendChild(title)

      const grid = document.createElement('div')
      grid.className = 'bench-section-grid'
      for (const section of scenarioSections) {
        grid.appendChild(renderSectionCard(section, filterState.libraries))
      }
      groupEl.appendChild(grid)
      gridHost.appendChild(groupEl)
    }
  }

  function renderDashboard(host, history, sourceLabel) {
    host.innerHTML = ''

    // Top-line meta strip: number of runs, last-run timestamp, source.
    const meta = document.createElement('div')
    meta.className = 'dashboard-meta'
    // Runs that put something on a chart, not entries in the file. Deleting the
    // two withdrawn wasm series emptied three 2026-05 entries whose only
    // content was those series, and counting them here would claim more data
    // than the grid below can show.
    const runCount = history.filter(
      (e) => (e.samples ?? []).length > 0 || (e.skipped ?? []).length > 0,
    ).length
    const last = history[history.length - 1]
    const lastWhen = last ? formatShortDate(last.capturedAt) : 'n/a'
    const lastVersion = last ? last.version : 'n/a'
    meta.innerHTML =
      `<div><strong>Runs</strong> ${runCount}</div>` +
      `<div><strong>Latest</strong> ${escapeHtml(lastWhen)}</div>` +
      `<div><strong>Version</strong> <code>${escapeHtml(lastVersion)}</code></div>` +
      `<div><strong>Source</strong> ${escapeHtml(sourceLabel)}</div>`
    host.appendChild(meta)

    // The comparable era. Sections are rebuilt whenever this changes,
    // because the window decides which runs share the x-axis.
    const eraStart = comparableEraStart(history)
    const eraBoundaryAt = eraStart > 0 ? history[eraStart].capturedAt : null
    eraBoundaryCapturedAt = eraBoundaryAt
    const windowed = () =>
      filterState.fullHistory || eraStart === 0 ? history : history.slice(eraStart)

    const allSections = sortSections(groupSections(windowed()))
    if (allSections.length === 0) {
      const empty = document.createElement('p')
      empty.className = 'dashboard-empty'
      empty.textContent = 'history.json had no samples to plot.'
      host.appendChild(empty)
      return
    }

    seedFilterState(allSections)

    // Filter UI inside a <details> so it folds out of the way on
    // narrow viewports while staying keyboard-reachable.
    const filterShell = document.createElement('details')
    filterShell.className = 'filter-shell'
    filterShell.open = true
    const filterSummary = document.createElement('summary')
    filterSummary.className = 'filter-summary'
    filterSummary.textContent = 'Filters'
    filterShell.appendChild(filterSummary)

    const filterHost = document.createElement('div')
    filterHost.className = 'filter-host'
    filterHost.setAttribute('role', 'group')
    filterHost.setAttribute('aria-label', 'Dashboard filters')
    filterShell.appendChild(filterHost)
    host.appendChild(filterShell)

    // Legend strip for the verdict badges.
    const legend = document.createElement('div')
    legend.className = 'verdict-legend'
    legend.setAttribute('aria-label', 'Regression verdict legend')
    legend.innerHTML =
      `<span class="verdict-legend-label">Verdict</span>` +
      Object.entries(VERDICT_COLOR)
        .filter(([k]) => k !== 'unknown')
        .map(
          ([k, c]) =>
            `<span class="verdict-legend-item" data-kind="${k}">` +
            `<span class="verdict-legend-dot" style="--verdict-color:${c}"></span>` +
            `<span class="verdict-legend-text">${k}</span>` +
            `</span>`,
        )
        .join('')
    host.appendChild(legend)

    const gridHost = document.createElement('div')
    gridHost.className = 'dashboard-grid'
    host.appendChild(gridHost)

    // Toggling the era changes which runs share the x-axis, so the
    // sections are rebuilt rather than re-filtered. Every other filter
    // only decides what is drawn from sections already built.
    let sections = allSections
    let lastFullHistory = filterState.fullHistory
    const onChange = () => {
      if (filterState.fullHistory !== lastFullHistory) {
        lastFullHistory = filterState.fullHistory
        sections = sortSections(groupSections(windowed()))
        seedFilterState(sections)
      }
      renderGrid(gridHost, sections)
    }
    renderFilterUI(filterHost, onChange)
    renderGrid(gridHost, sections)
  }

  /** Try the live URL, fall back to the sample stub. */
  async function loadHistory() {
    const tryFetch = async (url) => {
      const res = await fetch(url, { cache: 'no-cache' })
      if (!res.ok) {
        const err = new Error(`HTTP ${res.status} for ${url}`)
        err.status = res.status
        throw err
      }
      return res.json()
    }

    try {
      const data = await tryFetch(HISTORY_URL)
      return { data, sourceLabel: 'history.json (live)' }
    } catch {
      const data = await tryFetch(SAMPLE_URL)
      return { data, sourceLabel: 'history.sample.json (stub)' }
    }
  }

  /** Public bootstrap. Called from index.html once the DOM is ready. */
  async function bootstrap() {
    const host = document.querySelector('#dashboard-root')
    if (!host) return

    host.innerHTML = '<p class="dashboard-loading">Loading history…</p>'

    try {
      const { data, sourceLabel } = await loadHistory()
      if (!Array.isArray(data)) {
        throw new Error('history JSON was not an array of HistoryEntry')
      }
      renderDashboard(host, data, sourceLabel)
    } catch (err) {
      host.innerHTML =
        `<p class="dashboard-error">` +
        `Could not load benchmark history: ${escapeHtml((err && err.message) || String(err))}.` +
        `</p>`
    } finally {
      host.setAttribute('aria-busy', 'false')
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap)
  } else {
    bootstrap()
  }
})()
