//! Offline access to an organisation's private app (IC-948, Addendum 3 G).
//!
//! A member who opens an organisation's app while online is handed a lease
//! by the hub: a short statement, signed with the hub's lease key, that this
//! principal may open these exact bytes until a date. While the machine is
//! offline, Krate opens the app only if a stored lease for that digest and
//! that principal verifies under the pinned key and has not expired.
//!
//! This crate is the lease itself -- its format, its signature check and the
//! pinned key. Fetching, storing and deciding when to check in live in the
//! CLI. Kept apart so the release build's handling of the key override can be
//! proved on its own: `cargo test --release -p krate-lease` runs
//! [`tests::a_release_build_ignores_the_override`], which only exists in a
//! build without debug assertions.
//!
//! The honest limit: a lease is enforced by Krate on the device. Someone who
//! copies the bytes and rebuilds them is not opening the organisation's signed
//! app, but nothing here can stop bytes from being copied.

use ring::signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde_json::Value;

/// The environment variable that replaces the pinned key, honoured only in a
/// debug or test build -- never in a release build.
pub const LEASE_KEY_OVERRIDE_ENV: &str = "KRATE_TEST_LEASE_KEY";

/// The hub's production lease public key (Ed25519, hex), pinned in every
/// release. The hub serves the same key at `/lease-key`, which is for
/// checking this pin, never for trusting a new key. An `Option` so a build
/// without a key fails safe: no lease verifies, and a private app opens only
/// while online.
pub const PINNED_LEASE_KEY: Option<&str> =
    Some("51c5f9471f2233f7080059eed2ea48cff14ccb7f65f285bcf8dbf45e9ffb8a4d");

/// The pinned key's id: the first 16 hex of sha256 of its 32 bytes, as
/// `/lease-key` and every signed lease name it.
pub const PINNED_LEASE_KEY_ID: &str = "c9d613381c02c8f2";

/// The id of a 32-byte lease public key, as the hub computes it.
pub fn key_id(key: &[u8; 32]) -> String {
    hex_of(ring::digest::digest(&ring::digest::SHA256, key).as_ref())[..16].to_string()
}

/// The key a lease must verify under: the override in a debug build when it
/// is set, otherwise the pinned key.
pub fn lease_key() -> Option<[u8; 32]> {
    lease_key_with(
        cfg!(debug_assertions),
        std::env::var(LEASE_KEY_OVERRIDE_ENV).ok().as_deref(),
    )
}

/// [`lease_key`] with its two inputs made explicit, so both branches can be
/// tested in one build.
pub fn lease_key_with(debug_build: bool, override_hex: Option<&str>) -> Option<[u8; 32]> {
    if debug_build {
        if let Some(key) = override_hex.and_then(parse_key) {
            return Some(key);
        }
    }
    PINNED_LEASE_KEY.and_then(parse_key)
}

/// 64 hex characters as a 32-byte key.
pub fn parse_key(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The lease classes, shortest first. `online` has no lease at all: every
/// open checks in.
pub const CLASSES: [&str; 3] = ["online", "days-7", "days-30"];

/// How long a class lets an app work offline, for comparing two of them: a
/// shorter class shows the notice again (R26). Unknown classes rank as
/// `online`, the strictest.
pub fn class_rank(class: &str) -> usize {
    CLASSES.iter().position(|c| *c == class).unwrap_or(0)
}

/// What a lease says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub v: u64,
    pub org: String,
    pub digest: String,
    pub principal: String,
    pub class: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub lease_id: String,
}

impl Lease {
    /// Is it past half its life at `now`? Then it is refreshed on the next
    /// online open.
    pub fn past_half_life(&self, now: u64) -> bool {
        let life = self.expires_at.saturating_sub(self.issued_at);
        now >= self.issued_at.saturating_add(life / 2)
    }
}

/// What the hub's lease answer turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The app's class is `online`: there is no lease, every open checks in.
    OnlineOnly,
    /// A signed lease. `stored` is the whole answer, kept on disk as it came.
    Signed { lease: Lease, stored: Value },
}

