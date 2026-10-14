// Open one Player link headless and say what happened (a debugging aid).
//   node look.mjs <url> [chromium|webkit] [shot.png] [waitMs]
import { chromium, webkit } from 'playwright';

const [url, engine = 'chromium', shot, wait = '1500'] = process.argv.slice(2);
const browser = await (engine === 'webkit' ? webkit : chromium).launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
const logs = [];
page.on('console', (m) => logs.push(`${m.type()}: ${m.text()}`));
page.on('pageerror', (e) => logs.push(`pageerror: ${e.message}`));
await page.goto(url);
await page.waitForTimeout(500);
if ((await page.evaluate(() => window.__krate.state)) === 'consent') await page.click('#consentAllow');
await page.waitForTimeout(Number(wait));
const k = await page.evaluate(() => window.__krate);
console.log(JSON.stringify({ state: k.state, marks: k.marks, frames: k.frames, out: k.out, trap: k.trap, refused: k.refused, notices: k.notices, error: k.error, workerMarks: k.workerMarks }, null, 1));
console.log(logs.join('\n'));
if (shot) await page.screenshot({ path: shot });
await browser.close();
