/**
 * The series identity is injectable (#104).
 *
 * `chart.mjs` drew exactly one thing for as long as it existed: the four
 * libviprs engines, from a module-level `ENGINES` const. That was fine while
 * the only family was `engines`. It stopped being fine when `storage` grew
 * eleven scenarios with no chart, and it blocks the `format` family (#102)
 * and libviprs-org#91 outright.
 *
 * Two things were baked in, and both are tested here:
 *   1. WHICH series exist, in what order, with what colours and labels;
 *   2. the FIELD the series key lives in. `orderedEngines` read `p.engine`
 *      directly, so a `storage` sample keyed on `backend` could not be drawn
 *      at all.
 *
 * The property that must survive the refactor, and the reason it is pinned
 * here rather than left to the existing suite: a series present in the data
 * but absent from the canonical list is drawn with a deterministic fallback
 * colour chosen by sorted position, so it is never silently dropped and the
 * output stays byte-stable.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  createSeriesTheme,
  ENGINE_THEME,
  ENGINE_ORDER,
  COLORS,
  ENGINE_LABELS,
} from './chart.mjs';

test('the default theme is the engines one, and the old exports still name it', () => {
  assert.equal(ENGINE_THEME.seriesKey, 'engine');
  assert.deepEqual(ENGINE_THEME.order, ENGINE_ORDER);
  assert.deepEqual(ENGINE_THEME.colors, COLORS);
  assert.deepEqual(ENGINE_THEME.labels, ENGINE_LABELS);

  // The back-compat contract: every existing import keeps working unchanged.
  assert.deepEqual(ENGINE_ORDER, ['libvips', 'monolithic', 'streaming', 'mapreduce']);
  assert.equal(COLORS.monolithic, '#4285f4');
  assert.equal(ENGINE_LABELS.mapreduce, 'MapReduce');
});

test('a theme can key on a field other than engine', () => {
  const storage = createSeriesTheme({
    seriesKey: 'backend',
    series: [
      { key: 'pmtiles', label: 'PMTiles', color: '#2b6cb0' },
      { key: 'tree', label: 'Directory tree', color: '#b7791f' },
    ],
  });

  assert.equal(storage.seriesKey, 'backend');
  assert.deepEqual(storage.order, ['pmtiles', 'tree']);
  assert.equal(storage.labelFor('pmtiles'), 'PMTiles');
  assert.equal(storage.colorFor('tree', storage.order), '#b7791f');

  // The point of the whole change: points keyed on `backend`, not `engine`.
  const points = [{ backend: 'tree' }, { backend: 'pmtiles' }];
  assert.deepEqual(storage.ordered(points), ['pmtiles', 'tree']);
});

test('a series in the data but not in the theme is drawn, not dropped', () => {
  const theme = createSeriesTheme({
    seriesKey: 'format',
    series: [{ key: 'png', label: 'PNG', color: '#2b6cb0' }],
  });

  const points = [{ format: 'webp' }, { format: 'png' }, { format: 'jpeg' }];
  const ordered = theme.ordered(points);

  // Canonical first, then the extras in SORTED order, so output is stable.
  assert.deepEqual(ordered, ['png', 'jpeg', 'webp']);

  // Each extra gets a real colour rather than undefined.
  for (const key of ordered) {
    const c = theme.colorFor(key, ordered);
    assert.match(c, /^#[0-9a-f]{6}$/i, `${key} must get a colour, got ${c}`);
  }

  // And an unlabelled series falls back to its own key rather than blank.
  assert.equal(theme.labelFor('webp'), 'webp');
});

test('fallback colours are assigned by sorted position, so output is byte-stable', () => {
  const theme = createSeriesTheme({
    seriesKey: 'format',
    series: [{ key: 'png', label: 'PNG', color: '#2b6cb0' }],
  });

  // Same set of extras, two different input orders: same colour assignment.
  const a = theme.ordered([{ format: 'webp' }, { format: 'jpeg' }]);
  const b = theme.ordered([{ format: 'jpeg' }, { format: 'webp' }]);
  assert.deepEqual(a, b);
  assert.equal(theme.colorFor('jpeg', a), theme.colorFor('jpeg', b));
  assert.equal(theme.colorFor('webp', a), theme.colorFor('webp', b));

  // Two distinct extras must not collide onto one colour, or the chart lies.
  assert.notEqual(theme.colorFor('jpeg', a), theme.colorFor('webp', a));
});

test('the engines theme still resolves its own series exactly as before', () => {
  const points = [{ engine: 'streaming' }, { engine: 'libvips' }];
  assert.deepEqual(ENGINE_THEME.ordered(points), ENGINE_ORDER);
  assert.equal(ENGINE_THEME.colorFor('libvips', ENGINE_ORDER), '#9c27b0');
  assert.equal(ENGINE_THEME.labelFor('streaming'), 'Streaming');
});
