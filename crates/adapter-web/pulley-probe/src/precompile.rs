//! Precompile a Krate component for pulley64, with the exact engine settings
//! the in-tab engine uses (`krate_pulley_probe::engine_config`).
//!
//! ```text
//! cargo run --release --bin precompile -- in.wasm out.cwasm
//! ```

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: precompile <component.wasm> <out.cwasm>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&input).expect("read the component");
    let mut config = krate_pulley_probe::engine_config();
    config.target("pulley32").expect("pulley32 target");
    let engine = wasmtime::Engine::new(&config).expect("engine");
    let started = std::time::Instant::now();
    let out = engine.precompile_component(&bytes).expect("precompile");
    std::fs::write(&output, &out).expect("write");
    println!(
        "{input} ({} bytes) -> {output} ({} bytes) in {:?}",
        bytes.len(),
        out.len(),
        started.elapsed()
    );
}

#[cfg(target_arch = "wasm32")]
fn main() {}
