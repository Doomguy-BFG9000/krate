// The Krate Player page: a .krate link opened in this tab (IC-956 Phase 1).
//
// What happens, in order, each step saying so in words:
//   1. fetch the app (the bundle and the glue the hub made for it at
//      publish) -- with this tab's sign-in for an organisation's app;
//   2. check it: the bytes are the ones the link names (SHA-256), and the
//      bundle passes crates/validator-wasm -- the same rules the hub's door
//      and `krate run` apply, signature included; every core module about
//      to run is one of the judged component's own;
//   3. ask the person, with the desktop's own words for each capability;
//   4. run it in a Web Worker, answering its async work (fetch, storage,
//      the clipboard) over the bridge and putting its frames on the canvas.
//
// Nothing here decides a rule; the Player module (crates/adapter-web) and the
// validator do. window.__krate carries the timings the proofs read.

import init, { consent } from './krate_web.js';
import { makeBridgeMemory, pageBridge } from './bridge.js';
import { makeRing, pageRing } from './ring.js';

const $ = (id) => document.getElementById(id);
const report = (window.__krate = { marks: {}, frames: 0, latencies: [], paintLatencies: [], notices: [], out: '', state: 'loading' });
const mark = (name) => { report.marks[name] = performance.now(); };
const SITE = 'https://krate.tech';
const params = new URLSearchParams(location.search);
const HASH = (location.pathname.match(/^\/run\/([0-9a-f]{64})\/?$/) || [])[1];
const SESSION_KEY = 'krate-player-session';
const NONCE_KEY = 'krate-player-nonce';

// ------------------------------------------------------------ look and words
const forced = params.get('theme');
if (forced === 'dark' || forced === 'light') document.documentElement.dataset.theme = forced;
const dark = () => (forced ? forced === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches);

const STATES = ['loading', 'consent', 'signin', 'refused', 'app'];
function show(state) {
  report.state = state;
  for (const s of STATES) $(s).hidden = s !== state;
}
function say(words) { $('loadingWords').textContent = words; }
function refuse(title, words, { act = 'krate', detail } = {}) {
  $('refusedTitle').textContent = title;
  $('refusedWords').textContent = words;
  $('refusedDetails').hidden = !detail;
  $('refusedDetail').textContent = detail || '';
  const a = $('refusedAct');
  if (act === 'krate') { a.textContent = 'Open in Krate'; a.href = openInKrate(); }
  else if (act === 'home') { a.textContent = 'Go to krate.tech'; a.href = SITE + '/'; }
  report.refused = { title, words, detail };
  show('refused');
}
function openInKrate() {
  return HASH ? `${SITE}/open/?a=${HASH}${report.private ? '&private=1' : ''}` : `${SITE}/open/`;
}

// What a tab cannot do yet, in the words a person uses.
const WHAT = {
  'krate:gfx/canvas2d': 'drawing its own screen',
  'krate:gfx/canvas-images': 'drawing pictures',
  'krate:gfx/scene3d': '3D drawing',
  'krate:gfx/video': 'video',
  'krate:gpu/gpu': 'your graphics card',
  'krate:audio/playback': 'sound',
  'krate:audio/playback-queue': 'sound',
  'krate:audio/capture': 'your microphone',
  'krate:camera/capture': 'your camera',
  'krate:speech/transcription': 'speech',
  'krate:speech/synthesis': 'speaking aloud',
  'krate:fs/files': 'files on your computer',
  'krate:fs/watch': 'files on your computer',
  'krate:store/sql': 'its own database',
  'krate:store/org': "your team's shared data",
  'krate:store/secret': 'saved sign-in details',
  'krate:store/shared': 'data shared by invite code',
  'krate:store/group': 'data shared with other apps',
  'krate:net/ws': 'live connections',
  'krate:net/sockets': 'network connections',
  'krate:resources/assets': 'its own pictures and files',
  'krate:ui/dialog': 'dialogs and file pickers',
  'krate:ui/notify': 'notifications',
  'krate:ui/reminders': 'reminders',
  'krate:ui/print': 'printing',
  'krate:ui/menu': 'its own menus',
  'krate:ui/bluetooth': 'Bluetooth',
  'krate:ui/location': 'your location',
  'krate:ui/photos': 'your photos',
  'krate:ui/share': 'sharing',
  'krate:ui/sign-in': 'signing in to another service',
  'krate:ui/web-page': 'web pages inside it',
};
const whatOf = (iface) => WHAT[iface.replace(/@.*$/, '')] || 'something only Krate on a computer has';

