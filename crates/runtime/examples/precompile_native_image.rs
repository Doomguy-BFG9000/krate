//! Compile a Krate app to native code for a host that cannot compile at
//! run time (iOS), as an image to build into that host's binary.
//!
//! ```text
//! cargo run -p krate-runtime --example precompile_native_image -- \
//!     in.wasm out.cwasm aarch64-apple-ios manifest.toml
//! ```
//!
//! The manifest's `memory.budget` is part of the image's memory layout, so
//! it is read here exactly as the iOS Player reads it.

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output), Some(target), Some(manifest)) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: precompile_native_image <component.wasm> <out.cwasm> <target triple> <manifest.toml>"
        );
        std::process::exit(2);
    };
    let bytes = std::fs::read(&input).expect("read the component");
    // Windowed, as the app will run: the UI mode decides whether epoch
    // interruption is compiled in, and the loader must match it.
    let manifest = krate_manifest::Manifest::parse(
        &std::fs::read_to_string(&manifest).expect("read the manifest"),
    )
    .expect("parse the manifest");
    let mut config = krate_runtime::Config {
        phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
        ..krate_runtime::Config::default()
    };
    if let Some(mib) = memory_budget_mib(&manifest) {
        config.memory_bytes = mib * 1024 * 1024;
    }
    let compiled = krate_runtime::Runtime::precompile_native_image(&config, &bytes, &target)
        .expect("precompile a native image");
    std::fs::write(&output, &compiled).expect("write the image");
    println!(
        "{input} ({} bytes) -> {output} ({} bytes) for {target}",
        bytes.len(),
        compiled.len()
    );
}

/// The same reading as crates/player-ios: `memory.budget:<MiB>`, if declared.
fn memory_budget_mib(manifest: &krate_manifest::Manifest) -> Option<u64> {
    manifest
        .capabilities
        .iter()
        .find_map(|c| c.cap.strip_prefix("memory.budget:")?.parse::<u64>().ok())
}
