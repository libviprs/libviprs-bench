// Run a dashboard.js against a history document and return the HTML it built.
//
// `dashboard.js` is a browser IIFE: it runs on load, fetches its history and
// writes into `#dashboard-root`. So the harness gives it a context with the
// DOM shim, a `fetch` that serves the history it was handed, and optionally a
// `LIBVIPRS_BENCH_CONFIG` global, lets it finish, and serialises the root.
//
// This is what makes "the parameterised dashboard renders exactly what the
// frozen one does" checkable instead of arguable. Both sides go through this
// same function with the same history and the same (absent) config, and the
// two strings either match byte for byte or they do not.

import vm from 'node:vm';
import { createDocument } from './dom-shim.mjs';

/**
 * @param {string} source      dashboard.js text
 * @param {object} opts.history  the array served as ./history.json
 * @param {object} [opts.sample] the array served as ./history.sample.json
 * @param {object} [opts.config] value for globalThis.LIBVIPRS_BENCH_CONFIG
 * @param {number} [opts.historyStatus] HTTP status for ./history.json (404 to force the fallback)
 * @returns {Promise<{html: string, fetched: string[], errors: string[]}>}
 */
export async function render(source, opts) {
  const { document, serialize } = createDocument();
  const fetched = [];
  const errors = [];

  const served = {
    './history.json': { status: opts.historyStatus ?? 200, body: opts.history },
    './history.sample.json': { status: 200, body: opts.sample ?? opts.history },
  };

  const sandbox = {
    document,
    console: { log() {}, warn() {}, error(...a) { errors.push(a.map(String).join(' ')); } },
    fetch: async (url) => {
      fetched.push(url);
      const hit = served[url];
      if (!hit) return { ok: false, status: 404, async json() { throw new Error('no body'); } };
      return {
        ok: hit.status >= 200 && hit.status < 300,
        status: hit.status,
        // Structured-clone the body so a renderer that mutates what it was
        // handed cannot leak that mutation into the other side's render.
        async json() {
          return JSON.parse(JSON.stringify(hit.body));
        },
      };
    },
  };
  if (opts.config !== undefined) sandbox.LIBVIPRS_BENCH_CONFIG = opts.config;

  const context = vm.createContext(sandbox);
  vm.runInContext(source, context, { filename: opts.filename ?? 'dashboard.js' });

  // `bootstrap()` is async and nothing returns its promise, so drain the
  // microtask queue until the root stops changing. Bounded: a render that has
  // not settled in 200 turns is a bug, not a slow one.
  let last = null;
  for (let i = 0; i < 200; i += 1) {
    await new Promise((r) => setImmediate(r));
    const now = serialize();
    if (now === last && fetched.length > 0) break;
    last = now;
  }

  return { html: serialize(), fetched, errors };
}