function notice(words, act = true) {
  $('noticeWords').textContent = words;
  $('noticeAct').hidden = !act;
  $('noticeAct').href = openInKrate();
  $('notice').hidden = false;
}
$('noticeClose').onclick = () => { $('notice').hidden = true; };

// ------------------------------------------------------------------ sign-in
function session() {
  try { return JSON.parse(localStorage.getItem(SESSION_KEY) || 'null'); } catch { return null; }
}
// A finished sign-in comes back in the fragment (it never leaves the
// browser), with the nonce this tab made: taken only when it matches.
function takeSignIn() {
  if (!location.hash.includes('token=')) return;
  const got = new URLSearchParams(location.hash.slice(1));
  let expected = null;
  try { expected = sessionStorage.getItem(NONCE_KEY); sessionStorage.removeItem(NONCE_KEY); } catch {}
  history.replaceState(null, '', location.pathname + location.search);
  if (!expected || got.get('nonce') !== expected) return;
  try {
    localStorage.setItem(SESSION_KEY, JSON.stringify({ token: got.get('token'), login: got.get('login') || '', name: got.get('name') || '' }));
  } catch {}
}
function signIn(words) {
  const nonce = Array.from(crypto.getRandomValues(new Uint8Array(18)), (b) => b.toString(16).padStart(2, '0')).join('');
  try { sessionStorage.setItem(NONCE_KEY, nonce); } catch {}
  const q = (path) => `${path}?${new URLSearchParams({ from: `player:${HASH}`, nonce })}`;
  $('signinGithub').href = q('/login/start');
  $('signinGoogle').href = q('/login/google/start');
  if (words) $('signinWords').textContent = words;
  show('signin');
}

// ------------------------------------------------------------------ the app
const hex = (buf) => Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, '0')).join('');
const sha256 = async (bytes) => hex(await crypto.subtle.digest('SHA-256', bytes));

/// [u32 bundle length][bundle][u32 header length][header JSON][files...]
function unpack(buf) {
  const dv = new DataView(buf);
  const blen = dv.getUint32(0, true);
  const bundle = new Uint8Array(buf, 4, blen);
  let at = 4 + blen;
  const hlen = dv.getUint32(at, true);
  const meta = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, at + 4, hlen)));
  at += 4 + hlen;
  const files = new Map();
  for (const f of meta.files || []) {
    files.set(f.name, new Uint8Array(buf, at, f.size));
    at += f.size;
  }
  return { bundle, meta, files };
}

async function judge(validatorModule, bytes) {
  const v = (await WebAssembly.instantiate(validatorModule, {})).exports;
  const input = v.krate_alloc(bytes.length);
  new Uint8Array(v.memory.buffer, input, bytes.length).set(bytes);
  const out = v.krate_judge_for_tab(input, bytes.length);
  const len = new DataView(v.memory.buffer).getUint32(out, true);
  const text = new TextDecoder().decode(new Uint8Array(v.memory.buffer, out + 4, len));
  v.krate_free(out, 4 + len);
  v.krate_free(input, bytes.length);
  return JSON.parse(text);
}

