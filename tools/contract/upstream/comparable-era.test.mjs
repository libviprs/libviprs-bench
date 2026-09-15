/**
 * The comparable era: which runs share a chart's x-axis by default.
 *
 * ## The defect this closes
 *
 * `history.json` reaches back to 2026-05. The wasm engine was not
 * measured until 2026-07-29, so 28 of the 32 entries carrying
 * `op-tx-set-isolated-1k` have causl-ts, jotai, mobx and
 * redux-toolkit and NO causl-wasm. Charted across the whole range,
 * causl-ts drew a line across every run and causl-wasm a stub at the
 * right-hand edge, which reads as the suite failing to measure the
 * wasm engine rather than as a fact about when the engine started
 * being measured. Every one of those cells IS measured today: the
 * 2026-08-21 run has all five libraries on all 90 (scenario × scale)
 * groups, and there is no group where causl-ts has a result and
 * causl-wasm-ts does not.
 *
 * The fix is a window, not a back-fill. Inventing causl-wasm rows for
 * 2026-05 would re-commit the exact defect the series-id note at the
 * top of `dashboard.js` records having fixed: two wasm-labelled series
 * were DELETED from this feed because their engine label was asserted
 * rather than observed, and were deliberately not relabelled.
 *
 * ## What is asserted
 *
 * The boundary is derived from the data — the longest suffix over
 * which the measured library set does not change — and never from a
 * hard-coded date, so it stays correct the next time a library is
 * added or retired.
 */

import { test } from 'node:test'
import { strict as assert } from 'node:assert'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))

/** Lift the helper out of the browser bundle: it is a pure function of
 *  history, and importing the IIFE would need a DOM. */
function loadHelper() {
  const src = readFileSync(join(here, 'dashboard.js'), 'utf8')
  const m = src.match(/function comparableEraStart\(history\)\s*\{[\s\S]*?\n {2}\}/)
  assert.ok(m, 'comparableEraStart is no longer findable in dashboard.js')
  return new Function(`${m[0]}; return comparableEraStart`)()
}

const comparableEraStart = loadHelper()
const libsOf = (e) => [...new Set((e.samples ?? []).map((s) => s.library))].sort()
const entry = (libs) => ({ samples: libs.map((library) => ({ library })) })

test('an unchanging library set is one era, so nothing is windowed away', () => {
  const five = ['causl-ts', 'causl-wasm', 'jotai', 'mobx', 'redux-rtk']
  assert.equal(comparableEraStart([entry(five), entry(five), entry(five)]), 0)
})

test('the era starts where the library set last changed', () => {
  const old = ['causl-ts', 'jotai', 'mobx', 'redux-toolkit']
  const now = ['causl-ts', 'causl-wasm', 'jotai', 'mobx', 'redux-rtk']
  assert.equal(comparableEraStart([entry(old), entry(old), entry(now), entry(now)]), 2)
})

test('an empty entry carries no library claim and does not end the era', () => {
  // A run that measured nothing is not evidence that the library set
  // changed. Letting it break the era would window away everything
  // before an unrelated failed sweep.
  const now = ['causl-ts', 'causl-wasm']
  assert.equal(comparableEraStart([entry(now), { samples: [] }, entry(now)]), 0)
})

test('empty and degenerate histories do not throw', () => {
  assert.equal(comparableEraStart([]), 0)
  assert.equal(comparableEraStart(null), 0)
  assert.equal(comparableEraStart([{ samples: [] }]), 0)
})

test('the real history opens its era at the first run carrying causl-wasm', () => {
  const history = JSON.parse(readFileSync(join(here, 'history.json'), 'utf8'))
  const start = comparableEraStart(history)
  assert.ok(start > 0, 'the shipped history spans two library sets, so the era cannot start at 0')

  // Every run in the default window carries every library, which is
  // the whole property the window exists to guarantee.
  const era = history.slice(start)
  const expected = libsOf(history[history.length - 1])
  assert.ok(expected.includes('causl-wasm'), 'the newest run should measure the wasm engine')
  for (const e of era) {
    assert.deepEqual(
      libsOf(e),
      expected,
      `run ${e.runId ?? e.capturedAt} is inside the era but measures a different library set`,
    )
  }

  // And the run immediately before it does not, or the boundary is in
  // the wrong place.
  assert.notDeepEqual(libsOf(history[start - 1]), expected)
})

test('the window keeps the causl-wasm series whole for the cell that motivated it', () => {
  const history = JSON.parse(readFileSync(join(here, 'history.json'), 'utf8'))
  const era = history.slice(comparableEraStart(history))
  const SCENARIO = 'op-tx-set-isolated-1k'
  const runsWithCell = era.filter((e) =>
    (e.samples ?? []).some((s) => s.scenario === SCENARIO),
  )
  assert.ok(runsWithCell.length > 0, `no run in the era measured ${SCENARIO}`)
  for (const e of runsWithCell) {
    const libs = (e.samples ?? []).filter((s) => s.scenario === SCENARIO).map((s) => s.library)
    assert.ok(
      libs.includes('causl-wasm'),
      `${SCENARIO} has no causl-wasm sample in ${e.runId ?? e.capturedAt}, so the chart ` +
        'would still show a partial series inside the default window',
    )
    assert.ok(libs.includes('causl-ts'), `${SCENARIO} has no causl-ts sample either`)
  }
})
