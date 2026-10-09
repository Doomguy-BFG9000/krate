//! Compile a Krate app to native code for a host that cannot compile at
//! run time (iOS), as an image to build into that host's binary.
//!
//! ```text
//! cargo run -p krate-runtime --example precompile_native_image -- \
//!     in.wasm out.cwasm aarch64-apple-ios
//! ```

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output), Some(target)) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: precompile_native_image <component.wasm> <out.cwasm> <target triple>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&input).expect("read the component");
    // Windowed, as the app will run: the UI mode decides whether epoch
    // interruption is compiled in, and the loader must match it.
    let config = krate_runtime::Config {
        phase3_ui_mode: krate_runtime::phase3_ui::Phase3HostUiMode::NativePrototype,
        ..krate_runtime::Config::default()
    };
    let compiled = krate_runtime::Runtime::precompile_native_image(&config, &bytes, &target)
        .expect("precompile a native image");
    std::fs::write(&output, &compiled).expect("write the image");
    println!(
        "{input} ({} bytes) -> {output} ({} bytes) for {target}",
        bytes.len(),
        compiled.len()
    );
}