/// Why a stored lease does not let the app open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This build has no key to check a lease with.
    NoKey,
    /// Not a lease, or one in a format this Krate does not read.
    Malformed(String),
    /// The signature does not verify under the pinned key.
    BadSignature,
    /// A lease for other bytes.
    OtherApp,
    /// A lease for somebody else.
    OtherPerson,
    /// It ran out at `expires_at`.
    Expired { expires_at: u64 },
}

// ------------------------------------------------------------------------
// The format. Everything that knows the lease's JSON shape and the exact
// bytes its signature covers is in the two functions below, so the
// contract's "Appendix: lease format" is filled in here and nowhere else.
//
// It reads what the hub's lease code signs (cloud/worker/src/org-lease.js,
// "krate-lease-v1"): the lease Addendum 3 G names --
// `{v, org, digest, principal, class, issued_at, expires_at, lease_id}` --
// carried as `{"lease": {...}, "signature": "<hex>", "key_id", "alg"}`, the
// signature an Ed25519 one over "krate-lease-v1\n" followed by the lease's
// canonical JSON (keys sorted, no whitespace). The online class answers
// `{"class": "online", "lease": null}`.
// ------------------------------------------------------------------------

/// Split the hub's answer into the signed body and the signature bytes, or
/// say it is the online-only answer.
fn split_answer(answer: &Value) -> Result<Option<(&Value, Vec<u8>)>, Refusal> {
    if answer["lease"].is_null() && answer["class"].as_str() == Some("online") {
        return Ok(None);
    }
    let body = &answer["lease"];
    if !body.is_object() {
        return Err(Refusal::Malformed("there is no lease in it".into()));
    }
    let signature = answer["signature"]
        .as_str()
        .and_then(hex_bytes)
        .filter(|s| s.len() == 64)
        .ok_or_else(|| Refusal::Malformed("its signature is missing or not hex".into()))?;
    Ok(Some((body, signature)))
}

/// The exact bytes the hub signs for a lease body.
pub fn signing_bytes(body: &Value) -> Vec<u8> {
    format!("{LEASE_DOMAIN}{}", canonical_json(body)).into_bytes()
}

/// What every v1 lease signature begins with, so a signature over anything
/// else the hub's key signs can never pass as a lease.
pub const LEASE_DOMAIN: &str = "krate-lease-v1\n";

// ------------------------------------------------------------------------

/// Read the hub's answer: online-only, or a lease whose signature verifies.
pub fn read_answer(answer: &Value, key: &[u8; 32]) -> Result<Answer, Refusal> {
    match split_answer(answer)? {
        None => Ok(Answer::OnlineOnly),
        Some(_) => Ok(Answer::Signed {
            lease: verify(answer, key)?,
            stored: answer.clone(),
        }),
    }
}

/// The lease a stored answer holds, if its signature verifies under `key`.
pub fn verify(stored: &Value, key: &[u8; 32]) -> Result<Lease, Refusal> {
    let Some((body, signature_bytes)) = split_answer(stored)? else {
        return Err(Refusal::Malformed("it is an online-only answer".into()));
    };
    UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(&signing_bytes(body), &signature_bytes)
        .map_err(|_| Refusal::BadSignature)?;
    lease_of(body)
}

/// May `principal` open the bytes `digest` offline at `now`, by this stored
/// lease? The whole offline decision.
pub fn check(
    stored: &Value,
    key: Option<&[u8; 32]>,
    org: &str,
    digest: &str,
    principal: &str,
    now: u64,
) -> Result<Lease, Refusal> {
    let key = key.ok_or(Refusal::NoKey)?;
    let lease = verify(stored, key)?;
    if !lease.org.eq_ignore_ascii_case(org) || !lease.digest.eq_ignore_ascii_case(digest) {
        return Err(Refusal::OtherApp);
    }
    if !lease.principal.eq_ignore_ascii_case(principal) {
        return Err(Refusal::OtherPerson);
    }
    if now >= lease.expires_at {
        return Err(Refusal::Expired {
            expires_at: lease.expires_at,
        });
    }
    Ok(lease)
}

