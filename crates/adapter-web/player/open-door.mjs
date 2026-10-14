// The krate.tech door (docs/open/index.html) offers "Open in this tab" for an
// app link (IC-956). The page is served from the repo; the hub's /meta answer
// is stood in for, so this runs offline.
//   node open-door.mjs [shot-dir]
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join, normalize } from 'node:path';
import { chromium } from 'playwright';

const here = fileURLToPath(new URL('.', import.meta.url));
const repo = join(here, '..', '..', '..');
const shots = process.argv[2] || join(here, 'build', 'shots');
const server = http.createServer(async (req, res) => {
  let p = normalize(new URL(req.url, 'http://x').pathname);
  if (p.startsWith('/open')) p = '/open/index.html';
  const roots = [join(repo, 'docs'), join(repo, 'docs', 'landing')];
  for (const root of roots) {
    try { const body = await readFile(join(root, p)); res.writeHead(200); res.end(body); return; } catch {}
  }
  res.writeHead(404); res.end();
}).listen(8915, '127.0.0.1');
const hash = 'c'.repeat(64);
const browser = await chromium.launch({ headless: true });
let failed = 0;
for (const [label, viewport, ua] of [['desktop', { width: 1200, height: 900 }, undefined], ['phone', { width: 390, height: 844 }, 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148']]) {
  const context = await browser.newContext({ viewport, userAgent: ua });
  await context.route('https://hub.krate.tech/**', (route) => {
    if (route.request().url().includes('/meta/')) return route.fulfill({ json: { meta: { name: 'Team Notes', size: 40000 } } });
    return route.fulfill({ status: 204, body: '' });
  });
  const page = await context.newPage();
  await page.goto(`http://127.0.0.1:8915/open/?a=${hash}`);
  await page.waitForSelector('#taTab:not([hidden])', { timeout: 5000 }).catch(() => {});
  const tab = await page.evaluate(() => { const t = document.getElementById('taTab'); return { hidden: t.hidden, href: t.href, cls: t.className, text: t.textContent, get: !document.getElementById('taGet').hidden }; });
  const ok = !tab.hidden && tab.href === `https://hub.krate.tech/run/${hash}` && (label === 'phone' ? tab.cls.includes('dark') && !tab.get : tab.get);
  if (!ok) failed += 1;
  console.log(`${ok ? 'ok  ' : 'FAIL'} [${label}] /open/ offers "Open in this tab" beside "Open in Krate"`, JSON.stringify(tab));
  await page.waitForTimeout(1500);
  await page.screenshot({ path: join(shots, `open-door-${label}.png`) });
  await context.close();
}
await browser.close();
server.close();
process.exit(failed ? 1 : 0);
