// The two behaviours upstream has no equivalent of, rendered against a history
// built from the first real libviprs capture.
//
//   1. `gated: false` on a sample renders "measured, not gated" and never a
//      verdict chip.
//   2. A delta smaller than the cell's own replicate spread is `noise`: not a
//      pass, not a regression.
//
// Neither is hypothetical. Run 20260914T145707Z measured a median replicate
// spread of 3.46% across 48 metrics with `directory.read_concurrent@4.
// lookups_per_s` at 53.3%, and 106 of its 332 measured cells came back
// low-confidence because they sit below the 4.1 us the timer can be trusted
// over. On that metric upstream's 10% line rules a 20% move a regression, and
// a 20% move is what an idle machine hands you.
//
// Each test names the wrong implementation it goes red against.

import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { render } from './lib/render-dashboard.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const read = (p) => readFileSync(join(here, p), 'utf8');
const json = (p) => JSON.parse(read(p));

const FROZEN = read('upstream/dashboard.js');
const GENERATED = read('generated/dashboard.js');
const CONFIG = json('config.json');
const HISTORY = json('fixtures/history.libviprs.json');

/** Every legend chip in a render, as {library, scenario, verdict, chipText}. */
function chips(html) {
  const out = [];
  const cardRe = /<article class="bench-section-card"[^>]*data-scenario="([^"]*)"[^>]*>/g;
  let m;
  const bounds = [];
  while ((m = cardRe.exec(html)) !== null) bounds.push([m.index, m[1]]);
  for (let i = 0; i < bounds.length; i += 1) {
    const [start, scenario] = bounds[i];
    const end = i + 1 < bounds.length ? bounds[i + 1][0] : html.length;
    const card = html.slice(start, end);
    const itemRe = /<li class="bench-legend-item"[^>]*data-library="([^"]*)"[^>]*data-verdict="([^"]*)"[\s\S]*?<\/li>/g;
    let n;
    while ((n = itemRe.exec(card)) !== null) {
      const body = n[0];
      const chip = /<span class="bench-legend-verdict"[^>]*>[\s\S]*?<span class="bench-legend-verdict-label">([^<]*)<\/span>/.exec(body);
      const measured = /<span class="bench-legend-measured"[^>]*>[\s\S]*?<span class="bench-legend-measured-label">([^<]*)<\/span>/.exec(body);
      out.push({
        scenario,
        library: n[1],
        verdict: n[2],
        chip: chip ? chip[1] : null,
        measured: measured ? measured[1] : null,
      });
    }
  }
  return out;
}

const withConfig = await render(GENERATED, { history: HISTORY, config: CONFIG });
const withoutConfig = await render(GENERATED, { history: HISTORY });
const frozen = await render(FROZEN, { history: HISTORY });

const find = (set, library, scenarioPrefix) =>
  set.find((c) => c.library === library && c.scenario.startsWith(scenarioPrefix));

test('the fixture is what it says it is: run 1 real, run 2 constructed and labelled', () => {
  assert.equal(HISTORY.length, 2);
  assert.equal(HISTORY[0].constructed, false);
  assert.equal(
    HISTORY[0].derivedFrom.documentDigest,
    'sha256:6a712af369d3229bb359a13bc3b6ece1bd3bff02257f540f85c0e0f1b0cf49d3',
    'run 1 is no longer the archived capture it claims to be',
  );
  assert.equal(HISTORY[1].constructed, true);
  assert.match(HISTORY[1].constructedWhy, /never be imported into a published history/);
  // Only series the producer actually emits.
  const libs = new Set(HISTORY.flatMap((e) => e.samples.map((s) => s.library)));
  assert.deepEqual([...libs].sort(), ['directory', 'pmtiles']);
});

// Goes red against: a noise rule that reads a single global threshold rather
// than the cell's own spread. `directory.read_concurrent@4.lookups_per_s` moved
// 20% and its spread is 53.3%, so it is noise. `directory.read_random.p50`
// moved 25% and its spread is 3.4%, so it is not. One number cannot produce
// both answers, which is the whole point.
test('a delta inside the cell replicate spread is noise, and one outside it is not', () => {
  const c = chips(withConfig.html);
  const noisy = find(c, 'directory', 'read_concurrent@4.lookups_per_s');
  const real = find(c, 'directory', 'read_random.p50');
  assert.ok(noisy, 'the 53.3%-spread cell did not render');
  assert.ok(real, 'the 3.4%-spread cell did not render');
  assert.equal(noisy.verdict, 'noise', `expected noise, got ${noisy.verdict} (${noisy.chip})`);
  assert.equal(real.verdict, 'regressed', `expected regressed, got ${real.verdict}`);
  assert.match(noisy.chip ?? '', /noise/);
});

