//! Threaded apps (IC-929): an app built for `wasm32-wasip1-threads` runs on
//! as many threads as it spawns, all sharing its memory.
//!
//! Why a core module and not a component, measured on wasmtime 46:
//!
//! - wasi-threads works: one `SharedMemory`, one `Store` and instance of the
//!   same module per OS thread, `wasi.thread-spawn` answered by the host. A
//!   rayon pool of 8 threads ran 7.2-7.4x faster than one thread on an M4.
//! - A component cannot be shared: wit-component refuses a module that
//!   imports a shared memory ("mismatch in the shared flag for memories"),
//!   a component cannot import a memory at all, and wasmtime 46's component
//!   threading (`thread.new-indirect`) is cooperative threads in one store,
//!   not parallel ones.
//!
//! So a threaded app is the core module rustc and wit-bindgen already make,
//! and Krate links its `krate:*` imports itself. The glue is generated from
//! the WIT (build.rs writes `core_glue.rs`): each import lifts its arguments
//! out of the shared memory into the Rust types wasmtime's bindgen made,
//! calls the very `Host` trait method the component path calls, and lowers
//! the result back. There is one host state, behind a lock; the window, the
//! GPU surface and sound answer only the app's main thread, as AppKit wants.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use wasmtime::{Caller, Linker, Module, SharedMemory, Store, UpdateDeadline};

use super::{finish_gui_run, Config, HostState, OutputMode, Result, RunOutcome, RuntimeError};

/// Canonical ABI helpers the generated glue is written against. Every
/// primitive has its four, whether or not today's WIT uses it.
#[allow(dead_code)]
pub(crate) mod abi {
    use wasmtime::{SharedMemory, Val, ValType};

    /// A view of the app's shared memory.
    pub(crate) struct Cx {
        base: *mut u8,
        len: usize,
    }

    pub(crate) fn bad(what: &str) -> wasmtime::Error {
        wasmtime::Error::msg(format!("the app passed an invalid {what}"))
    }

    impl Cx {
        pub(crate) fn new(memory: &SharedMemory) -> Cx {
            let data = memory.data();
            Cx {
                base: data.as_ptr() as *mut u8,
                len: data.len(),
            }
        }

        pub(crate) fn check(&self, a: u32, n: u32) -> wasmtime::Result<()> {
            match (a as usize).checked_add(n as usize) {
                Some(end) if end <= self.len => Ok(()),
                _ => Err(wasmtime::Error::msg(format!(
                    "the app pointed past its memory ({a}+{n} > {})",
                    self.len
                ))),
            }
        }

