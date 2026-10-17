// The parameterised dashboard is for a project that is not causl, so the words it
// puts in front of a reader must not name causl's series.
//
// The "Default view" button is the case that got through: its label and its
// aria-label are string literals in the generated file, written for causl, and the
// config has no key for them. On libviprs-org's benchmarks page it reads
// "Default view (causl-ts + causl-wasm + mobx @ 10k)" over a chart of libviprs
// series. Red until the labels come from the config's own series and defaults.

import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const GENERATED = readFileSync(join(here, 'generated', 'dashboard.js'), 'utf8');

test('no user-visible string in the generated dashboard names causl, mobx or redux series', () => {
  // textContent and aria-label assignments are what a reader or a screen reader meets.
  const lines = GENERATED.split('\n');
  const hits = [];
  lines.forEach((line, i) => {
    const visible = /textContent\s*=\s*['`"]/.test(line) || /aria-label/.test(line);
    const next = lines[i + 1] ?? '';
    const text = visible ? `${line}\n${/aria-label/.test(line) ? next : ''}` : '';
    if (visible && /\b(causl-(ts|wasm)|mobx|redux)\b/i.test(text)) hits.push(`line ${i + 1}: ${line.trim()}`);
  });
  assert.deepEqual(hits, []);
});

// The scan above reads source. This one reads what a page built from config.json
// actually shows, and what upstream's own config-less page still shows.
import { render } from './lib/render-dashboard.mjs';

const json = (p) => JSON.parse(readFileSync(join(here, p), 'utf8'));

test('the Default view button is named from the config, and falls back to upstream without one', async () => {
  const history = json('upstream/history.sample.json');
  const config = json('config.json');
  const withConfig = await render(GENERATED, { history, config, filename: 'generated.js' });
  const labels = config.defaults.libraries.map((l) => config.series.label[l]);
  assert.ok(
    withConfig.html.includes(`Default view (${labels.join(' + ')} @ ${config.defaults.scales.join(', ')})`),
    'the button text is built from the default libraries and scales',
  );
  assert.ok(withConfig.html.includes(`Restore the default view: ${labels.join(' and ')} at scale`));
  assert.doesNotMatch(withConfig.html, /Default view \([^)]*(causl|mobx)/);

  const bare = await render(GENERATED, { history, filename: 'generated.js' });
  assert.ok(bare.html.includes('Default view (causl-ts + causl-wasm + mobx @ 10k)'));
});