fn lease_of(body: &Value) -> Result<Lease, Refusal> {
    let text = |key: &str| {
        body[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Refusal::Malformed(format!("it has no {key}")))
    };
    let number = |key: &str| {
        body[key]
            .as_u64()
            .ok_or_else(|| Refusal::Malformed(format!("it has no {key}")))
    };
    let v = number("v")?;
    if v != 1 {
        return Err(Refusal::Malformed(format!(
            "it is lease format {v}, which this Krate does not read"
        )));
    }
    let lease = Lease {
        v,
        org: text("org")?,
        digest: text("digest")?,
        principal: text("principal")?,
        class: text("class")?,
        issued_at: number("issued_at")?,
        expires_at: number("expires_at")?,
        lease_id: text("lease_id")?,
    };
    // The class names the life exactly, and nothing is longer than 30 days
    // (the contract's appendix): a signature on anything else is not a
    // lease this Krate honours.
    let life = match lease.class.as_str() {
        "days-7" => 7 * 86_400,
        "days-30" => 30 * 86_400,
        other => {
            return Err(Refusal::Malformed(format!(
                "its class is {other}, which has no offline access"
            )))
        }
    };
    if lease.expires_at.checked_sub(lease.issued_at) != Some(life) {
        return Err(Refusal::Malformed(
            "its life does not match its class".to_string(),
        ));
    }
    Ok(lease)
}

/// JSON with every object's keys sorted and no whitespace.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sign a lease body with a PKCS#8 Ed25519 key, in the format above: what
/// the hub does, for tests and fakes that stand in for it.
pub fn sign(body: &Value, pkcs8: &[u8]) -> Option<Value> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).ok()?;
    let signature = pair.sign(&signing_bytes(body));
    Some(serde_json::json!({ "lease": body, "signature": hex_of(signature.as_ref()) }))
}

/// The public key of a PKCS#8 Ed25519 key, as hex.
pub fn public_key_hex(pkcs8: &[u8]) -> Option<String> {
    Ed25519KeyPair::from_pkcs8(pkcs8)
        .ok()
        .map(|pair| hex_of(pair.public_key().as_ref()))
}

