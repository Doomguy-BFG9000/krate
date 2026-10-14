// A tiny static server for the web Player spike, with the cross-origin
// isolation headers switchable by path prefix so each browser can be asked
// the same question four ways:
//
//   /coi/...   COOP same-origin + COEP require-corp   (what the hub Worker would send)
//   /cred/...  COOP same-origin + COEP credentialless
//   /plain/... no isolation headers                   (what GitHub Pages sends)
//
// /plain/web/sw-test.html then checks whether a service worker can add the
// headers itself (the coi-serviceworker approach).
//
//   node server.mjs [port]     default 8861; serves this directory
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('.', import.meta.url));
const port = Number(process.argv[2] || 8861);
const types = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.wasm': 'application/wasm', '.json': 'application/json', '.css': 'text/css',
};
const modes = {
  coi: { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp' },
  cred: { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'credentialless' },
  plain: {},
};

http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  const [, mode, ...rest] = url.pathname.split('/');
  const headers = modes[mode];
  const rel = normalize(rest.join('/'));
  if (!headers || rel.startsWith('..')) { res.writeHead(404).end(); return; }
  try {
    const body = await readFile(join(root, rel));
    res.writeHead(200, {
      'Content-Type': types[extname(rel)] || 'application/octet-stream',
      'Cross-Origin-Resource-Policy': 'same-origin',
      'Cache-Control': 'no-store',
      ...headers,
    });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
}).listen(port, '127.0.0.1', () => console.log(`web player spike on http://127.0.0.1:${port}/`));