async function main() {
  mark('start');
  takeSignIn();
  if (!HASH) {
    refuse('This is not an app link', 'A Krate app link ends in the app’s 64-character address.', { act: 'home' });
    return;
  }
  report.private = params.get('private') === '1';
  $('inKrate').href = openInKrate();
  if (!self.crossOriginIsolated || typeof SharedArrayBuffer !== 'function') {
    refuse('This browser cannot open Krate apps in a tab',
      'It does not allow the shared memory a Krate app needs here. The app opens in Krate on your computer.',
      { detail: `crossOriginIsolated=${self.crossOriginIsolated}` });
    return;
  }

  say('Opening the app...');
  const slow = setTimeout(() => say('Still opening... a slow connection'), 4000);
  const me = session();
  const headers = me && me.token ? { authorization: `Bearer ${me.token}` } : {};
  const [playerModule, validatorModule, res] = await Promise.all([
    WebAssembly.compileStreaming(fetch('/player/krate_web_bg.wasm')),
    WebAssembly.compileStreaming(fetch('/player/validator.wasm')),
    fetch(`/a/${HASH}/web`, { headers, cache: 'no-store' }),
  ]);
  mark('fetched');
  clearTimeout(slow);
  await init({ module_or_path: playerModule });

  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    const reason = body.error || '';
    if (reason === 'not-signed-in' || res.status === 401) {
      report.private = true;
      $('inKrate').href = openInKrate();
      signIn();
    } else if (reason === 'not-a-member') {
      refuse('This app was not shared with you', body.message || 'Ask whoever sent the link to invite you.', { act: 'home' });
    } else if (reason === 'removed') {
      refuse('You are no longer in this team', body.message || 'Ask the team to add you again.', { act: 'home' });
    } else if (res.status === 410) {
      refuse('This version was withdrawn', body.message || 'Ask whoever sent it for the current link.', { act: 'home' });
    } else if (res.status === 404) {
      refuse('This app is not on Krate', 'The link may be mistyped, or the app was removed.', { act: 'home' });
    } else if (reason === 'no-tab') {
      refuse('This app opens in Krate, not in a tab yet', body.message || 'Open it in Krate on your computer.', { detail: body.detail });
    } else {
      refuse('This app did not open', body.message || `The hub answered ${res.status}. Try again in a moment.`, { act: 'home' });
    }
    return;
  }

  say('Checking it is the app that was sent...');
  const { bundle, meta, files } = unpack(await res.arrayBuffer());
  // The bytes are the ones the link names.
  const digest = await sha256(bundle);
  if (digest !== HASH) {
    refuse('This app was changed on the way', 'What arrived is not the app the link names, so it was not opened.',
      { act: 'home', detail: `expected ${HASH}\nreceived ${digest}` });
    return;
  }
  // The bundle passes the same rules the hub and `krate run` apply.
  const judged = await judge(validatorModule, bundle);
  mark('judged');
  if (!judged.ok) {
    refuse('This app did not pass Krate’s checks', 'It was not opened.', { act: 'home', detail: judged.problem });
    return;
  }
  const j = judged.judgement;
  const sig = j.signature.state;
  if (sig !== 'absent' && sig !== 'valid') {
    refuse('This app was changed after it was signed', 'It was not opened: a signed app that no longer matches its signature could be anyone’s.',
      { act: 'home', detail: `${sig}: ${j.signature.detail || ''}` });
    return;
  }
  // Every core module about to run is one of the judged component's own.
  const modules = [];
  for (const [name, bytes] of files) {
    if (!name.endsWith('.wasm')) continue;
    const d = await sha256(bytes);
    if (!judged.modules.includes(d)) {
      refuse('This app opens in Krate, not in a tab yet', 'Part of what a tab would run could not be matched to the checked app, so it was not run.',
        { detail: `${name} ${d} is not a module of the judged component` });
      return;
    }
    modules.push([name, bytes.slice()]);
  }
  const glue = files.get('app.js');
  if (!glue) {
    refuse('This app opens in Krate, not in a tab yet', 'The hub has nothing for a tab to run for it.');
    return;
  }

  const app = j.manifest.app;
  const name = app.name || 'This app';
  document.title = `${name} - Krate`;
  $('whoName').textContent = name;
  const release = sig === 'valid' && j.release;
  $('whoSig').textContent = release ? `Signed by ${release.namespace.split('/')[0]}` : 'Not signed';
  $('who').hidden = false;
  report.app = { id: app.id, name, world: app.world, signature: sig };

  // ---- consent: the desktop's words, the tab's reach
  const rows = JSON.parse(consent(JSON.stringify(j.manifest.capabilities || []))).rows || [];
  const blocked = rows.filter((r) => r.required && !r.in_tab);
  const granted = rows.filter((r) => r.in_tab).map((r) => r.cap);
  const storeKey = release ? `${app.id}@${release.authority.slice(0, 16)}` : app.id;
  if (rows.length) {
    $('consentTitle').textContent = `“${name}” wants to:`;
    const list = $('consentRows');
    list.textContent = '';
    for (const r of rows) {
      const li = document.createElement('li');
      if (!r.in_tab) li.className = 'away';
      li.innerHTML = '<i><svg viewBox="0 0 16 16" fill="none"><path d="M3.5 8.5l3 3 6-6.5" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg></i><span><b></b><em></em></span>';
      li.querySelector('b').textContent = r.plain;
      li.querySelector('em').textContent = r.in_tab ? r.rationale : `${r.rationale ? r.rationale + ' - ' : ''}works in Krate on your computer`;
      list.appendChild(li);
    }
    if (blocked.length) {
      $('consentNote').textContent = `${name} needs ${blocked.map((r) => r.plain).join(', ')}, which works in Krate on your computer, not in a tab yet.`;
      $('consentAllow').textContent = 'Open in Krate';
      $('consentAllow').onclick = () => { location.href = openInKrate(); };
    } else {
      $('consentNote').textContent = rows.some((r) => !r.in_tab)
        ? 'Greyed out: works in Krate on your computer. The app opens here without it.' : '';
      $('consentAllow').textContent = 'Allow and open';
    }
    $('consentCancel').onclick = () => refuse('Not opened', `${name} did not open. Nothing was allowed.`, { act: 'home' });
    show('consent');
    mark('consent');
    if (blocked.length) return;
    await new Promise((resolve) => { $('consentAllow').onclick = resolve; });
    mark('allowed');
  } else {
    mark('allowed');
  }

  say(`Starting ${name}...`);
  show('loading');
  await start({ meta, modules, glue: new TextDecoder().decode(glue), granted, storeKey, playerModule, world: app.world, name });
}

