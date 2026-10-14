// How long jco's transpile step takes, warm, in-process (node), for each app:
// the cost the hub would pay once per published version if it transpiles at
// publish time. node transpile-time.mjs <component.wasm>...
import { readFile } from 'node:fs/promises';
import { transpile } from '@bytecodealliance/jco';

for (const path of process.argv.slice(2)) {
  const bytes = await readFile(path);
  const opts = { name: 'app', instantiation: { tag: 'async' }, noTypescript: true, noNodejsCompat: true, base64Cutoff: 0 };
  await transpile(bytes, opts); // the first call loads jco's own wasm
  const times = [];
  for (let i = 0; i < 10; i++) { const t = performance.now(); await transpile(bytes, opts); times.push(performance.now() - t); }
  times.sort((a, b) => a - b);
  console.log(path.split('/').pop(), `median ${times[4].toFixed(1)} ms over 10`);
}
