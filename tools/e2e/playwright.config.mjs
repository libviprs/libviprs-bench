import { defineConfig } from '@playwright/test';

// Browser specs for the generated pages. Run them in the Playwright image with
// `npm i --no-save @playwright/test` first; nothing here is part of `npm test`.
export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.mjs',
  outputDir: process.env.E2E_OUT || './test-results',
  reporter: 'list',
  use: { viewport: { width: 640, height: 420 }, deviceScaleFactor: 2 },
});
