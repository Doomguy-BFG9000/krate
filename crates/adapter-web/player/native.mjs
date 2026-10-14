// The same two apps, native, through the repo's own `krate` build, for the
// numbers beside the browser's.
//   node native.mjs <path-to-krate> [runs]
// cli  time from spawn to the first stdout bytes of `krate run --headless`
// gui  time from spawn to `krate run --shoot` having written its PNG
//      (headless: no window may open on this machine during the spike)
// Peak RSS of each from /usr/bin/time -l.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, rmSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const krate = process.argv[2];
const runs = Number(process.argv[3] || 10);
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const dice = join(repo, 'apps/krate-diceroll/target/wasm32-wasip1/release/krate_diceroll.wasm');
const diceManifest = join(repo, 'apps/krate-diceroll/manifest.toml');
const hello = join(repo, 'apps/krate-hello-gui/target/wasm32-wasip1/release/krate_hello_gui.wasm');
const helloManifest = join(repo, 'apps/krate-hello-gui/manifest.toml');
const scratch = mkdtempSync(join(tmpdir(), 'krate-native-'));
const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s[Math.floor((s.length - 1) / 2)]; };

const firstStdout = (args) => new Promise((resolve) => {
  const t0 = performance.now();
  let first;
  const p = spawn(krate, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  p.stdout.once('data', () => { first = performance.now() - t0; });
  p.on('exit', (code) => resolve({ first, total: performance.now() - t0, code }));
});
const untilFile = (args, file) => new Promise((resolve) => {
  rmSync(file, { force: true });
  const t0 = performance.now();
  let seen;
  const timer = setInterval(() => { if (seen === undefined && existsSync(file)) seen = performance.now() - t0; }, 1);
  const p = spawn(krate, args, { stdio: ['ignore', 'ignore', 'pipe'] });
  let err = '';
  p.stderr.on('data', (d) => { err += d; });
  p.on('exit', (code) => { clearInterval(timer); resolve({ first: seen, total: performance.now() - t0, code, err }); });
});
const peakRss = (args) => {
  const r = spawnSync('/usr/bin/time', ['-l', krate, ...args], { encoding: 'utf8' });
  const m = /(\d+)\s+maximum resident set size/.exec(r.stderr);
  return m ? Number(m[1]) / 1048576 : NaN;
};

const version = spawnSync(krate, ['--version'], { encoding: 'utf8' }).stdout.trim();
const cliArgs = ['run', '--headless', '--manifest', diceManifest, '--auto-grant', '--profile', 'analysis', dice, '--', '5'];
const shot = join(scratch, 'hello.png');
const guiArgs = ['run', '--shoot', shot, '--shoot-scale', '1', '--manifest', helloManifest, '--auto-grant', '--profile', 'analysis', hello];

const cli = [], gui = [];
for (let i = 0; i < runs; i++) cli.push(await firstStdout(cliArgs));
for (let i = 0; i < runs; i++) gui.push(await untilFile(guiArgs, shot));
const out = {
  krate: version,
  cli: { firstStdoutMedianMs: median(cli.map((r) => r.first)), totalMedianMs: median(cli.map((r) => r.total)),
    codes: [...new Set(cli.map((r) => r.code))], peakRssMB: peakRss(cliArgs) },
  gui: { pngWrittenMedianMs: median(gui.filter((r) => r.first !== undefined).map((r) => r.first)),
    totalMedianMs: median(gui.map((r) => r.total)), shots: gui.filter((r) => r.first !== undefined).length,
    codes: [...new Set(gui.map((r) => r.code))], err: gui[0].err.slice(0, 300), peakRssMB: peakRss(guiArgs) },
  shot,
};
console.log(JSON.stringify(out, null, 2));
