// The Phase 0 numbers: cold start, input-to-frame latency and memory, per
// path per browser. Every run is a fresh browser context (empty cache), the
// server sends Cache-Control: no-store, and nothing is warmed first.
//
//   node measure.mjs [base] [browsers] [runs]
//
// cold start   performance.now() at the first stdout text (CLI) or the first
//              frame put on the page's canvas (GUI), from navigation start
// latency      pointerdown event timestamp -> the frame showing the app's
//              response is on the page's canvas (putImageData returned);
//              ten clicks on the checkbox per run, which toggles each time
// memory       RSS of every Playwright browser process, app page minus the
//              same context on about:blank, plus the wasm linear memories
import { execFileSync } from 'node:child_process';
import { chromium, firefox, webkit } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8861';
const only = (process.argv[3] || 'chromium,webkit,firefox').split(',');
const runs = Number(process.argv[4] || 10);
const CHECKBOX = { x: 16 + 10, y: 16 + 32 + 28 + 10 }; // hello-gui's checkbox box
const paths = [
  ['baseline (empty module page)', '/coi/web/baseline.html', 'firstOutput', false],
  ['cli (jco, main thread)', '/coi/web/cli.html', 'firstOutput', false],
  ['cli (pulley in wasm, comparison)', '/coi/web/cli-pulley.html', 'firstOutput', false],
  ['gui jspi (main thread)', '/coi/web/gui-jspi.html', 'firstFrame', true],
  ['gui worker+atomics (shared frame)', '/coi/web/gui-worker.html?draw=shared', 'firstFrame', true],
];

const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.floor((s.length - 1) / 2)] : NaN; };
const pct = (xs, q) => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.min(s.length - 1, Math.floor(q * s.length))] : NaN; };
const browserRss = () => {
  const out = execFileSync('ps', ['-A', '-o', 'rss=,command='], { encoding: 'utf8' });
  let kb = 0;
  for (const line of out.split('\n')) {
    if (line.includes('ms-playwright') || line.includes('com.apple.WebKit')) kb += Number(line.trim().split(/\s+/)[0]) || 0;
  }
  return kb / 1024;
};

const all = [['chromium', chromium], ['webkit', webkit], ['firefox', firefox]];
const table = [];
for (const [name, type] of all.filter(([n]) => only.includes(n))) {
  let browser;
  try { browser = await type.launch({ headless: true, timeout: 60000 }); } catch (e) {
    table.push({ browser: name, path: '(all)', works: false, error: String(e).split('\n')[0] });
    continue;
  }
  for (const [label, path, key, gui] of paths) {
    const starts = [], lat = [], errors = [];
    let mem = null;
    for (let i = 0; i < runs; i++) {
      const ctx = await browser.newContext({ viewport: { width: 800, height: 600 } });
      const page = await ctx.newPage();
      const measureMem = i === runs - 1;
      let blank = 0;
      if (measureMem) { await page.goto('about:blank'); await page.waitForTimeout(1000); blank = browserRss(); }
      await page.goto(base + path);
      try {
        await page.waitForFunction((k) => window.__krate && (window.__krate[k] !== undefined || window.__krate.error), key, { timeout: 15000 });
      } catch { /* the report says */ }
      const r = await page.evaluate(() => ({ ...window.__krate, memories: undefined }));
      if (r.error || r[key] === undefined) { errors.push(r.error || 'no first output/frame in 15 s'); await ctx.close(); continue; }
      starts.push(r[key]);
      if (gui) {
        for (let c = 0; c < 10; c++) { await page.mouse.click(CHECKBOX.x, CHECKBOX.y); await page.waitForTimeout(120); }
        await page.waitForTimeout(200);
        lat.push(...(await page.evaluate(() => window.__krate.latencies)));
      }
      if (measureMem) {
        await page.waitForTimeout(1000);
        const wasm = await page.evaluate(() => {
          const k = window.__krate;
          const m = typeof k.memories === 'function' ? k.memories() : (k.workerMemories || k.memories || []);
          return m.reduce((a, b) => a + b, 0) / 1048576;
        });
        mem = { rssDeltaMB: browserRss() - blank, rssTotalMB: browserRss(), wasmMB: wasm };
      }
      await ctx.close();
    }
    table.push({
      browser: `${name} ${browser.version()}`, path: label, works: starts.length === runs,
      runs: starts.length, coldMedianMs: median(starts), coldMinMs: Math.min(...starts), coldMaxMs: Math.max(...starts),
      clicks: lat.length, latMedianMs: median(lat), latP95Ms: pct(lat, 0.95), ...mem,
      errors: [...new Set(errors)].slice(0, 2),
    });
    console.error(JSON.stringify(table[table.length - 1]));
  }
  await browser.close();
}
console.log(JSON.stringify(table, null, 2));