        pub(crate) fn bytes(&self, a: u32, n: u32) -> wasmtime::Result<Vec<u8>> {
            self.check(a, n)?;
            let mut out = vec![0u8; n as usize];
            // SAFETY: in bounds. The memory is shared, so another thread may
            // write it at the same moment; that race is the app's own, as it
            // would be natively.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.base.add(a as usize),
                    out.as_mut_ptr(),
                    n as usize,
                )
            };
            Ok(out)
        }

        pub(crate) fn write(&self, a: u32, b: &[u8]) -> wasmtime::Result<()> {
            self.check(a, b.len() as u32)?;
            // SAFETY: as above.
            unsafe {
                std::ptr::copy_nonoverlapping(b.as_ptr(), self.base.add(a as usize), b.len())
            };
            Ok(())
        }

        fn read<const N: usize>(&self, a: u32) -> wasmtime::Result<[u8; N]> {
            self.check(a, N as u32)?;
            let mut out = [0u8; N];
            // SAFETY: as above.
            unsafe {
                std::ptr::copy_nonoverlapping(self.base.add(a as usize), out.as_mut_ptr(), N)
            };
            Ok(out)
        }

        pub(crate) fn string(&self, a: u32, n: u32) -> wasmtime::Result<String> {
            String::from_utf8(self.bytes(a, n)?).map_err(|_| bad("string"))
        }
    }

    /// Where lowering allocates in the app's memory: its `cabi_realloc`.
    pub(crate) trait Realloc {
        fn alloc(&mut self, size: u32, align: u32) -> wasmtime::Result<u32>;
    }

    pub(crate) fn i32_of(v: &Val) -> i32 {
        match v {
            Val::I32(x) => *x,
            Val::I64(x) => *x as i32,
            Val::F32(x) => *x as i32,
            Val::F64(x) => *x as i32,
            _ => 0,
        }
    }

    pub(crate) fn i64_of(v: &Val) -> i64 {
        match v {
            Val::I64(x) => *x,
            Val::I32(x) => *x as u32 as i64,
            Val::F32(x) => *x as i64,
            Val::F64(x) => *x as i64,
            _ => 0,
        }
    }

    pub(crate) fn f32_of(v: &Val) -> f32 {
        f32::from_bits(i32_of(v) as u32)
    }

    pub(crate) fn f64_of(v: &Val) -> f64 {
        f64::from_bits(i64_of(v) as u64)
    }

    /// A joined variant slot read back as the type a case wants.
    pub(crate) fn coerce(v: &Val, want: ValType) -> Val {
        match want {
            ValType::I32 => Val::I32(i32_of(v)),
            ValType::I64 => Val::I64(i64_of(v)),
            ValType::F32 => Val::F32(i32_of(v) as u32),
            ValType::F64 => Val::F64(i64_of(v) as u64),
            _ => *v,
        }
    }

    /// A case's flat value widened to its joined slot.
    pub(crate) fn widen(v: &Val, slot: ValType) -> Val {
        match (v, slot) {
            (Val::F32(b), ValType::I32) => Val::I32(*b as i32),
            (Val::F32(b), ValType::I64) => Val::I64(*b as i64),
            (Val::F64(b), ValType::I64) => Val::I64(*b as i64),
            (Val::I32(x), ValType::I64) => Val::I64(*x as u32 as i64),
            _ => *v,
        }
    }

    pub(crate) fn zero(t: ValType) -> Val {
        match t {
            ValType::I64 => Val::I64(0),
            ValType::F32 => Val::F32(0),
            ValType::F64 => Val::F64(0),
            _ => Val::I32(0),
        }
    }

    pub(crate) fn put_u8(cx: &Cx, a: u32, v: u8) -> wasmtime::Result<()> {
        cx.write(a, &[v])
    }
    pub(crate) fn put_u16(cx: &Cx, a: u32, v: u16) -> wasmtime::Result<()> {
        cx.write(a, &v.to_le_bytes())
    }
    pub(crate) fn put_u32(cx: &Cx, a: u32, v: u32) -> wasmtime::Result<()> {
        cx.write(a, &v.to_le_bytes())
    }

    /// One primitive's four helpers.
    macro_rules! prim {
        ($load:ident, $store:ident, $lift:ident, $lower:ident, $t:ty, $n:literal,
         |$b:ident| $from_bytes:expr, |$v:ident| $to_bytes:expr,
         |$f:ident| $from_flat:expr, |$w:ident| $to_flat:expr) => {
            pub(crate) fn $load(cx: &Cx, a: u32) -> wasmtime::Result<$t> {
                let $b = cx.read::<$n>(a)?;
                Ok($from_bytes)
            }
            pub(crate) fn $store(
                cx: &Cx,
                _rl: &mut dyn Realloc,
                a: u32,
                $v: $t,
            ) -> wasmtime::Result<()> {
                cx.write(a, &$to_bytes)
            }
            pub(crate) fn $lift(_cx: &Cx, f: &[Val]) -> wasmtime::Result<$t> {
                let $f = &f[0];
                Ok($from_flat)
            }
            pub(crate) fn $lower(
                _cx: &Cx,
                _rl: &mut dyn Realloc,
                $w: $t,
                out: &mut Vec<Val>,
            ) -> wasmtime::Result<()> {
                out.push($to_flat);
                Ok(())
            }
        };
    }

    prim!(
        load_bool,
        store_bool,
        lift_bool,
        lower_bool,
        bool,
        1,
        |b| b[0] != 0,
        |v| [v as u8],
        |f| i32_of(f) != 0,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_u8,
        store_u8,
        lift_u8,
        lower_u8,
        u8,
        1,
        |b| b[0],
        |v| [v],
        |f| i32_of(f) as u8,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_s8,
        store_s8,
        lift_s8,
        lower_s8,
        i8,
        1,
        |b| b[0] as i8,
        |v| [v as u8],
        |f| i32_of(f) as i8,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_u16,
        store_u16,
        lift_u16,
        lower_u16,
        u16,
        2,
        |b| u16::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i32_of(f) as u16,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_s16,
        store_s16,
        lift_s16,
        lower_s16,
        i16,
        2,
        |b| i16::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i32_of(f) as i16,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_u32,
        store_u32,
        lift_u32,
        lower_u32,
        u32,
        4,
        |b| u32::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i32_of(f) as u32,
        |w| Val::I32(w as i32)
    );
    prim!(
        load_s32,
        store_s32,
        lift_s32,
        lower_s32,
        i32,
        4,
        |b| i32::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i32_of(f),
        |w| Val::I32(w)
    );
    prim!(
        load_u64,
        store_u64,
        lift_u64,
        lower_u64,
        u64,
        8,
        |b| u64::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i64_of(f) as u64,
        |w| Val::I64(w as i64)
    );
    prim!(
        load_s64,
        store_s64,
        lift_s64,
        lower_s64,
        i64,
        8,
        |b| i64::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| i64_of(f),
        |w| Val::I64(w)
    );
    prim!(
        load_f32,
        store_f32,
        lift_f32,
        lower_f32,
        f32,
        4,
        |b| f32::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| f32_of(f),
        |w| Val::F32(w.to_bits())
    );
    prim!(
        load_f64,
        store_f64,
        lift_f64,
        lower_f64,
        f64,
        8,
        |b| f64::from_le_bytes(b),
        |v| v.to_le_bytes(),
        |f| f64_of(f),
        |w| Val::F64(w.to_bits())
    );
    prim!(
        load_char,
        store_char,
        lift_char,
        lower_char,
        char,
        4,
        |b| char::from_u32(u32::from_le_bytes(b)).unwrap_or('\u{fffd}'),
        |v| (v as u32).to_le_bytes(),
        |f| char::from_u32(i32_of(f) as u32).unwrap_or('\u{fffd}'),
        |w| Val::I32(w as i32)
    );

    pub(crate) fn load_string(cx: &Cx, a: u32) -> wasmtime::Result<String> {
        let p = load_u32(cx, a)?;
        let n = load_u32(cx, a + 4)?;
        cx.string(p, n)
    }
    pub(crate) fn store_string(
        cx: &Cx,
        rl: &mut dyn Realloc,
        a: u32,
        v: String,
    ) -> wasmtime::Result<()> {
        let n = v.len() as u32;
        let p = rl.alloc(n, 1)?;
        cx.write(p, v.as_bytes())?;
        put_u32(cx, a, p)?;
        put_u32(cx, a + 4, n)
    }
    pub(crate) fn lift_string(cx: &Cx, f: &[Val]) -> wasmtime::Result<String> {
        cx.string(i32_of(&f[0]) as u32, i32_of(&f[1]) as u32)
    }
    pub(crate) fn lower_string(
        cx: &Cx,
        rl: &mut dyn Realloc,
        v: String,
        out: &mut Vec<Val>,
    ) -> wasmtime::Result<()> {
        let n = v.len() as u32;
        let p = rl.alloc(n, 1)?;
        cx.write(p, v.as_bytes())?;
        out.push(Val::I32(p as i32));
        out.push(Val::I32(n as i32));
        Ok(())
    }
}

