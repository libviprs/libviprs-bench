// libviprs-bench#117: every piece of chart text meets 4.5:1 in both schemes.
// The chart is drawn inline so the browser resolves the palette itself, then the
// computed fill of each <text> is measured against the computed surface fill.
import { test, expect } from '@playwright/test';
import { buildStorageCharts } from '../charts/storage-charts.mjs';
import { loadContract, CONTRACT_PATH } from '../charts/comparability.mjs';

const OUT = process.env.E2E_OUT || './test-results';
const contract = loadContract(CONTRACT_PATH);
const cell = (over = {}) => ({
  backend: 'directory', scenario: 'read_random', metric: 'p50', unit: 'us',
  direction: 'lower-is-better', cell: '2048x2048@256+gradient', scale: 93,
  source: 'gradient', outcome: 'ok', median: 10, ci95: [9.5, 10.6], confidence: 'high', ...over,
});
const { charts } = buildStorageCharts({
  family: 'libviprs-storage',
  cells: [cell(), cell({ backend: 'pmtiles', median: 7 })],
  invariants: [],
}, { contract });
const svg = charts[0].svg;

for (const scheme of ['light', 'dark']) {
  test(`117: all chart text is at least 4.5:1 in the ${scheme} scheme`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.setContent(`<body style="margin:0">${svg}</body>`);
    const rows = await page.evaluate(() => {
      const lum = (c) => {
        const [r, g, b] = c.match(/[\d.]+/g).slice(0, 3).map((v) => {
          const s = Number(v) / 255;
          return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
        });
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
      };
      const bg = getComputedStyle(document.querySelector('rect.bc-surface')).fill;
      return [...document.querySelectorAll('svg text')].map((t) => {
        const fg = getComputedStyle(t).fill;
        const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
        return { text: t.textContent, fg, ratio: (hi + 0.05) / (lo + 0.05) };
      });
    });
    expect(rows.length).toBeGreaterThan(0);
    const low = rows.filter((r) => r.ratio < 4.5).map((r) => `${r.text} ${r.fg} ${r.ratio.toFixed(2)}`);
    expect(low).toEqual([]);
    await page.locator('svg').screenshot({ path: `${OUT}/117-frame-${scheme}.png` });
  });
}
