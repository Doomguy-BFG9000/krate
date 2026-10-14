// The bridge between a Krate app blocked in a Web Worker and the page that
// can do async work for it (IC-956 Phase 1).
//
// A Krate app's run() never returns to the worker's event loop: it blocks
// in events.wait the way a desktop app does. So anything that is a Promise
// in a browser -- fetch, IndexedDB, the clipboard, the hub -- cannot finish
// inside the worker. The worker asks the page instead and blocks on
// Atomics.wait until the page writes the answer into shared memory:
//
//   worker: postMessage({ t: 'call', seq, op, args })      then Atomics.wait
//   page:   runs op, encodes the answer, writes one chunk, Atomics.notify
//   worker: copies the chunk; if more is coming, postMessage({ t: 'more' })
//
// One call is in flight at a time (the app has one thread). An answer
// larger than the shared buffer arrives in chunks, so a response body is
// never limited by the buffer's size.
//
// Wire format of an answer: [u32 json length][json][binary body]. The JSON
// carries `ok` or `err` (a WIT error value); the body carries bytes (an
// HTTP body, a stored value) without a base64 detour.

export const CHUNK = 1 << 20;
// ctrl slots
const STATE = 0; // 0 idle, 1 a chunk is ready
const SEQ = 1;
const LEN = 2;
const DONE = 3;

export function makeBridgeMemory() {
  return { ctrl: new SharedArrayBuffer(16), data: new SharedArrayBuffer(CHUNK) };
}

const enc = new TextEncoder();
const dec = new TextDecoder();

export function encodeAnswer(json, body) {
  const head = enc.encode(JSON.stringify(json ?? null));
  const bytes = body ? (body instanceof Uint8Array ? body : new Uint8Array(body)) : new Uint8Array(0);
  const out = new Uint8Array(4 + head.length + bytes.length);
  new DataView(out.buffer).setUint32(0, head.length, true);
  out.set(head, 4);
  out.set(bytes, 4 + head.length);
  return out;
}

export function decodeAnswer(bytes) {
  const len = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0, true);
  const json = JSON.parse(dec.decode(bytes.subarray(4, 4 + len)));
  return { json, body: bytes.subarray(4 + len) };
}

// The worker's half. `post` is the worker's postMessage.
export function workerBridge({ ctrl, data }, post) {
  const c = new Int32Array(ctrl);
  const d = new Uint8Array(data);
  let seq = 0;
  return {
    // Blocks until the page answers. Returns { json, body }.
    call(op, args, transfer) {
      seq += 1;
      Atomics.store(c, STATE, 0);
      post({ t: 'call', seq, op, args }, transfer || []);
      const parts = [];
      let total = 0;
      for (;;) {
        Atomics.wait(c, STATE, 0);
        const len = Atomics.load(c, LEN);
        const done = Atomics.load(c, DONE) === 1;
        // Copy out of shared memory: decoders and wasm refuse shared views.
        parts.push(d.slice(0, len));
        total += len;
        Atomics.store(c, STATE, 0);
        if (done) break;
        post({ t: 'more', seq });
      }
      const all = new Uint8Array(total);
      let at = 0;
      for (const p of parts) {
        all.set(p, at);
        at += p.length;
      }
      return decodeAnswer(all);
    },
  };
}

// The page's half. `handlers[op](args)` returns (or resolves to)
// { json, body }. A handler that throws answers { err: { tag: 'other',
// val: <message> } } so the app always gets an answer it can read.
export function pageBridge({ ctrl, data }, handlers, onError) {
  const c = new Int32Array(ctrl);
  const d = new Uint8Array(data);
  const pending = new Map();
  const send = (seq, bytes, from) => {
    const len = Math.min(CHUNK, bytes.length - from);
    d.set(bytes.subarray(from, from + len), 0);
    Atomics.store(c, SEQ, seq);
    Atomics.store(c, LEN, len);
    const done = from + len >= bytes.length;
    Atomics.store(c, DONE, done ? 1 : 0);
    if (done) pending.delete(seq);
    else pending.set(seq, { bytes, from: from + len });
    Atomics.store(c, STATE, 1);
    Atomics.notify(c, STATE);
  };
  return {
    async onMessage(msg) {
      if (msg.t === 'more') {
        const p = pending.get(msg.seq);
        if (p) send(msg.seq, p.bytes, p.from);
        return;
      }
      if (msg.t !== 'call') return;
      let answer;
      try {
        const handler = handlers[msg.op];
        if (!handler) throw new Error(`the page has no ${msg.op}`);
        answer = await handler(msg.args);
      } catch (e) {
        if (onError) onError(msg.op, e);
        answer = { json: { err: { tag: 'other', val: String((e && e.message) || e) } } };
      }
      send(msg.seq, encodeAnswer(answer.json, answer.body), 0);
    },
  };
}
