//! The company's root, once the computer trusts it, is trusted by Krate too
//! (IC-949 N). On Linux the system store can be pointed at a file without
//! touching the machine (`SSL_CERT_FILE`, read by the system-store loader),
//! which makes this provable in CI; on macOS and Windows the store is the
//! keychain and the certificate store, which a test must not change.
//!
//! Its own test binary, so the trust settings are read for the first time
//! after the variable is set.
#![cfg(target_os = "linux")]

mod common;

#[test]
fn the_companys_root_is_trusted_once_the_computer_trusts_it() {
    let ca = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ca.pem");
    // SAFETY-free on edition 2021; nothing else in this process reads it yet.
    std::env::set_var("SSL_CERT_FILE", ca);
    std::env::remove_var("SSL_CERT_DIR");
    let url = common::serve_once();
    let body = krate_http::get(&url)
        .call()
        .expect("the root the system store holds is trusted")
        .into_string()
        .unwrap();
    assert_eq!(body, "ok");
}
