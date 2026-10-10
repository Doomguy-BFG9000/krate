//! A build that carries one app as native code: `KRATE_NATIVE_IMAGE` (made
//! by the runtime's precompile_native_image for aarch64-apple-ios),
//! `KRATE_NATIVE_MANIFEST` (that app's manifest.toml) and
//! `KRATE_NATIVE_WASM` (the WebAssembly the image was compiled from, whose
//! hash lets the same app arriving as a `.krate` run as the native code).
fn main() {
    for var in [
        "KRATE_NATIVE_IMAGE",
        "KRATE_NATIVE_MANIFEST",
        "KRATE_NATIVE_WASM",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo::rustc-check-cfg=cfg(krate_native_image)");
    let (Ok(image), Ok(manifest), Ok(wasm)) = (
        std::env::var("KRATE_NATIVE_IMAGE"),
        std::env::var("KRATE_NATIVE_MANIFEST"),
        std::env::var("KRATE_NATIVE_WASM"),
    ) else {
        return;
    };
    for path in [&image, &manifest, &wasm] {
        println!("cargo:rerun-if-changed={path}");
    }
    use sha2::Digest;
    let bytes = std::fs::read(&wasm).expect("read KRATE_NATIVE_WASM");
    let hex: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("cargo:rustc-env=KRATE_NATIVE_SHA256={hex}");
    println!("cargo:rustc-cfg=krate_native_image");
}
