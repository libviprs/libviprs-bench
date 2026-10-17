// Every piece of text a chart draws has to meet WCAG AA (4.5:1) against the
// surface it is drawn on, in both colour schemes the chart ships.
//
// The charts carry their palette as CSS variables and switch on
// prefers-color-scheme, so the pairs are read out of the SVG itself rather than
// assumed. Measured, not eyeballed: relative luminance per WCAG 2.x.
//
// Written before the fix for the axis tick labels, which use --bc-muted
// (#898781) and sit at 3.59:1 on the light surface. Red until the token moves.

import test from 'node:test';
import assert from 'node:assert/strict';
import { buildStorageCharts } from './storage-charts.mjs';
import { loadContract, CONTRACT_PATH } from './comparability.mjs';

const contract = loadContract(CONTRACT_PATH);

const cell = (over = {}) => ({
  backend: 'directory', scenario: 'read_random', metric: 'p50', unit: 'us',
  direction: 'lower-is-better', cell: '2048x2048@256+gradient', scale: 93,
  source: 'gradient', outcome: 'ok', median: 10, ci95: [9.5, 10.6], confidence: 'high', ...over,
});

const hex = (h) => {
  const s = h.length === 4 ? [...h.slice(1)].map((c) => c + c).join('') : h.slice(1);
  return [0, 2, 4].map((i) => parseInt(s.slice(i, i + 2), 16));
};
const lum = (rgb) => {
  const [r, g, b] = rgb.map((c) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const ratio = (a, b) => {
  const [hi, lo] = [lum(hex(a)), lum(hex(b))].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};

function schemes(svg) {
  const style = /<style>([\s\S]*?)<\/style>/.exec(svg)[1];
  const vars = (block) => Object.fromEntries([...block.matchAll(/(--bc-[a-z-]+):\s*(#[0-9a-f]{3,6})/gi)].map((m) => [m[1], m[2]]));
  const base = vars(/^:root\{([^}]*)\}/.exec(style)[1]);
  const dark = { ...base, ...vars(/@media \(prefers-color-scheme: dark\)\{[^{]*\{([^}]*)\}/.exec(style)[1]) };
  return { light: base, dark };
}

test('chart text meets 4.5:1 against the chart surface in the light and dark schemes', () => {
  const { charts } = buildStorageCharts({
    family: 'libviprs-storage',
    cells: [cell(), cell({ backend: 'pmtiles', median: 7 })],
    invariants: [],
  }, { contract });
  assert.equal(charts.length, 1);
  const svg = charts[0].svg;
  const tokens = schemes(svg);
  const fills = new Set([...svg.matchAll(/<text\b[^>]*\bfill="([^"]+)"/g)].map((m) => m[1]));
  assert.ok(fills.size > 0, 'the chart draws no text, so there is nothing to measure');

  const failures = [];
  for (const [scheme, t] of Object.entries(tokens)) {
    for (const fill of fills) {
      const token = /^var\((--[a-z-]+)\)$/.exec(fill)?.[1];
      const fg = token ? t[token] : fill;
      const r = ratio(fg, t['--bc-surface']);
      if (r < 4.5) failures.push(`${scheme}: text ${fill} (${fg}) on ${t['--bc-surface']} is ${r.toFixed(2)}:1, under 4.5:1`);
    }
  }
  assert.deepEqual(failures, []);
});