// ------------------------------------------------------------- storage
const DB = 'krate-player';
function db() {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(DB, 1);
    open.onupgradeneeded = () => open.result.createObjectStore('kv');
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
}
const tx = (d, mode, fn) => new Promise((resolve, reject) => {
  const t = d.transaction('kv', mode);
  const out = fn(t.objectStore('kv'));
  t.oncomplete = () => resolve(out && out.result);
  t.onerror = () => reject(t.error);
  t.onabort = () => reject(t.error || new Error('the browser stopped the write'));
});
const range = (store) => IDBKeyRange.bound([store, ''], [store, '￿']);
async function loadKv(d, store) {
  const keys = await tx(d, 'readonly', (s) => s.getAllKeys(range(store)));
  const values = await tx(d, 'readonly', (s) => s.getAll(range(store)));
  return keys.map((k, i) => [k[1], new Uint8Array(values[i])]);
}

// ------------------------------------------------------------- running
async function start({ meta, modules, glue, granted, storeKey, playerModule, world, name }) {
  const gui = /\/gui@/.test(world || '');
  const d = gui || granted.includes('store.kv') ? await db().catch(() => null) : null;
  const kv = d && granted.includes('store.kv') ? await loadKv(d, storeKey).catch(() => []) : [];

  const ringBuf = makeRing();
  const ring = pageRing(ringBuf);
  const bridgeMem = makeBridgeMemory();
  const worker = new Worker('/player/app-worker.js', { type: 'module' });
  const canvas = $('screen');
  const ctx2d = canvas.getContext('2d');
  const term = $('term');
  const windows = new Map();
  let current = null;
  const now = () => performance.timeOrigin + performance.now();

  // ---- async host work, answered over the bridge
  const fetches = new Map();
  let nextFetch = 1;
  const MAX = 1024 * 1024;
  const CORS = 'the server did not answer this tab. It may not allow requests from a browser tab (CORS), or be unreachable from here; the app reaches it from Krate on your computer.';
  const doFetch = async ({ method, url, headers, body, timeout }) => {
    if (location.protocol === 'https:' && /^http:/i.test(url)) {
      return { json: { err: { tag: 'connect-failure', val: 'a page on https cannot reach a plain http address; the app reaches it from Krate on your computer.' } } };
    }
    const abort = new AbortController();
    const timer = timeout ? setTimeout(() => abort.abort('timeout'), timeout) : null;
    try {
      const res = await fetch(url, {
        method,
        headers: (headers || []).map((h) => [h.name, h.value]),
        body: body && method !== 'GET' && method !== 'HEAD' ? body : undefined,
        // Redirects are not followed, as on desktop: a granted host must not
        // be able to send the app's request anywhere it likes. A tab cannot
        // even see where a redirect points, so the request stops here.
        redirect: 'manual',
        credentials: 'omit',
        cache: 'no-store',
        referrerPolicy: 'no-referrer',
        signal: abort.signal,
      });
      if (res.type === 'opaqueredirect') {
        return { json: { err: { tag: 'other', val: 'the server answered with a redirect, which a browser tab does not follow or show; the app can follow it from Krate on your computer.' } } };
      }
      const reader = res.body ? res.body.getReader() : null;
      const parts = [];
      let total = 0;
      while (reader) {
        const { done, value } = await reader.read();
        if (done) break;
        total += value.length;
        if (total > MAX) { abort.abort('too-large'); return { json: { err: { tag: 'body-too-large' } } }; }
        parts.push(value);
      }
      const out = new Uint8Array(total);
      let at = 0;
      for (const p of parts) { out.set(p, at); at += p.length; }
      return { json: { ok: { status: res.status, headers: [...res.headers].map(([n, v]) => ({ name: n, value: v })) } }, body: out };
    } catch (e) {
      if (abort.signal.aborted && abort.signal.reason === 'timeout') return { json: { err: { tag: 'timeout' } } };
      return { json: { err: { tag: 'connect-failure', val: CORS } } };
    } finally {
      if (timer) clearTimeout(timer);
    }
  };
  const kvKey = (key) => [storeKey, key];
  const kvWrite = async (fn) => {
    try { await tx(d, 'readwrite', fn); return { json: { ok: null } }; }
    catch (e) { return { json: { err: { tag: 'io', val: `the browser did not save it (${(e && e.message) || e})` } } }; }
  };
  const bridge = pageBridge(bridgeMem, {
    'http.fetch': doFetch,
    'http.begin': (args) => {
      const id = nextFetch++;
      const entry = { state: 'pending' };
      entry.done = doFetch(args).then((a) => { entry.answer = a; entry.state = 'done'; });
      fetches.set(String(id), entry);
      return { json: { ok: id } };
    },
    'http.poll': ({ handle }) => {
      const entry = fetches.get(handle);
      if (!entry) return { json: { state: 'unknown' } };
      if (entry.state === 'pending') return { json: { state: 'pending' } };
      fetches.delete(handle);
      return entry.answer;
    },
    'http.cancel': ({ handle }) => { fetches.delete(handle); return { json: { ok: null } }; },
    'kv.set': ({ key, value }) => (d ? kvWrite((s) => s.put(value, kvKey(key))) : { json: { err: { tag: 'io', val: 'this browser keeps no storage for this tab' } } }),
    'kv.delete': ({ key }) => (d ? kvWrite((s) => s.delete(kvKey(key))) : { json: { ok: null } }),
    'kv.clear': () => (d ? kvWrite((s) => s.delete(range(storeKey))) : { json: { ok: null } }),
    'clipboard.read': async () => {
      try { return { json: { ok: await navigator.clipboard.readText() } }; }
      catch { return { json: { err: { tag: 'platform', val: 'the browser did not let this tab read the clipboard' } } }; }
    },
    'clipboard.write': async ({ text }) => {
      try { await navigator.clipboard.writeText(text); return { json: { ok: null } }; }
      catch { return { json: { err: { tag: 'platform', val: 'the browser did not let this tab write to the clipboard' } } }; }
    },
    'open-url': ({ url }) => {
      const opened = window.open(url, '_blank', 'noopener,noreferrer');
      if (!opened) notice(`${name} wants to open ${url}. Your browser held it back.`, false);
      report.opened = (report.opened || []).concat(url);
      return { json: { ok: null } };
    },
  }, (op, e) => { report.bridgeErrors = (report.bridgeErrors || []).concat(`${op}: ${e}`); });

  // ---- frames
  let scratch = null;
  // The worker bumps a shared counter after each frame and the page wakes on
  // it (Atomics.waitAsync), so a frame reaches the canvas as soon as it is
  // painted; the 'frame' message that follows carries only what it was for.
  const signalBuf = new SharedArrayBuffer(8);
  const signal = new Int32Array(signalBuf);
  const blitted = new Map();
  const blit = (id) => {
    const w = windows.get(id);
    if (!w || !scratch) return now();
    // ImageData cannot wrap shared memory, so one copy out of it.
    scratch.set(new Uint8Array(w.frame));
    ctx2d.putImageData(new ImageData(scratch, w.pw, w.ph), 0, 0);
    report.frames += 1;
    if (report.marks.firstFrame === undefined) report.marks.firstFrame = performance.now();
    return now();
  };
  let seen = 0;
  const listen = () => {
    if (typeof Atomics.waitAsync !== 'function') return;
    const r = Atomics.waitAsync(signal, 0, seen);
    (r.async ? r.value : Promise.resolve()).then(() => {
      const seq = Atomics.load(signal, 0);
      if (seq !== seen) {
        seen = seq;
        blitted.set(seq, blit(String(Atomics.load(signal, 1))));
      }
      listen();
    });
  };
  listen();
  worker.onmessage = ({ data }) => {
    switch (data.t) {
      case 'call':
      case 'more':
        bridge.onMessage(data);
        break;
      case 'window': {
        windows.set(data.id, data);
        current = data.id;
        canvas.width = data.pw;
        canvas.height = data.ph;
        canvas.style.width = `${data.lw}px`;
        canvas.style.height = `${data.lh}px`;
        scratch = new Uint8ClampedArray(data.pw * data.ph * 4);
        if (report.state !== 'app') { show('app'); $('frame').hidden = false; $('closeApp').hidden = false; canvas.focus(); }
        break;
      }
      case 'frame': {
        if (!windows.get(data.id)) break;
        const done = blitted.get(data.seq) ?? blit(data.id);
        blitted.delete(data.seq);
        if (data.inputAt !== undefined) {
          report.latencies.push(done - data.inputAt);
          // The worker's half: input to the frame painted in shared memory.
          (report.paintLatencies = report.paintLatencies || []).push(data.paintedAt - data.inputAt);
        }
        if (data.screen) report.screen = JSON.parse(data.screen);
        break;
      }
      case 'title': document.title = `${data.title} - Krate`; break;
      case 'out': {
        report.out += data.text;
        if (!gui || data.fd === 2) {
          if (term.hidden) { term.hidden = false; if (!gui) show('app'); }
          const span = document.createElement('span');
          if (data.fd === 2) span.className = 'err';
          span.textContent = data.text;
          term.appendChild(span);
          if (report.marks.firstOutput === undefined) report.marks.firstOutput = performance.now();
        }
        break;
      }
      case 'notice': {
        report.notices.push({ iface: data.iface, fname: data.fname });
        notice(`${name} wanted to use ${whatOf(data.iface)}, which works in Krate on your computer, not in a tab yet.`);
        break;
      }
      case 'text-entry': if (data.on) $('ime').focus({ preventScroll: true }); else canvas.focus(); break;
      case 'marks': report.workerMarks = data.marks; break;
      case 'closed': windows.delete(data.id); break;
      case 'exit':
        report.exit = data.code;
        finish(data.code === 0 ? `${name} finished.` : `${name} stopped (exit ${data.code}).`);
        break;
      case 'trap':
        report.trap = data.message;
        finish(data.stopped ? data.message : `${name} stopped: ${data.message}`, !data.stopped);
        break;
      default: break;
    }
  };
  worker.onerror = (e) => { report.trap = `worker: ${e.message}`; finish(`${name} stopped: ${e.message}`); };
  function finish(words, crashed) {
    $('closeApp').hidden = true;
    if (report.state !== 'app') show('app');
    $('frame').hidden = !gui || windows.size === 0;
    const ended = $('ended');
    ended.textContent = words;
    ended.hidden = false;
    if (crashed) notice(words);
    worker.terminate();
  }

  // ---- input, in logical pixels, to the app's ring
  const mods = (e) => (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0);
  const at = (e) => performance.timeOrigin + e.timeStamp;
  const win = () => current;
  canvas.addEventListener('pointerdown', (e) => {
    canvas.focus();
    canvas.setPointerCapture(e.pointerId);
    ring.push({ type: 'down', window: win(), x: e.offsetX, y: e.offsetY, button: e.button, mods: mods(e), at: at(e) });
    e.preventDefault();
  });
  canvas.addEventListener('pointerup', (e) => ring.push({ type: 'up', window: win(), x: e.offsetX, y: e.offsetY, button: e.button, mods: mods(e), at: at(e) }));
  canvas.addEventListener('pointermove', (e) => ring.push({ type: 'move', window: win(), x: e.offsetX, y: e.offsetY }));
  canvas.addEventListener('pointerleave', () => ring.push({ type: 'leave', window: win() }));
  canvas.addEventListener('wheel', (e) => {
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? canvas.clientHeight : 1;
    ring.push({ type: 'wheel', window: win(), x: e.offsetX, y: e.offsetY, dx: e.deltaX * unit, dy: e.deltaY * unit, mods: mods(e), at: at(e) });
    e.preventDefault();
  }, { passive: false });
  const key = (pressed) => (e) => {
    if (current === null) return;
    const typed = pressed && e.key.length === 1 && !e.ctrlKey && !e.metaKey ? e.key : undefined;
    ring.push({ type: 'key', window: win(), key: e.key, pressed, mods: mods(e), text: typed, at: at(e) });
    // Keys the app takes stay with the app; browser shortcuts (Cmd/Ctrl)
    // stay with the browser.
    if (!e.ctrlKey && !e.metaKey) e.preventDefault();
  };
  canvas.addEventListener('keydown', key(true));
  canvas.addEventListener('keyup', key(false));
  canvas.addEventListener('paste', (e) => {
    const text = e.clipboardData && e.clipboardData.getData('text/plain');
    if (text) ring.push({ type: 'text', window: win(), text, at: performance.timeOrigin + performance.now() });
    e.preventDefault();
  });
  // A phone's keyboard types into the hidden field; its text goes to the app.
  const ime = $('ime');
  ime.addEventListener('input', (e) => {
    if (e.inputType === 'deleteContentBackward') ring.push({ type: 'key', window: win(), key: 'Backspace', pressed: true, mods: 0, at: at(e) });
    else if (ime.value) ring.push({ type: 'text', window: win(), text: ime.value, at: at(e) });
    ime.value = '';
  });
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (!forced) ring.push({ type: 'theme', dark: dark() });
  });
  window.addEventListener('blur', () => ring.push({ type: 'blur' }));
  // The window takes the room the tab has, up to what the app asked for.
  const room = () => {
    const s = $('stage').getBoundingClientRect();
    return { width: Math.max(1, Math.floor(s.width - 32)), height: Math.max(1, Math.floor(s.height - 32)) };
  };
  new ResizeObserver(() => { if (current !== null) ring.push({ type: 'resize', window: win(), ...room() }); }).observe($('stage'));
  $('closeApp').onclick = () => {
    if (current === null) { worker.terminate(); finish(`${name} closed.`); return; }
    ring.push({ type: 'close', window: win() });
    // An app that does not answer its close is ended, as Krate's own Close
    // ends one (IC-934).
    setTimeout(() => { if (report.exit === undefined && !report.trap) finish(`${name} closed.`); }, 2000);
  };

  report.marks.workerStarted = performance.now();
  worker.postMessage({
    t: 'start',
    playerModule,
    meta,
    modules,
    glue,
    granted,
    kv,
    storeKey,
    args: params.getAll('arg').join('\n'),
    scale: Math.min(4, Math.max(1, window.devicePixelRatio || 1)),
    dark: dark(),
    touch: matchMedia('(pointer: coarse)').matches,
    probe: params.get('probe') === '1',
    avail: room(),
    ring: ringBuf,
    signal: signalBuf,
    bridge: bridgeMem,
  });
}

main().catch((e) => {
  report.error = String((e && e.stack) || e);
  refuse('This app did not open', 'Something went wrong in this tab. Reload to try again.', { act: 'home', detail: String((e && e.message) || e) });
});
