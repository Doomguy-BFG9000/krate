// One pass over every path in every browser: does it run, and what did it say.
//   node smoke.mjs [base]    base defaults to http://127.0.0.1:8861
import { chromium, firefox, webkit } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8861';
const pages = [
  ['cli', '/coi/web/cli.html', 'firstOutput'],
  ['jspi', '/coi/web/gui-jspi.html', 'firstFrame'],
  ['worker-shared', '/coi/web/gui-worker.html?draw=shared', 'firstFrame'],
  ['worker-offscreen', '/coi/web/gui-worker.html?draw=offscreen', 'firstFrame'],
];
const only = (process.argv[3] || 'chromium,webkit,firefox').split(',');
const all = [['chromium', chromium], ['webkit', webkit], ['firefox', firefox]];
for (const [name, type] of all.filter(([n]) => only.includes(n))) {
  const browser = await type.launch({ headless: true });
  for (const [label, path, key] of pages) {
    const ctx = await browser.newContext({ viewport: { width: 800, height: 600 } });
    const page = await ctx.newPage();
    const logs = [];
    page.on('console', (m) => logs.push(m.text()));
    page.on('pageerror', (e) => logs.push(`pageerror: ${e.message}`));
    await page.goto(base + path);
    try {
      await page.waitForFunction((k) => window.__krate && (window.__krate[k] !== undefined || window.__krate.error), key, { timeout: 10000 });
    } catch { /* report what is there */ }
    const r = await page.evaluate(() => {
      const k = window.__krate || {};
      return { ...k, memories: typeof k.memories === 'function' ? k.memories() : k.memories };
    });
    if (label !== 'cli' && !r.error) {
      await page.mouse.click(16 + 80, 16 + 16); // the button
      await page.waitForTimeout(300);
      await page.screenshot({ path: `build/smoke-${name}-${label}.png` });
    }
    const r2 = await page.evaluate(() => ({ latencies: window.__krate?.latencies, frames: window.__krate?.frames }));
    console.log(name, label, JSON.stringify({ ...r, ...r2 }).slice(0, 600), logs.slice(0, 3).join(' | ').slice(0, 300));
    await ctx.close();
  }
  await browser.close();
}
