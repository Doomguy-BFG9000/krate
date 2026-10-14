//! wasmtime + pulley inside a tab, running apps/krate-diceroll: the
//! comparison IC-956 Phase 0 asked for. Only the five host functions that
//! app imports are defined; every other import would trap.

use wasmtime::component::{ComponentType, Lift, Lower, Resource, ResourceType};

#[cfg(target_arch = "wasm32")]
#[path = "../../src/platform.rs"]
mod platform;

/// One function for both ends, so the artifact always loads.
pub fn engine_config() -> wasmtime::Config {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    #[cfg(target_arch = "wasm32")]
    config.target("pulley32").expect("pulley32");
    config
}

pub struct OutStream;

#[derive(ComponentType, Lift, Lower, Debug)]
#[component(variant)]
pub enum IoError {
    #[component(name = "closed")]
    Closed,
    #[component(name = "interrupted")]
    Interrupted,
    #[component(name = "unexpected-eof")]
    UnexpectedEof,
    #[component(name = "invalid-utf8")]
    InvalidUtf8,
    #[component(name = "other")]
    Other(String),
}

#[derive(ComponentType, Lift, Lower, Debug)]
#[component(variant)]
pub enum RandomError {
    #[component(name = "denied")]
    Denied,
    #[component(name = "too-large")]
    TooLarge,
    #[component(name = "empty-range")]
    EmptyRange,
    #[component(name = "unavailable")]
    Unavailable(String),
}

/// Fills a buffer with random bytes.
pub type RandomSource = Box<dyn FnMut(&mut [u8]) + Send>;

pub struct Host {
    pub args: String,
    pub stdout: Vec<u8>,
    pub random: RandomSource,
}

/// Load a precompiled component and run it. Returns (exit code, stdout).
///
/// # Safety
/// `cwasm` must come from `precompile` with the same engine settings;
/// wasmtime checks the settings, but the bytes are trusted as code.
pub unsafe fn run_precompiled(cwasm: &[u8], host: Host) -> wasmtime::Result<(i32, Host)> {
    let engine = wasmtime::Engine::new(&engine_config())?;
    let component = unsafe { wasmtime::component::Component::deserialize(&engine, cwasm)? };
    let mut linker = wasmtime::component::Linker::<Host>::new(&engine);
    linker
        .instance("krate:io/args@0.1.0")?
        .func_wrap("raw", |store, (): ()| Ok((store.data().args.clone(),)))?;
    linker
        .instance("krate:io/stdio@0.1.0")?
        .func_wrap("stdout", |_store, (): ()| {
            Ok((Resource::<OutStream>::new_own(1),))
        })?;
    {
        let mut streams = linker.instance("krate:io/streams@0.1.0")?;
        streams.resource(
            "output-stream",
            ResourceType::host::<OutStream>(),
            |_, _| Ok(()),
        )?;
        streams.func_wrap(
            "[method]output-stream.write-all",
            |mut store, (_s, bytes): (Resource<OutStream>, Vec<u8>)| {
                store.data_mut().stdout.extend_from_slice(&bytes);
                Ok((Ok::<(), IoError>(()),))
            },
        )?;
        streams.func_wrap(
            "[method]output-stream.flush",
            |_store, (_s,): (Resource<OutStream>,)| Ok((Ok::<(), IoError>(()),)),
        )?;
    }
    linker.instance("krate:random/bytes@0.1.0")?.func_wrap(
        "get",
        |mut store, (count,): (u32,)| {
            let mut out = vec![0u8; count as usize];
            (store.data_mut().random)(&mut out);
            Ok((Ok::<Vec<u8>, RandomError>(out),))
        },
    )?;
    let mut store = wasmtime::Store::new(&engine, host);
    let instance = linker.instantiate(&mut store, &component)?;
    let run = instance.get_typed_func::<(), (i32,)>(&mut store, "run")?;
    let (code,) = run.call(&mut store, ())?;
    Ok((code, store.into_data()))
}

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::prelude::*;

    /// Run diceroll from its precompiled bytes; returns "code|stdout|ms" or
    /// the error sentence.
    #[wasm_bindgen]
    pub fn run_dice(cwasm: &[u8]) -> String {
        std::panic::set_hook(Box::new(|info| {
            web_sys_log(&format!("krate panic: {info}"));
        }));
        let host = super::Host {
            args: "5\n".to_string(),
            stdout: Vec::new(),
            // A timing probe, not a product: Math.random is enough to roll
            // dice. The real host uses crypto.getRandomValues (web/host.js).
            random: Box::new(|buf| {
                for b in buf.iter_mut() {
                    *b = (js_sys::Math::random() * 256.0) as u8;
                }
            }),
        };
        match unsafe { super::run_precompiled(cwasm, host) } {
            Ok((code, host)) => format!("{code}|{}", String::from_utf8_lossy(&host.stdout)),
            Err(err) => format!("error: {err:?}"),
        }
    }

    fn web_sys_log(text: &str) {
        let console = js_sys::Reflect::get(&js_sys::global(), &"console".into()).ok();
        if let Some(console) = console {
            if let Ok(error) = js_sys::Reflect::get(&console, &"error".into()) {
                let _ = js_sys::Function::from(error).call1(&console, &text.into());
            }
        }
    }
}