include!(concat!(env!("OUT_DIR"), "/core_glue.rs"));

/// A store's data: which app thread it runs.
pub(crate) struct Thread {
    #[allow(dead_code)]
    tid: i32,
}

/// The host state, made sendable. It is only ever touched under
/// `Shared::host`'s lock, one thread at a time, so its `Rc`s are never raced;
/// what must stay on the main thread (the window, AppKit) is refused to the
/// others before the lock is taken.
pub(crate) struct SendHost(HostState);

// SAFETY: see the type's documentation.
unsafe impl Send for SendHost {}

impl std::ops::Deref for SendHost {
    type Target = HostState;
    fn deref(&self) -> &HostState {
        &self.0
    }
}

impl std::ops::DerefMut for SendHost {
    fn deref_mut(&mut self) -> &mut HostState {
        &mut self.0
    }
}

/// Everything the app's threads share.
pub(crate) struct Shared {
    memory: SharedMemory,
    host: Mutex<SendHost>,
    main: std::thread::ThreadId,
    next_tid: AtomicI32,
    /// Most threads the app may run at once.
    max_threads: i32,
    live: AtomicI32,
}

impl Shared {
    pub(crate) fn cx(&self) -> abi::Cx {
        abi::Cx::new(&self.memory)
    }

    /// The host, for a call into `iface`. The window, the GPU surface and
    /// sound answer the main thread only.
    pub(crate) fn host(
        &self,
        main_only: bool,
        iface: &str,
    ) -> wasmtime::Result<MutexGuard<'_, SendHost>> {
        if main_only && std::thread::current().id() != self.main {
            return Err(wasmtime::Error::msg(format!(
                "krate:{iface} answers the app's main thread only: the window and \
                 the screen live there"
            )));
        }
        Ok(self.host.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub(crate) fn realloc<'a, 'b>(
        &self,
        caller: &'a mut Caller<'b, Thread>,
    ) -> CallerRealloc<'a, 'b> {
        CallerRealloc(caller)
    }
}

/// Allocation through the calling thread's `cabi_realloc`.
pub(crate) struct CallerRealloc<'a, 'b>(&'a mut Caller<'b, Thread>);

impl abi::Realloc for CallerRealloc<'_, '_> {
    fn alloc(&mut self, size: u32, align: u32) -> wasmtime::Result<u32> {
        if size == 0 {
            return Ok(align.max(1));
        }
        let f = self
            .0
            .get_export("cabi_realloc")
            .and_then(|e| e.into_func())
            .ok_or_else(|| wasmtime::Error::msg("the app has no cabi_realloc"))?;
        let f = f.typed::<(i32, i32, i32, i32), i32>(&self.0)?;
        Ok(f.call(&mut *self.0, (0, 0, align as i32, size as i32))? as u32)
    }
}

/// Whether `bytes` are a threaded app: a core module importing a shared
/// memory.
pub(crate) fn is_threaded_module(bytes: &[u8]) -> bool {
    if wasmparser::Parser::is_component(bytes) || !wasmparser::Parser::is_core_wasm(bytes) {
        return false;
    }
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        if let Ok(wasmparser::Payload::ImportSection(reader)) = payload {
            for import in reader.into_imports().flatten() {
                if let wasmparser::TypeRef::Memory(m) = import.ty {
                    if m.shared {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// How many threads the machine runs at once.
fn parallelism() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}

fn linker(
    engine: &wasmtime::Engine,
    shared: &Arc<Shared>,
    module: &Module,
) -> wasmtime::Result<Linker<Thread>> {
    let mut linker = Linker::new(engine);
    linker.allow_shadowing(true);
    glue::add_to_linker(&mut linker, shared)?;
    // The memory every thread shares.
    let memory_import = module
        .imports()
        .find(|i| matches!(i.ty(), wasmtime::ExternType::Memory(_)))
        .map(|i| (i.module().to_string(), i.name().to_string()));
    if let Some((m, n)) = memory_import {
        linker.define_unknown_imports_as_traps(module)?;
        let mut scratch = Store::new(engine, Thread { tid: -1 });
        linker.define(&mut scratch, &m, &n, shared.memory.clone())?;
    }
    // Sleeping holds no lock: a thread asleep must not stop the others
    // reaching the host.
    linker.func_wrap(
        "krate:time/sleep@0.1.0",
        "sleep-millis",
        |_: Caller<'_, Thread>, ms: i32| {
            std::thread::sleep(std::time::Duration::from_millis(ms as u32 as u64));
        },
    )?;
    linker.func_wrap(
        "krate:thread",
        "available-parallelism",
        |_: Caller<'_, Thread>| -> i32 { parallelism() },
    )?;
    let spawn_shared = shared.clone();
    let spawn_module = module.clone();
    linker.func_wrap(
        "wasi",
        "thread-spawn",
        move |caller: Caller<'_, Thread>, start_arg: i32| -> i32 {
            spawn(&spawn_shared, caller.engine(), &spawn_module, start_arg)
        },
    )?;
    Ok(linker)
}

/// `wasi.thread-spawn`: a new OS thread running a new instance of the app's
/// module over the same memory. A positive thread id, or a negative errno.
fn spawn(shared: &Arc<Shared>, engine: &wasmtime::Engine, module: &Module, start_arg: i32) -> i32 {
    let trace = std::env::var_os("KRATE_THREADS_TRACE").is_some();
    if trace {
        eprintln!("krate-threads: spawn requested (start arg {start_arg:#x})");
    }
    if shared.live.fetch_add(1, Ordering::SeqCst) >= shared.max_threads {
        shared.live.fetch_sub(1, Ordering::SeqCst);
        return -6; // EAGAIN
    }
    let tid = shared.next_tid.fetch_add(1, Ordering::SeqCst);
    let owner = shared.clone();
    let shared = shared.clone();
    let engine = engine.clone();
    let module = module.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("krate-app-{tid}"))
        .stack_size(8 << 20)
        .spawn(move || {
            let outcome = (|| -> wasmtime::Result<()> {
                let mut store = Store::new(&engine, Thread { tid });
                quiet_epoch(&mut store);
                let linker = linker(&engine, &shared, &module)?;
                let instance = linker.instantiate(&mut store, &module)?;
                let start =
                    instance.get_typed_func::<(i32, i32), ()>(&mut store, "wasi_thread_start")?;
                if trace {
                    eprintln!("krate-threads: thread {tid} starting");
                }
                let r = start.call(&mut store, (tid, start_arg));
                if trace {
                    eprintln!(
                        "krate-threads: thread {tid} ended: {:?}",
                        r.as_ref().map(|_| ())
                    );
                }
                r
            })();
            shared.live.fetch_sub(1, Ordering::SeqCst);
            if let Err(error) = outcome {
                // A thread's trap is the app's: end it with the reason, as a
                // native process ends on a crashed thread.
                eprintln!(
                    "error: the app's thread {tid} stopped: {}",
                    super::trap_message(&error)
                );
                std::process::exit(70);
            }
        });
    match spawned {
        Ok(_) => tid,
        Err(_) => {
            owner.live.fetch_sub(1, Ordering::SeqCst);
            -6
        }
    }
}

/// A worker's store: the engine may interrupt on epochs (the window's
/// pump), which a worker has nothing to do with.
fn quiet_epoch(store: &mut Store<Thread>) {
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Continue(1 << 20)));
}

