// The golden render: with no config present, the parameterised dashboard emits
// exactly what the frozen one does.
//
// This is the test that makes the parameterisation safe to offer upstream. Every
// hard-coded constant became `__cfg('<path>', <the same literal>)`, and the two
// behaviours libviprs needs are both config-gated and default off. If that is
// true then causl's own page is untouched by the change, and the only way to
// show it is to render both and compare the bytes.
//
// Each test names the wrong implementation it goes red against.

import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { render } from './lib/render-dashboard.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const read = (p) => readFileSync(join(here, p), 'utf8');
const json = (p) => JSON.parse(read(p));
const sha = (s) => createHash('sha256').update(s).digest('hex');

const FROZEN = read('upstream/dashboard.js');
const GENERATED = read('generated/dashboard.js');
const SAMPLE = json('upstream/history.sample.json');
const REAL = json('upstream/history.json');
const CONFIG = json('config.json');

const GOLDEN_DIR = join(here, 'golden');
const GOLDEN_HTML = join(GOLDEN_DIR, 'dashboard.sample.html');
const GOLDEN_SHA = join(GOLDEN_DIR, 'RENDERS.sha256');
const REGEN = process.env.REGEN_GOLDEN === '1';

/** The renders every case below draws on, computed once. */
const renders = {};
test('render both dashboards over both histories', async () => {
  renders.frozenSample = await render(FROZEN, { history: SAMPLE, filename: 'frozen.js' });
  renders.generatedSample = await render(GENERATED, { history: SAMPLE, filename: 'generated.js' });
  renders.frozenReal = await render(FROZEN, { history: REAL, filename: 'frozen.js' });
  renders.generatedReal = await render(GENERATED, { history: REAL, filename: 'generated.js' });
  renders.generatedConfigured = await render(GENERATED, {
    history: SAMPLE,
    config: CONFIG,
    filename: 'generated.js',
  });
  for (const [k, v] of Object.entries(renders)) {
    assert.deepEqual(v.errors, [], `${k} logged an error while rendering`);
    assert.ok(v.html.length > 10000, `${k} rendered ${v.html.length} bytes, which is not a dashboard`);
  }
});

// Goes red against: a parameterisation that changed a default. Substituting
// `__cfg('series.color', {...})` for the literal is only safe if the fallback
// IS the literal; one transposed hex digit or one dropped series id shows up
// here as a byte difference.
test('the sample history renders byte for byte the same through both', () => {
  assert.equal(
    renders.generatedSample.html,
    renders.frozenSample.html,
    'the parameterised dashboard drew something different from the frozen one with no config set',
  );
});

// Goes red against: an era rewrite that changed which runs share the x-axis.
// The shipped history spans two library sets, so `comparableEraStart` returns
// a non-zero index on it and the whole windowing path runs. The sample history
// is one era and would not have exercised it.
test('the real history, which spans two eras, renders the same through both', () => {
  assert.ok(REAL.length > SAMPLE.length, 'history.json should be the longer feed');
  assert.equal(
    renders.generatedReal.html,
    renders.frozenReal.html,
    'the parameterised dashboard windowed the real history differently from the frozen one',
  );
});

// Goes red against: a fetch-path edit. The page falls back to the sample stub
// when history.json 404s, and announces it in the meta strip. A parameterisation
// that touched `loadHistory` would show up here and nowhere else.
test('the sample-stub fallback path renders the same through both', async () => {
  const a = await render(FROZEN, { history: REAL, sample: SAMPLE, historyStatus: 404 });
  const b = await render(GENERATED, { history: REAL, sample: SAMPLE, historyStatus: 404 });
  assert.match(a.html, /history\.sample\.json \(stub\)/);
  assert.equal(b.html, a.html);
});

// Goes red against: nothing, unless the golden itself is stale. This is the
// drift detector: it turns "the render changed" into a diff a reviewer reads
// rather than a fact nobody notices.
test('the committed golden still matches what the frozen dashboard renders', () => {
  if (REGEN) {
    mkdirSync(GOLDEN_DIR, { recursive: true });
    writeFileSync(GOLDEN_HTML, renders.frozenSample.html);
    writeFileSync(
      GOLDEN_SHA,
      [
        '# sha256 of each render golden-render.test.mjs produces. Regenerate with',
        '# REGEN_GOLDEN=1 node --test tools/contract/golden-render.test.mjs',
        `${sha(renders.frozenSample.html)}  frozen dashboard.js over history.sample.json`,
        `${sha(renders.frozenReal.html)}  frozen dashboard.js over history.json`,
        `${sha(renders.generatedConfigured.html)}  generated dashboard.js over history.sample.json with config.json`,
        '',
      ].join('\n'),
    );
  }
  assert.ok(existsSync(GOLDEN_HTML), 'no committed golden render; regenerate with REGEN_GOLDEN=1');
  assert.equal(
    renders.frozenSample.html,
    readFileSync(GOLDEN_HTML, 'utf8'),
    'the frozen dashboard renders something other than the committed golden',
  );

  const lines = readFileSync(GOLDEN_SHA, 'utf8').split('\n').filter((l) => l && !l.startsWith('#'));
  const want = Object.fromEntries(lines.map((l) => [l.slice(66), l.slice(0, 64)]));
  assert.equal(want['frozen dashboard.js over history.json'], sha(renders.frozenReal.html));
  assert.equal(
    want['generated dashboard.js over history.sample.json with config.json'],
    sha(renders.generatedConfigured.html),
  );
});

// The positive control. A golden test that compares two things which cannot
// differ proves nothing, and the way to find out whether this one can fail is
// to make it fail on purpose. Goes red against: a parameterisation that reads
// the config and then ignores it, which would pass every test above.
test('with the libviprs config loaded the render DOES change, so the comparison can fail', () => {
  assert.notEqual(
    renders.generatedConfigured.html,
    renders.frozenSample.html,
    'loading config.json changed nothing, so the config paths are inert and the equality ' +
      'tests above are proving nothing',
  );
  // And specifically: causl's palette is gone (this render is over causl's own
  // sample history, so causl's SERIES are still the data; what changed is that
  // libviprs's config no longer names them, so they draw in the renderer's
  // fallback grey), and the two new verdict kinds reached the legend strip.
  assert.doesNotMatch(renders.generatedConfigured.html, /#11D9FF/);
  assert.match(renders.generatedConfigured.html, /data-kind="noise"/);
  assert.match(renders.generatedConfigured.html, /data-kind="ungated"/);
  assert.doesNotMatch(renders.frozenSample.html, /data-kind="noise"/);
});

// Goes red against: a shim that silently swallows a whole subtree. If the
// serializer dropped attributes or children, both sides would still compare
// equal while proving nothing. So assert the render actually contains the
// things the parameterised constants feed.
test('the shim renders the things the parameterised constants feed', () => {
  const html = renders.frozenSample.html;
  for (const needle of [
    '#11D9FF', // LIBRARY_COLOR, causl-ts
    '6 3', // LIBRARY_DASH, redux-rtk
    'redux-toolkit (rtk runner)', // LIBRARY_LABEL
    'verdict-legend', // VERDICT_COLOR drives this strip
    'bench-legend-verdict', // the chip __verdictChip replaced
    '<svg', // renderSectionChart
  ]) {
    assert.ok(html.includes(needle), `the render is missing ${needle}, so the shim is eliding output`);
  }
});