/// A fresh PKCS#8 Ed25519 key, for tests.
pub fn generate_pkcs8() -> Option<Vec<u8>> {
    Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .ok()
        .map(|doc| doc.as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK: u64 = 7 * 86_400;

    /// A days-7 lease that runs out at `expires_at`.
    fn body(expires_at: u64) -> Value {
        serde_json::json!({
            "v": 1, "org": "acme", "digest": "ab".repeat(32),
            "principal": "github:dan", "class": "days-7",
            "issued_at": expires_at - WEEK, "expires_at": expires_at, "lease_id": "l1",
        })
    }

    fn keyed() -> (Vec<u8>, [u8; 32]) {
        let pkcs8 = generate_pkcs8().expect("key");
        let key = parse_key(&public_key_hex(&pkcs8).expect("public")).expect("hex");
        (pkcs8, key)
    }

    /// Far enough past the epoch for a week to fit before it.
    const END: u64 = 2_000_000_000;

    #[test]
    fn a_lease_opens_its_own_app_for_its_own_person_until_it_runs_out() {
        let (pkcs8, key) = keyed();
        let stored = sign(&body(END), &pkcs8).expect("signed");
        let digest = "ab".repeat(32);
        let during = END - 1;
        let lease =
            check(&stored, Some(&key), "acme", &digest, "github:dan", during).expect("opens");
        assert_eq!(lease.class, "days-7");
        assert_eq!(
            check(&stored, Some(&key), "acme", &digest, "github:dan", END),
            Err(Refusal::Expired { expires_at: END })
        );
        assert_eq!(
            check(
                &stored,
                Some(&key),
                "acme",
                &"cd".repeat(32),
                "github:dan",
                during
            ),
            Err(Refusal::OtherApp)
        );
        assert_eq!(
            check(&stored, Some(&key), "globex", &digest, "github:dan", during),
            Err(Refusal::OtherApp),
            "a lease names its organisation too"
        );
        assert_eq!(
            check(
                &stored,
                Some(&key),
                "acme",
                &digest,
                "email:dan@acme.example",
                during
            ),
            Err(Refusal::OtherPerson)
        );
        assert_eq!(
            check(&stored, None, "acme", &digest, "github:dan", during),
            Err(Refusal::NoKey)
        );
    }

    /// The appendix: the class names the life exactly, and nothing is
    /// longer than thirty days -- whoever signed it.
    #[test]
    fn a_lease_whose_life_does_not_match_its_class_is_not_honoured() {
        let (pkcs8, key) = keyed();
        let mut long = body(END);
        long["issued_at"] = serde_json::json!(END - 8 * 86_400);
        let stored = sign(&long, &pkcs8).unwrap();
        assert!(matches!(verify(&stored, &key), Err(Refusal::Malformed(_))));
        let mut forever = body(END);
        forever["class"] = serde_json::json!("days-365");
        forever["issued_at"] = serde_json::json!(END - 365 * 86_400);
        let stored = sign(&forever, &pkcs8).unwrap();
        assert!(matches!(verify(&stored, &key), Err(Refusal::Malformed(_))));
        let mut month = body(END);
        month["class"] = serde_json::json!("days-30");
        month["issued_at"] = serde_json::json!(END - 30 * 86_400);
        assert!(verify(&sign(&month, &pkcs8).unwrap(), &key).is_ok());
    }

    /// The contract's own test vector (Appendix: lease format), byte for
    /// byte and signature for signature.
    #[test]
    fn the_contracts_test_vector_holds() {
        let lease = serde_json::json!({
            "v": 1, "org": "acme",
            "digest": "0000000000000000000000000000000000000000000000000000000000000001",
            "principal": "email:erin@corp.example", "class": "days-7",
            "issued_at": 1_800_000_000u64, "expires_at": 1_800_604_800u64,
            "lease_id": "00112233445566778899aabbccddeeff",
        });
        assert_eq!(
            String::from_utf8(signing_bytes(&lease)).unwrap(),
            "krate-lease-v1\n{\"class\":\"days-7\",\"digest\":\"0000000000000000000000000000000000000000000000000000000000000001\",\"expires_at\":1800604800,\"issued_at\":1800000000,\"lease_id\":\"00112233445566778899aabbccddeeff\",\"org\":\"acme\",\"principal\":\"email:erin@corp.example\",\"v\":1}"
        );
        let answer = serde_json::json!({
            "lease": lease,
            "signature": "bd8f037118eb8edab4d6f1c55032dda0cd5e27f358e1772cc34e7a642f9a868e727613baa8eaa690241403eed28d989a58a016eb13ec80190c8fc92979a38904",
            "key_id": "x", "alg": "Ed25519",
        });
        let key =
            parse_key("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
        let checked = check(
            &answer,
            Some(&key),
            "acme",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "email:erin@corp.example",
            1_800_000_001,
        )
        .expect("the contract's vector verifies");
        assert_eq!(checked.lease_id, "00112233445566778899aabbccddeeff");
    }

    #[test]
    fn a_changed_or_foreign_lease_does_not_verify() {
        let (pkcs8, key) = keyed();
        let mut stored = sign(&body(END), &pkcs8).expect("signed");
        stored["lease"]["expires_at"] = serde_json::json!(9_999_999_999u64);
        assert_eq!(verify(&stored, &key), Err(Refusal::BadSignature));
        let (other, _) = keyed();
        let foreign = sign(&body(END), &other).expect("signed");
        assert_eq!(verify(&foreign, &key), Err(Refusal::BadSignature));
        assert!(matches!(
            verify(&serde_json::json!({ "lease": 3 }), &key),
            Err(Refusal::Malformed(_))
        ));
    }

    #[test]
    fn the_online_class_has_no_lease() {
        let (_, key) = keyed();
        assert_eq!(
            read_answer(&serde_json::json!({ "class": "online" }), &key),
            Ok(Answer::OnlineOnly)
        );
        assert!(class_rank("online") < class_rank("days-7"));
        assert!(class_rank("days-7") < class_rank("days-30"));
    }

    #[test]
    fn half_life() {
        let lease = lease_of(&body(END)).expect("lease");
        let half = END - WEEK / 2;
        assert!(!lease.past_half_life(half - 1));
        assert!(lease.past_half_life(half));
    }

    /// A lease signed by the hub's own code (org-lease.js `leaseSigningBytes`
    /// and WebCrypto Ed25519, with RFC 8032's first test seed) verifies
    /// here: the two halves agree on the exact signed bytes.
    #[test]
    fn a_lease_the_hub_signed_verifies_here() {
        let answer: Value = serde_json::from_str(
            r#"{"lease":{"v":1,"org":"acme","digest":"abababababababababababababababababababababababababababababababab","principal":"email:dan@acme.example","class":"days-7","issued_at":1760400000,"expires_at":1761004800,"lease_id":"0123456789abcdef0123456789abcdef"},"signature":"45100b79a1deaa00a47c4711c6043bd6e5be80ac3d19cb217994c3b8d1dce7afc2293fd48829932f95d9da3359a3c20e0a572c3e75bbc89e9cd0a6008ddd3506","key_id":"x","alg":"Ed25519"}"#,
        )
        .unwrap();
        let key =
            parse_key("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
        let lease = verify(&answer, &key).expect("the hub's signature holds");
        assert_eq!(lease.principal, "email:dan@acme.example");
        assert_eq!(lease.expires_at, 1_761_004_800);
    }

    #[test]
    fn the_signed_bytes_are_the_hubs_domain_then_canonical_json() {
        let lease = serde_json::json!({ "v": 1, "org": "acme", "class": "days-7" });
        assert_eq!(
            signing_bytes(&lease),
            b"krate-lease-v1\n{\"class\":\"days-7\",\"org\":\"acme\",\"v\":1}".to_vec()
        );
    }

    #[test]
    fn the_signed_bytes_do_not_depend_on_key_order() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":[3,"x"]}}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"c":[3,"x"],"d":2},"b":1}"#);
    }

    /// The pinned key is the one the contract names, and its id is computed
    /// the way the hub computes it.
    #[test]
    fn the_production_key_is_pinned() {
        let pinned = PINNED_LEASE_KEY.and_then(parse_key).expect("a pinned key");
        assert_eq!(
            hex_of(&pinned),
            "51c5f9471f2233f7080059eed2ea48cff14ccb7f65f285bcf8dbf45e9ffb8a4d"
        );
        assert_eq!(key_id(&pinned), PINNED_LEASE_KEY_ID);
        // And the contract's test key, whose id is known to the appendix's
        // own arithmetic: the pin is not the test key.
        let test_key =
            parse_key("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
        assert_ne!(key_id(&test_key), PINNED_LEASE_KEY_ID);
    }

    #[test]
    fn the_override_counts_only_in_a_debug_build() {
        let (_, key) = keyed();
        let hex = hex_of(&key);
        assert_eq!(lease_key_with(true, Some(&hex)), Some(key));
        assert_eq!(
            lease_key_with(false, Some(&hex)),
            PINNED_LEASE_KEY.and_then(parse_key)
        );
    }

    /// The proof that a release build never honours the override: this test
    /// exists only without debug assertions, and runs the real
    /// [`lease_key`] wiring, `cfg!(debug_assertions)` included.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_release_build_ignores_the_override() {
        let (_, key) = keyed();
        let hex = hex_of(&key);
        assert!(!cfg!(debug_assertions));
        assert_eq!(
            lease_key_with(cfg!(debug_assertions), Some(&hex)),
            PINNED_LEASE_KEY.and_then(parse_key),
            "a release build reads the pinned key and nothing else"
        );
        assert_ne!(
            lease_key_with(cfg!(debug_assertions), Some(&hex)),
            Some(key)
        );
        // And through the environment, exactly as the CLI reads it.
        std::env::set_var(LEASE_KEY_OVERRIDE_ENV, &hex);
        assert_eq!(lease_key(), PINNED_LEASE_KEY.and_then(parse_key));
        // So a lease signed by a test key, with the override pointing at
        // that key, does not open anything in a release build.
        let (pkcs8, _) = {
            let pkcs8 = generate_pkcs8().unwrap();
            let k = parse_key(&public_key_hex(&pkcs8).unwrap()).unwrap();
            (pkcs8, k)
        };
        std::env::set_var(LEASE_KEY_OVERRIDE_ENV, public_key_hex(&pkcs8).unwrap());
        let stored = sign(&body(END), &pkcs8).unwrap();
        assert_eq!(
            check(
                &stored,
                lease_key().as_ref(),
                "acme",
                &"ab".repeat(32),
                "github:dan",
                END - 1
            ),
            Err(Refusal::BadSignature)
        );
        std::env::remove_var(LEASE_KEY_OVERRIDE_ENV);
    }
}
