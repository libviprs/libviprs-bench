// libviprs-bench#129: the Default view button must name this project's series.
import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', 'contract');
const read = (p) => readFileSync(join(root, p), 'utf8');
const config = JSON.parse(read('config.json'));
const history = read('upstream/history.sample.json');
const OUT = process.env.E2E_OUT || './test-results';

async function load(page, withConfig) {
  await page.route('**/history.json', (r) => r.fulfill({ contentType: 'application/json', body: history }));
  await page.route('**/history.sample.json', (r) => r.fulfill({ contentType: 'application/json', body: history }));
  await page.route('**/page.html', (r) => r.fulfill({
    contentType: 'text/html',
    body: `<!doctype html><meta charset="utf-8"><body style="font-family:system-ui;background:#fff;margin:16px">
      <style>${read('upstream/dashboard.css')}</style>
      ${withConfig ? `<script>window.LIBVIPRS_BENCH_CONFIG = ${JSON.stringify(config)}</script>` : ''}
      <div id="dashboard-root"></div><script>${read('generated/dashboard.js')}</script>`,
  }));
  await page.goto('http://example.test/page.html');
  await expect(page.locator('button.filter-action', { hasText: 'Default view' })).toBeVisible();
}

test('129: the Default view button names the configured series, in text and aria-label', async ({ page }) => {
  await load(page, true);
  const btn = page.locator('button.filter-action', { hasText: 'Default view' });
  const names = config.defaults.libraries.map((l) => config.series.label[l]);
  await expect(btn).toHaveText(new RegExp(`^Default view \\(${names.join(' \\+ ')} @ `));
  await expect(btn).not.toHaveText(/causl|mobx|redux/i);
  const aria = await btn.getAttribute('aria-label');
  expect(aria).toContain(names[0]);
  expect(aria).not.toMatch(/causl|mobx|redux/i);
  const strip = page.locator('button.filter-action').first().locator('xpath=..');
  const shot = (n) => strip.screenshot({ path: `${OUT}/129-frame-${n}.png` });
  await shot(1);
  await page.locator('button.filter-action', { hasText: 'All cells on' }).click();
  await shot(2);
  await btn.click();
  await shot(3);
  await expect(btn).toHaveText(/Default view \(/);
});

test('129: without a config the button still reads as upstream does', async ({ page }) => {
  await load(page, false);
  await expect(page.locator('button.filter-action', { hasText: 'Default view' }))
    .toHaveText('Default view (causl-ts + causl-wasm + mobx @ 10k)');
});
