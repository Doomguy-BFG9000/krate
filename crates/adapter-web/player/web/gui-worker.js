// The worker half of the Worker + Atomics path: the app's run() blocks here,
// in events.wait, on Atomics.wait over the page's input ring.
import init, { WebWindow } from '../build/painter/krate_adapter_web.js';
import { ioHost, uiHost, GRANTS, instantiateApp } from './host.js';

const abs = () => performance.timeOrigin + performance.now();

self.onmessage = async ({ data }) => {
  if (data.type !== 'start') return;
  const marks = {};
  const memories = [];
  try {
    const t = performance.now();
    const painter = await init();
    marks.painter = performance.now() - t;

    const { ring, draw } = data;
    const SLOTS = 64, SLOT = 256, HEAD = 64;
    const ctrl = new Int32Array(ring, 0, 2);
    const bytes = new Uint8Array(ring);
    const dv = new DataView(ring);
    const dec = new TextDecoder();
    const waitRaw = (timeout) => {
      let head = Atomics.load(ctrl, 0);
      const tail = Atomics.load(ctrl, 1);
      if (head === tail) {
        Atomics.wait(ctrl, 0, head, timeout === undefined ? Infinity : timeout);
        head = Atomics.load(ctrl, 0);
        if (head === tail) return undefined;
      }
      const off = HEAD + (tail % SLOTS) * SLOT;
      const len = dv.getUint32(off, true);
      // TextDecoder refuses a view of shared memory, so copy the slot out.
      const raw = JSON.parse(dec.decode(bytes.slice(off + 4, off + 4 + len)));
      Atomics.store(ctrl, 1, tail + 1);
      return raw;
    };

    const ctx = draw === 'offscreen' ? data.canvas.getContext('2d') : null;
    let frame = null;
    const imports = {
      ...ioHost({ args: '', grants: GRANTS.hello, onOutput: (fd, text) => self.postMessage({ type: 'stdout', text }) }),
      ...uiHost({
        WebWindow,
        onWindow(id, title, width, height) {
          if (ctx) {
            data.canvas.width = width; data.canvas.height = height;
            self.postMessage({ type: 'window', width, height });
          } else {
            frame = new SharedArrayBuffer(width * height * 4);
            self.postMessage({ type: 'window', width, height, frame });
          }
        },
        present(id, w, inputAt) {
          const view = new Uint8ClampedArray(painter.memory.buffer, w.surface.rgba_ptr(), w.surface.rgba_len());
          if (ctx) ctx.putImageData(new ImageData(view, w.width, w.height), 0, 0);
          else new Uint8Array(frame).set(view);
          self.postMessage({ type: 'frame', width: w.width, height: w.height, inputAt, paintedAt: abs() });
        },
        waitRaw,
      }),
    };
    const app = await instantiateApp('../build/hello-sync', imports, memories, marks);
    self.postMessage({ type: 'marks', marks,
      memories: memories.map((m) => m.buffer.byteLength).concat([painter.memory.buffer.byteLength]) });
    // Blocks until the person closes the window.
    const code = app.run();
    self.postMessage({ type: 'exit', code });
  } catch (e) {
    self.postMessage({ type: 'error', error: String(e && e.stack || e) });
  }
};
