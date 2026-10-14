// Two questions per browser, answered by looking rather than assuming:
//  1. Which headers make the page cross-origin isolated (SharedArrayBuffer)?
//     /coi (COEP require-corp), /cred (COEP credentialless), /plain (none),
//     and /plain + a service worker that adds the headers (coi-serviceworker).
//  2. Does a worker's OffscreenCanvas frame reach the page while the worker
//     is blocked in Atomics.wait?
//   node coi-probe.mjs [base] [browsers]
import { chromium, firefox, webkit } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8861';
const only = (process.argv[3] || 'chromium,webkit,firefox').split(',');
const all = [['chromium', chromium], ['webkit', webkit], ['firefox', firefox]];
const results = {};
for (const [name, type] of all.filter(([n]) => only.includes(n))) {
  const browser = await type.launch({ headless: true });
  const r = (results[name] = { version: browser.version() });
  for (const mode of ['coi', 'cred', 'plain']) {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await page.goto(`${base}/${mode}/web/sw-test.html`);
    r[mode] = await page.evaluate(() => ({ isolated: self.crossOriginIsolated, sab: typeof SharedArrayBuffer === 'function' }));
    await ctx.close();
  }
  // The service-worker route: first load registers, the reload is controlled.
  {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await page.goto(`${base}/plain/web/sw-test.html?sw=1`);
    try {
      await page.waitForFunction(() => window.__sw === 'ready' || window.__sw?.startsWith?.('error'), null, { timeout: 8000 });
    } catch { /* recorded below */ }
    const first = await page.evaluate(() => window.__sw);
    await page.reload();
    await page.waitForTimeout(500);
    r.serviceWorker = { registration: first, ...(await page.evaluate(() => ({
      controlled: !!navigator.serviceWorker?.controller, isolated: self.crossOriginIsolated,
      sab: typeof SharedArrayBuffer === 'function' }))) };
    await ctx.close();
  }
  for (const block of ['0', '1']) {
    const ctx = await browser.newContext({ viewport: { width: 200, height: 200 } });
    const page = await ctx.newPage();
    await page.goto(`${base}/coi/web/offscreen-control.html?block=${block}`);
    await page.waitForTimeout(700);
    const shot = await page.screenshot({ clip: { x: 100, y: 100, width: 1, height: 1 } });
    // A 1x1 PNG: the red channel is enough to tell red from white.
    r[`offscreen_block${block}`] = shot.length; // size placeholder, decoded below
    r[`offscreen_block${block}_red`] = await decodeOnePixel(page, shot);
    await ctx.close();
  }
  await browser.close();
}
console.log(JSON.stringify(results, null, 2));

async function decodeOnePixel(page, png) {
  return page.evaluate(async (b64) => {
    const img = new Image();
    img.src = `data:image/png;base64,${b64}`;
    await img.decode();
    const c = document.createElement('canvas'); c.width = 1; c.height = 1;
    const g = c.getContext('2d'); g.drawImage(img, 0, 0);
    const [r, gg, b] = g.getImageData(0, 0, 1, 1).data;
    return `rgb(${r},${gg},${b})`;
  }, png.toString('base64'));
}
