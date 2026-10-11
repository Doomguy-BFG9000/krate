# Threads for Krate apps (IC-929)

M4 Mac (4 performance + 6 efficiency cores), 2026-10-11, Krate from the GPU
worktree branch.

## What wasmtime 46 does (measured before choosing)

- wasi-threads, core module: one `SharedMemory`, one `Store` and instance of
  the same module per OS thread, `wasi.thread-spawn` answered by the host. A
  rayon pool of 8 threads ran a CPU job 7.2-7.4x faster than one thread
  (scratch harness, wasmtime 46.0.3 with the threads feature).
- A component cannot carry it. `wasm-tools component new` on a module that
  imports a shared memory: "failed to validate component output ...
  mismatch in the shared flag for memories". A component cannot import a
  memory, and wasmtime 46's component threading (`thread.new-indirect`) runs
  cooperative threads inside one store, not parallel ones (its docs: "very
  incomplete").

So a threaded app is a core module built for wasm32-wasip1-threads, and
Krate links its `krate:*` imports itself, with glue generated from the WIT
(crates/runtime/build.rs) that calls the same host trait methods the
component path calls.

## Proof (crates/cli/tests/cli.rs, through the krate binary)

- apps/krate-threads-probe: `available_parallelism` Ok(10); a std::thread
  joined; four scoped threads sharing one counter (400000); 640 CPU jobs on
  one thread 318 ms, on rayon's 10-thread pool 44-45 ms: 7.15-7.30x, same
  answer.
- apps/krate-tokio-probe: a tokio multi-thread runtime, 128 tasks on 10
  worker threads 9-10 ms against 65-74 ms on a current-thread runtime
  (6.5-7.8x); an mpsc channel between tasks; spawn_blocking; a 100 ms timer
  took 106 ms. Needs `--cfg tokio_unstable` (tokio's own gate for wasm).
- Sabotage: `wasi.thread-spawn` answering EAGAIN fails both tests.