impl super::Runtime {
    /// Run a threaded app (a core module importing a shared memory).
    pub(crate) fn run_threaded(
        &self,
        bytes: &[u8],
        config: &Config,
        output: OutputMode,
    ) -> Result<RunOutcome> {
        let engine = &self.engine;
        let module = Module::from_binary(engine, bytes)
            .map_err(|err| RuntimeError::InvalidComponent(err.to_string()))?;
        let memory_type = module
            .imports()
            .find_map(|i| match i.ty() {
                wasmtime::ExternType::Memory(m) => Some(m),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::InvalidComponent("no shared memory".to_string()))?;
        // The app's budget caps the memory every thread shares.
        let budget_pages = (config.memory_bytes / 65536).max(1);
        let max = memory_type
            .maximum()
            .unwrap_or(budget_pages)
            .min(budget_pages)
            .max(memory_type.minimum());
        let memory = SharedMemory::new(
            engine,
            wasmtime::MemoryType::shared(memory_type.minimum() as u32, max as u32),
        )
        .map_err(|err| RuntimeError::Instantiate(err.to_string()))?;

        let mut state = HostState::new(config, output)?;
        let gui_host = super::phase3_gui_host::Phase3GuiHost::new(
            super::UapiGuard::new(config.session_policy.clone()),
            config.phase3_ui_mode,
        )
        .map_err(|err| RuntimeError::Instantiate(super::phase3_ui::plain_reason(&err)))?
        .with_asset_root(config.bundle_assets_root.clone())
        .with_screenshot(
            config
                .screenshot_path
                .clone()
                .map(|path| (path, config.screenshot_scale)),
        )
        .with_usability(config.usability_plan.clone())
        .with_layout_check(config.check_layout)
        .with_chosen_files(state.chosen.clone())
        .with_stepped_clock(state.stepped_clock.clone())
        .with_app_name(config.app_name.clone());
        state.phase3_gui = Some(gui_host);

        let shared = Arc::new(Shared {
            memory,
            host: Mutex::new(SendHost(state)),
            main: std::thread::current().id(),
            next_tid: AtomicI32::new(1),
            max_threads: (parallelism() * 8).clamp(16, 512),
            live: AtomicI32::new(0),
        });
        let linker = linker(engine, &shared, &module)
            .map_err(|err| RuntimeError::Instantiate(err.to_string()))?;
        let mut store = Store::new(engine, Thread { tid: 0 });
        if let Some(fuel) = config.fuel {
            store
                .set_fuel(fuel)
                .map_err(|err| RuntimeError::EngineInit(err.to_string()))?;
        }
        if config.phase3_ui_mode.can_open_a_window() {
            let pump = shared.clone();
            store.set_epoch_deadline(super::EPOCH_TICKS_PER_PUMP);
            store.epoch_deadline_callback(move |_| {
                // Only when no other thread is in the host: the pump must
                // never wait behind a worker.
                if let Ok(host) = pump.host.try_lock() {
                    if let Some(gui) = host.phase3_gui.as_ref() {
                        gui.pump_native_windows_for_epoch();
                    }
                }
                Ok(UpdateDeadline::Continue(super::EPOCH_TICKS_PER_PUMP))
            });
            self.start_epoch_ticker();
        } else {
            quiet_epoch(&mut store);
        }
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| RuntimeError::Instantiate(super::instantiate_error(&err)))?;
        // The main thread's own pthread: wasi-libc sets it up in `_start` or
        // `_initialize`, and an app's `run` reaches neither. Without it the
        // first thread-local key walks an uninitialised thread list forever
        // (pthread_key_delete, under std's spawn hooks). Not `_initialize`:
        // that runs the constructors, and the SDK's entry runs them once
        // itself (K-1009).
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "__wasi_init_tp") {
            init.call(&mut store, ())
                .map_err(|err| RuntimeError::Trap(super::trap_message(&err)))?;
        }
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .map_err(|_| {
                RuntimeError::InvalidComponent(
                    "a threaded app exports `run: func() -> s32`".to_string(),
                )
            })?;
        let outcome = run.call(&mut store, ());
        let mut host = shared.host.lock().unwrap_or_else(|p| p.into_inner());
        finish_gui_run(&mut host, outcome)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_glue_covers_the_gui_world() {
        // Every function and resource drop of the Phase 4 gui world.
        const { assert!(super::glue::FUNCTIONS > 200) };
    }

    #[test]
    fn a_component_is_not_a_threaded_app() {
        let component = wat::parse_str("(component)").unwrap();
        assert!(!super::is_threaded_module(&component));
        let plain = wat::parse_str("(module (memory 1))").unwrap();
        assert!(!super::is_threaded_module(&plain));
        let threaded =
            wat::parse_str("(module (import \"env\" \"memory\" (memory 1 2 shared)))").unwrap();
        assert!(super::is_threaded_module(&threaded));
    }
}
