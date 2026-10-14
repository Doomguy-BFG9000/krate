// Krate's host interfaces for a browser tab -- IC-956 Phase 0, the spike.
//
// Only what the two measured apps import: krate:io (args, stdout, stderr),
// krate:random/bytes, and krate:ui (window, tree, events). Shapes follow
// jco's JS mapping of the WIT: u64 is BigInt, option is the value or
// undefined, variants are {tag, val}, enums are strings, a failed result is
// thrown as an Error whose `payload` is the WIT error value.
//
// The same file runs on the page's main thread (the JSPI path, where
// events.wait returns a Promise) and inside a Web Worker (the Atomics path,
// where events.wait blocks). The difference is only the `waitRaw` function
// passed in.

export class HostError extends Error {
  constructor(payload) {
    super(typeof payload === 'object' ? JSON.stringify(payload) : String(payload));
    this.payload = payload;
  }
}

// A grant set stands in for the session's capabilities. Phase 0 grants what
// the manifest declares; the point here is that a refused call answers with
// the WIT's own `denied`, never a crash.
export function ioHost({ args = '', grants, onOutput }) {
  const decoders = new Map();
  const text = (fd, bytes) => {
    if (!decoders.has(fd)) decoders.set(fd, new TextDecoder());
    onOutput(fd, decoders.get(fd).decode(bytes, { stream: true }));
  };
  class OutputStream {
    constructor(fd) { this.fd = fd; }
    writeAll(bytes) { text(this.fd, bytes); }
    write(bytes) { text(this.fd, bytes); return bytes.length; }
    flush() {}
  }
  return {
    'krate:io/types': {},
    'krate:io/streams': { OutputStream },
    'krate:io/stdio': {
      stdout: () => new OutputStream(1),
      stderr: () => new OutputStream(2),
    },
    'krate:io/args': { raw: () => (grants.has('io.args') ? args : '') },
  };
}

export function randomHost({ grants }) {
  return {
    'krate:random/bytes': {
      get(count) {
        if (!grants.has('random.bytes')) throw new HostError({ tag: 'denied' });
        if (count > 1 << 20) throw new HostError({ tag: 'too-large' });
        const out = new Uint8Array(count);
        // getRandomValues takes at most 65536 bytes a call.
        for (let at = 0; at < count; at += 65536) {
          crypto.getRandomValues(out.subarray(at, Math.min(count, at + 65536)));
        }
        return out;
      },
    },
  };
}

// The WIT widget-kind enum in declaration order; the painter takes the index.
export const KINDS = ['stack', 'grid', 'scroll', 'tabs', 'button', 'checkbox', 'radio',
  'switch', 'slider', 'progress', 'text', 'text-field', 'text-area', 'list-view',
  'tree-view', 'image', 'canvas', 'overlay'];

const NO_MODS = { shift: false, control: false, alt: false, meta: false };

// krate:ui. `WebWindow` is the painter (crates/adapter-web, wasm32).
// `present(win, surface, pendingInputAt)` puts a painted frame on screen.
// `waitRaw(timeoutMs)` returns (or resolves to) one raw input record from
// the page, or undefined on timeout.
export function uiHost({ WebWindow, onWindow, present, waitRaw, now }) {
  const windows = new Map();
  let nextId = 1n;
  let pendingInputAt = undefined;

  const win = (id) => {
    const w = windows.get(id);
    if (!w) throw new HostError({ tag: 'invalid-window' });
    return w;
  };
  const paintAll = () => {
    for (const [id, w] of windows) {
      if (w.surface.render()) {
        present(id, w, pendingInputAt);
        pendingInputAt = undefined;
      }
    }
  };
  const flat = (node) => [
    node.id, KINDS.indexOf(node.kind), node.label ?? undefined, node.role ?? undefined,
    node.style.width ?? undefined, node.style.height ?? undefined, node.style.grow,
    node.style.padding, node.checked ?? undefined, node.value ?? undefined,
    node.selected ?? undefined,
  ];
  const check = (err) => { if (err !== undefined) throw new HostError({ tag: 'platform', val: err }); };

  // Turn a raw page record into a WIT event, hit-testing on the painter's
  // own layout so `widget` names what the person pressed.
  const lift = (raw) => {
    const [id, w] = windows.entries().next().value ?? [];
    if (id === undefined) return undefined;
    switch (raw.type) {
      case 'down':
      case 'up': {
        if (raw.type === 'down') pendingInputAt = raw.at;
        const widget = w.surface.hit(raw.x, raw.y);
        return { tag: 'pointer', val: {
          window: id, widget: widget ?? undefined, x: raw.x, y: raw.y,
          button: 'primary', pressed: raw.type === 'down', modifiers: NO_MODS,
        } };
      }
      case 'key':
        pendingInputAt = raw.at;
        return { tag: 'key', val: { window: id, widget: undefined, key: raw.key, pressed: true, modifiers: NO_MODS } };
      case 'text':
        pendingInputAt = raw.at;
        return { tag: 'text-input', val: raw.text };
      case 'close':
        return { tag: 'close-requested', val: id };
      default:
        return undefined;
    }
  };

  const wait = (timeout) => {
    // The app asking for its next event is the frame boundary: whatever it
    // changed since the last wait is painted now, once.
    paintAll();
    const raw = waitRaw(timeout);
    if (raw instanceof Promise) return raw.then((r) => (r ? lift(r) : undefined));
    return raw ? lift(raw) : undefined;
  };

  return {
    'krate:ui/types': {},
    'krate:ui/window': {
      create(title, size) {
        const id = nextId++;
        windows.set(id, { title, width: size.width, height: size.height,
          surface: new WebWindow(size.width, size.height) });
        onWindow(id, title, size.width, size.height);
        return id;
      },
      show(id) { win(id); },
      close(id) { win(id); windows.delete(id); },
    },
    'krate:ui/tree': {
      setRoot(id, node) {
        const [nid, ...rest] = flat(node);
        check(win(id).surface.set_root(nid, ...rest));
      },
      upsertNode(id, node) {
        const [nid, ...rest] = flat(node);
        check(win(id).surface.upsert(nid, node.parent ?? undefined, ...rest));
      },
    },
    'krate:ui/events': { wait },
  };
}

// The manifest's declared capabilities, as the grant set (Phase 0).
export const GRANTS = {
  diceroll: new Set(['io.args', 'io.stdout', 'io.stderr', 'random.bytes']),
  hello: new Set(['ui.window:create', 'io.args', 'io.stdout']),
};

// Instantiate a jco "--instantiation async" output, keeping each core
// instance's memory so the page can report how much wasm memory it holds.
export async function instantiateApp(base, imports, memories, timings) {
  const t0 = performance.now();
  const mod = await import(`${base}/app.js`);
  timings.glueLoaded = performance.now() - t0;
  const compile = (name) => WebAssembly.compileStreaming(fetch(`${base}/${name}`));
  const instantiateCore = async (module, imp) => {
    const inst = await WebAssembly.instantiate(module, imp);
    if (inst.exports.memory) memories.push(inst.exports.memory);
    return inst;
  };
  const app = await mod.instantiate(compile, imports, instantiateCore);
  timings.instantiated = performance.now() - t0;
  return app;
}