// Goes red against: the frozen dashboard, which is exactly the point. This is
// the control that shows the noise rule is doing work rather than agreeing
// with what would have happened anyway.
test('without the config the same 20% move on the same cell is called a regression', () => {
  const c = chips(frozen.html);
  const same = find(c, 'directory', 'read_concurrent@4.lookups_per_s');
  assert.ok(same, 'the cell did not render through the frozen dashboard');
  assert.equal(
    same.verdict,
    'regressed',
    'upstream no longer rules this a regression, so the noise rule is not changing anything',
  );
  // And the parameterised file with no config agrees with the frozen one.
  const c2 = chips(withoutConfig.html);
  assert.deepEqual(chips(frozen.html), c2);
});

// Goes red against: a spread that is treated as absent-means-pass. A cell with
// no measured spread must not be ruled on, because a spread that was not
// measured is not a spread.
test('a cell with no measured spread is unknown, not pass and not regressed', () => {
  const c = chips(withConfig.html);
  const nospread = find(c, 'directory', 'generate.wall');
  assert.ok(nospread);
  assert.equal(nospread.verdict, 'unknown');
  assert.match(nospread.chip ?? '', /unknown/);
});

// Goes red against: an ungated sample that still gets a chip. The chip is the
// page's way of saying a threshold ruled on the number, and when nothing gated
// the cell no threshold did.
test('gated:false renders "measured, not gated" and no verdict chip at all', () => {
  const c = chips(withConfig.html);
  const ungated = find(c, 'pmtiles', 'generate.wall');
  assert.ok(ungated, 'the ungated cell did not render');
  assert.equal(ungated.verdict, 'ungated');
  assert.equal(ungated.measured, 'measured, not gated');
  assert.equal(ungated.chip, null, 'an ungated cell still rendered a verdict chip');
  // The cell moved 40%, so without the gate rule it would have been a loud
  // regression. Silence here is a decision, not an accident.
  const frozenSame = find(chips(frozen.html), 'pmtiles', 'generate.wall');
  assert.equal(frozenSame.verdict, 'regressed');
  assert.equal(frozenSame.measured, null);
});

// Goes red against: `gated: null` treated as `gated: false`. No cell in the
// archived capture carries the field at all, so if absent counted as ungated
// the whole page would go silent the day it was wired up.
test('only the literal false is ungated: null and absent are not', () => {
  const c = chips(withConfig.html);
  for (const item of c) {
    if (item.library === 'pmtiles' && item.scenario.startsWith('generate.wall')) continue;
    assert.notEqual(
      item.verdict,
      'ungated',
      `${item.library}/${item.scenario} has gated:null and was still treated as ungated`,
    );
  }
  const nulls = HISTORY[1].samples.filter((s) => s.gated === null);
  assert.ok(nulls.length >= 4, 'the fixture should carry gated:null on most cells');
});

// Goes red against: a colour map that forgot the new kinds. A verdict kind with
// no colour renders in `unknown`'s grey and silently reads as "no data".
test('the new verdict kinds reach the page legend with their own colours', () => {
  assert.match(withConfig.html, /data-kind="noise"/);
  assert.match(withConfig.html, /data-kind="ungated"/);
  assert.match(withConfig.html, /#8FA2AA/);
  assert.match(withConfig.html, /#6E7A8A/);
  assert.doesNotMatch(withoutConfig.html, /data-kind="noise"/);
});

// A recorded gap rather than a claim. The frozen dashboard has no notion of
// metric direction, so a higher-is-better metric that got FASTER renders as
// `regressed` because its number went up. `sections.directionFrom` carries the
// fact; nothing at this pin reads it. K2.5 owns the fix and this test exists so
// the gap cannot be discovered by a reader of the published page instead.
test('direction is carried but not yet acted on, and that is recorded here', () => {
  const s = HISTORY[1].samples.find(
    (x) => x.library === 'pmtiles' && x.key === 'read_concurrent@4.lookups_per_s',
  );
  assert.equal(s.direction, 'higher-is-better');
  const c = find(chips(withConfig.html), 'pmtiles', 'read_concurrent@4.lookups_per_s');
  assert.equal(
    c.verdict,
    'regressed',
    'if this is no longer `regressed`, something started reading direction and this note is stale',
  );
});
