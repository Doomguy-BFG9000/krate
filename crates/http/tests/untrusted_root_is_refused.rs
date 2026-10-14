//! A certificate from a root this computer does not trust is refused, and
//! the refusal is explained in words a person can act on (IC-949 N). The
//! other half -- the same root trusted once the computer trusts it -- is
//! `tests/trusted_root_is_accepted_on_linux.rs`.

mod common;

#[test]
fn a_root_the_computer_does_not_trust_is_refused_with_words() {
    let url = common::serve_once();
    let err = krate_http::get(&url)
        .call()
        .expect_err("a root nobody installed must not be trusted");
    let text = err.to_string();
    let words = krate_http::explain(&text)
        .unwrap_or_else(|| panic!("an untrusted certificate must be explained, got: {text}"));
    assert!(words.contains("IT team"), "{words}");
}
