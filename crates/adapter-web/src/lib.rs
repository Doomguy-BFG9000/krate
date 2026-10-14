//! The browser Player's own WebAssembly module (IC-956 Phase 1).
//!
//! A Krate app opens in a tab on the browser's own WebAssembly engine: its
//! component is transpiled once at publish (jco), and it runs in a Web
//! Worker that blocks in `Atomics.wait` the way a desktop app blocks in
//! `events.wait`. This crate is what that worker (and the page, for the
//! consent screen) loads beside it:
//!
//! - [`web_ui`]: the `krate:ui` host -- windows, trees, focus, scroll,
//!   keyboard and pointer routing, light and dark, and the frame, through
//!   the same `krate-lowering` step and painter the desktop uses;
//! - [`web_policy`]: the consent screen's words and the session's grants,
//!   through `krate-manifest` and `krate-policy`, including the
//!   `net.connect` judgement every request gets before the page fetches;
//! - [`web_kv`]: `krate:store/kv` under the desktop store's own rules.
//!
//! The text is vector text (parley + vello_cpu) in Geist Regular, bundled
//! from the site's own copy (`fonts/`, SIL Open Font License 1.1,
//! `fonts/OFL.txt`): a browser build finds no system fonts.
//!
//! Every rule is exercised by native tests; the wasm-bindgen face is
//! `bindings`, built only for wasm32.

pub mod web_canvas;
pub mod web_kv;
pub mod web_policy;
pub mod web_ui;

#[cfg(target_arch = "wasm32")]
mod bindings;

/// The bundled font, Geist Regular (OFL 1.1, `fonts/OFL.txt`).
pub const BUNDLED_FONT: &[u8] = include_bytes!("../fonts/Geist-Regular.ttf");

/// Hand the text engine the bundled font. A desktop build has system fonts
/// and never calls this; the Player calls it once at start.
pub fn use_bundled_font() -> bool {
    krate_adapter_common::vector_text::register_bundled_font(BUNDLED_FONT.to_vec())
}

// The symbols wasmtime asks a host it does not recognise to supply, for the
// Phase 0 pulley-in-a-tab comparison only (evidence/web-player).
#[cfg(all(target_arch = "wasm32", feature = "engine-probe"))]
mod platform;

#[cfg(test)]
mod tests {
    /// The bundled font is a font the text engine takes, and with it a
    /// label measures as vector text, not the 5x7 bitmap face.
    #[test]
    fn the_bundled_font_registers_and_measures() {
        assert!(super::use_bundled_font());
        let m = krate_adapter_common::vector_text::measure_canvas_text("Hello", 13.0)
            .expect("vector text measures");
        assert!(m.width > 20.0 && m.width < 60.0, "{}", m.width);
    }
}

/// Does wasmtime actually run in a browser tab?
///
/// Compiling and linking are not the same as running, and the answer
/// should come from a browser rather than a build log. This starts a real
/// engine and then asks it to load a real app's component -- the two steps
/// that decide whether an in-tab preview is possible at all.
///
/// Verified 2026-09-01: the engine STARTS. Loading a component then panics
/// with "time not implemented on this platform", because bare
/// wasm32-unknown-unknown has no clock and `std::time::Instant` is
/// unimplemented there. That is the next piece of work, and it is
/// plumbing -- the browser has `performance.now()`.
#[cfg(all(target_arch = "wasm32", feature = "engine-probe"))]
mod runtime_probe {
    use wasm_bindgen::prelude::*;

    /// A real app's compiled component, baked in so the probe answers its
    /// question without needing a fetch.
    ///
    /// Not committed -- it is build input. A browser build has no
    /// compiler, so this is a component ALREADY compiled for pulley, the
    /// interpreter a tab runs:
    ///
    /// ```text
    /// unzip -o evidence/demo/ratecard.krate code.wasm
    /// cargo run -p krate-runtime --example precompile_for_web -- \
    ///   code.wasm crates/adapter-web/probe-component.wasm
    /// ```
    const BUNDLED_COMPONENT: &[u8] = include_bytes!("../probe-component.wasm");

    #[wasm_bindgen]
    pub fn probe_engine() -> String {
        // wasm panics abort rather than unwind, so `catch_unwind` never
        // sees them and a browser reports only "RuntimeError:
        // unreachable". The hook is what turned that shrug into the real
        // sentence naming the clock.
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&format!("krate panic: {info}").into());
        }));
        // The engine's settings must match the ones the component was
        // compiled with, or wasmtime answers "compilation settings are not
        // compatible with the native host". A previewed app opens a
        // window, so this is a windowed mode -- the same one
        // `precompile_for_web` uses.
        let config = krate_runtime::Config {
            phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
            ..krate_runtime::Config::default()
        };
        let runtime = match krate_runtime::Runtime::new(&config) {
            Ok(runtime) => runtime,
            Err(err) => return format!("engine refused: {err}"),
        };
        match runtime.load_component(BUNDLED_COMPONENT) {
            Ok(_) => format!(
                "engine started and loaded a component ({} bytes)",
                BUNDLED_COMPONENT.len()
            ),
            Err(err) => format!("engine started, load refused: {err}"),
        }
    }
}
