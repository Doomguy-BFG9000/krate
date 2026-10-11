//! Signing the person in to a service through the system's sheet (IC-923):
//! the PKCE pair, the checks on what an app may ask the sheet to open, and
//! the headless stand-in tests drive the flow with.
//!
//! The sheet itself is the adapter's (ASWebAuthenticationSession on a Mac
//! and an iPhone). Nothing here sees a password: the service's page runs in
//! the system browser, and the app gets back only the address the service
//! sends the person to.

use sha2::{Digest, Sha256};

/// URL-safe base64 without padding (RFC 4648 §5), as PKCE wants it.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        let digits = chunk.len() + 1;
        for i in 0..digits {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// The S256 challenge for a verifier (RFC 7636 §4.2).
pub(crate) fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// A fresh verifier (43 characters from 32 random bytes) and its challenge.
pub(crate) fn pkce() -> (String, String) {
    let mut bytes = [0u8; 32];
    if crate::random_host::fill(&mut bytes).is_err() {
        // No randomness is no sign-in, never a guessable verifier: an empty
        // pair makes the service refuse the request.
        return (String::new(), String::new());
    }
    let verifier = base64url(&bytes);
    let challenge = challenge(&verifier);
    (verifier, challenge)
}

/// Why a request cannot open the sheet.
pub(crate) fn check(authorize_url: &str, scheme: &str) -> Result<(), String> {
    let Some(rest) = authorize_url.strip_prefix("https://") else {
        return Err("the sign-in address must start with https://".to_string());
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c == '@') {
        return Err(format!("{authorize_url} has no plain host to sign in at"));
    }
    let plain = !scheme.is_empty()
        && scheme.len() <= 64
        && scheme.starts_with(|c: char| c.is_ascii_lowercase())
        && scheme
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c));
    if !plain {
        return Err(format!(
            "{scheme:?} is not a callback scheme: lowercase letters, digits, + - ."
        ));
    }
    if matches!(
        scheme,
        "http" | "https" | "file" | "krate" | "javascript" | "data" | "about" | "mailto" | "tel"
    ) {
        return Err(format!("{scheme} cannot be an app's callback scheme"));
    }
    Ok(())
}

/// One query value of an address, undecoded.
pub(crate) fn query_value<'a>(address: &'a str, name: &str) -> Option<&'a str> {
    let query = address.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// What `KRATE_SIGN_IN` stands in for, headless: `cancel` is the person
/// closing the sheet; anything else is the code the service sends back,
/// with the request's own `state`, to `<scheme>://signed-in`.
pub(crate) fn stand_in(
    answer: &str,
    authorize_url: &str,
    scheme: &str,
) -> Result<Option<String>, String> {
    if answer == "cancel" {
        return Ok(None);
    }
    let mut back = format!("{scheme}://signed-in?code={answer}");
    if let Some(state) = query_value(authorize_url, "state") {
        back.push_str("&state=");
        back.push_str(state);
    }
    Ok(Some(back))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B: the specification's own verifier and challenge.
    #[test]
    fn the_challenge_is_the_one_the_specification_gives() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_fresh_pair_is_long_enough_and_never_repeats() {
        let (a, ca) = pkce();
        let (b, _) = pkce();
        assert_eq!(a.len(), 43, "32 random bytes, base64url: {a}");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(a, b);
        assert_eq!(ca, challenge(&a));
    }

    #[test]
    fn only_https_and_a_plain_scheme_reach_the_sheet() {
        assert!(check("https://github.com/login/oauth/authorize?x=1", "myapp").is_ok());
        assert!(check("http://github.com/login", "myapp").is_err());
        assert!(check("https:///login", "myapp").is_err());
        assert!(check("https://user@evil.com/", "myapp").is_err());
        assert!(check("https://github.com/", "https").is_err());
        assert!(check("https://github.com/", "krate").is_err());
        assert!(check("https://github.com/", "My App").is_err());
        assert!(check("https://github.com/", "").is_err());
    }

    #[test]
    fn the_stand_in_sends_back_the_request_state() {
        let url = "https://example.com/authorize?client_id=c&state=s123&code_challenge=x";
        assert_eq!(
            stand_in("abc", url, "myapp").unwrap().as_deref(),
            Some("myapp://signed-in?code=abc&state=s123")
        );
        assert_eq!(stand_in("cancel", url, "myapp").unwrap(), None);
        assert_eq!(
            query_value("myapp://signed-in?code=abc&state=s123", "state"),
            Some("s123")
        );
    }
}
