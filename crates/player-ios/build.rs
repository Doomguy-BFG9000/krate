//! A wrapper build: `KRATE_NATIVE_IMAGE` (a native image made by the
//! runtime's precompile_native_image for aarch64-apple-ios) and
//! `KRATE_NATIVE_MANIFEST` (that app's manifest.toml) make this player
//! carry one app as native code instead of the demo.
fn main() {
    println!("cargo:rerun-if-env-changed=KRATE_NATIVE_IMAGE");
    println!("cargo:rerun-if-env-changed=KRATE_NATIVE_MANIFEST");
    println!("cargo::rustc-check-cfg=cfg(krate_native_image)");
    if let (Ok(image), Ok(manifest)) = (
        std::env::var("KRATE_NATIVE_IMAGE"),
        std::env::var("KRATE_NATIVE_MANIFEST"),
    ) {
        println!("cargo:rerun-if-changed={image}");
        println!("cargo:rerun-if-changed={manifest}");
        println!("cargo:rustc-cfg=krate_native_image");
    }
}
