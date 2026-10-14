//! The signed bundles the hub's organisation tests run against (IC-946).
//!
//! The hub accepts an organisation candidate only when it is signed by the
//! organisation's root or by a release key that root delegated, unexpired
//! and unrevoked. Testing that needs real signatures made by the crate that
//! verifies them, not a JavaScript copy of the statement format, so the
//! files are made here, with `krate_bundle`'s own signing, and committed:
//!
//!   KRATE_WRITE_ORG_FIXTURES=1 cargo test -p krate-validator-wasm \
//!     --test org_fixtures -- --ignored
//!
//! The keys are fresh on every run and only their public halves are kept.
//! Every time is fixed, so the expired slip is expired whenever it is read
//! and the good slip stays good until 2100.

use std::fs;
use std::path::{Path, PathBuf};

use krate_bundle::delegation::{
    Delegation, Purpose, Revocation, RevocationList, SignedDelegation, SignedRevocationList,
    DELEGATION_SCHEMA,
};
use krate_bundle::signing::SigningKey;

const SIGNED_AT: u64 = 1_760_000_000;
const SLIP_FROM: u64 = 1_700_000_000;
/// 2100-01-01.
const SLIP_UNTIL: u64 = 4_102_444_800;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn key() -> SigningKey {
    SigningKey::from_pkcs8(&SigningKey::generate_pkcs8().expect("generate")).expect("load")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn slip(
    signer: &SigningKey,
    names_root: &SigningKey,
    release: &SigningKey,
    from: u64,
    until: u64,
) -> SignedDelegation {
    SignedDelegation::create(
        signer,
        Delegation {
            schema: DELEGATION_SCHEMA.to_string(),
            root: hex(&names_root.public_key()),
            namespace: "acme/*".to_string(),
            key: hex(&release.public_key()),
            purpose: Purpose::Release,
            not_before: from,
            expires: until,
            approvers: Vec::new(),
        },
    )
}

#[test]
#[ignore = "writes cloud/worker/test/fixtures/org; run by hand with KRATE_WRITE_ORG_FIXTURES=1"]
fn write_org_fixtures() {
    if std::env::var("KRATE_WRITE_ORG_FIXTURES").as_deref() != Ok("1") {
        eprintln!("set KRATE_WRITE_ORG_FIXTURES=1 to write the fixtures");
        return;
    }
    let out = repo().join("cloud/worker/test/fixtures/org");
    fs::create_dir_all(&out).unwrap();
    let base = repo().join("evidence/ported/bounce.krate");

    let root = key();
    let other_root = key();
    let release = key();
    let stolen = key();
    let foreign = key();

    let make = |name: &str, signer: &SigningKey, version: &str, slip: Option<SignedDelegation>| {
        let path = out.join(name);
        fs::copy(&base, &path).unwrap();
        match slip {
            Some(slip) => {
                krate_bundle::sign_bundle_delegated(
                    &path,
                    signer,
                    "acme/bounce",
                    version,
                    SIGNED_AT,
                    slip,
                )
                .unwrap();
            }
            None => {
                krate_bundle::sign_bundle(&path, signer, "acme/bounce", version, SIGNED_AT)
                    .unwrap();
            }
        }
    };

    let good = || slip(&root, &root, &release, SLIP_FROM, SLIP_UNTIL);
    // What the pilot's developer publishes, and the same app with changed
    // bytes after approval.
    make("candidate-v1.krate", &release, "1.0.0", Some(good()));
    make("candidate-v2.krate", &release, "1.0.1", Some(good()));
    // The root signing for itself.
    make("root-signed.krate", &root, "1.0.2", None);
    // A real slip from another publisher's root.
    make(
        "wrong-root.krate",
        &foreign,
        "1.0.0",
        Some(slip(
            &other_root,
            &other_root,
            &foreign,
            SLIP_FROM,
            SLIP_UNTIL,
        )),
    );
    // A slip that NAMES acme's root but that acme's root never signed.
    make(
        "forged-slip.krate",
        &foreign,
        "1.0.0",
        Some(slip(&other_root, &root, &foreign, SLIP_FROM, SLIP_UNTIL)),
    );
    // A real slip whose window closed before the signing time.
    make(
        "expired.krate",
        &release,
        "1.0.0",
        Some(slip(&root, &root, &release, SLIP_FROM, SIGNED_AT - 1)),
    );
    // A key the root delegated and later withdrew.
    make(
        "revoked.krate",
        &stolen,
        "1.0.0",
        Some(slip(&root, &root, &stolen, SLIP_FROM, SLIP_UNTIL)),
    );

    let list = SignedRevocationList::create(
        &root,
        RevocationList {
            schema: String::new(),
            root: String::new(),
            issued_at: SIGNED_AT,
            revocations: vec![Revocation {
                key: hex(&stolen.public_key()),
                compromised_from: SLIP_FROM,
                reason: "laptop lost".to_string(),
            }],
        },
    );
    fs::write(
        out.join("revocations.json"),
        serde_json::to_vec_pretty(&list).unwrap(),
    )
    .unwrap();
    let keys = serde_json::json!({
        "root": hex(&root.public_key()),
        "other_root": hex(&other_root.public_key()),
        "release": hex(&release.public_key()),
        "revoked_release": hex(&stolen.public_key()),
        "foreign_release": hex(&foreign.public_key()),
        "signed_at": SIGNED_AT,
        "slip_until": SLIP_UNTIL,
    });
    fs::write(
        out.join("keys.json"),
        serde_json::to_vec_pretty(&keys).unwrap(),
    )
    .unwrap();
}

/// The committed fixtures say what their names say, read back through the
/// validator the hub runs.
#[test]
fn the_org_fixtures_say_what_their_names_say() {
    let dir = repo().join("cloud/worker/test/fixtures/org");
    let keys = fs::read(dir.join("keys.json"))
        .expect("the committed fixtures; write them with KRATE_WRITE_ORG_FIXTURES=1");
    let keys: serde_json::Value = serde_json::from_slice(&keys).unwrap();
    let judge = |name: &str| -> serde_json::Value {
        let bytes = fs::read(dir.join(name)).unwrap();
        let answer: serde_json::Value =
            serde_json::from_str(&krate_validator_wasm::judge_to_json(&bytes)).unwrap();
        assert_eq!(answer["ok"], true, "{name}: {answer}");
        answer["judgement"]["signature"].clone()
    };
    let v1 = judge("candidate-v1.krate");
    assert_eq!(v1["state"], "valid");
    assert_eq!(v1["chain"]["state"], "revocation-unchecked");
    assert_eq!(v1["chain"]["root"], keys["root"]);
    assert_eq!(judge("candidate-v2.krate")["chain"]["root"], keys["root"]);
    let direct = judge("root-signed.krate");
    assert_eq!(direct["public_key"], keys["root"]);
    assert!(direct.get("chain").is_none());
    assert_eq!(
        judge("wrong-root.krate")["chain"]["root"],
        keys["other_root"]
    );
    let forged = judge("forged-slip.krate");
    assert_eq!(forged["chain"]["state"], "forged");
    assert!(forged["chain"]["root"].is_null());
    assert_eq!(judge("expired.krate")["chain"]["state"], "expired");
    let revoked = judge("revoked.krate");
    assert_eq!(revoked["chain"]["state"], "revocation-unchecked");
    assert_eq!(revoked["public_key"], keys["revoked_release"]);

    let list: serde_json::Value = serde_json::from_str(&krate_validator_wasm::revocations_to_json(
        &fs::read(dir.join("revocations.json")).unwrap(),
    ))
    .unwrap();
    assert_eq!(list["ok"], true, "{list}");
    assert_eq!(list["root"], keys["root"]);
    assert_eq!(list["revocations"][0]["key"], keys["revoked_release"]);
}

/// A list changed after its root signed it, and bytes that are not a list,
/// are refused with a reason.
#[test]
fn a_revocation_list_nobody_signed_is_refused() {
    let root = key();
    let list = SignedRevocationList::create(
        &root,
        RevocationList {
            schema: String::new(),
            root: String::new(),
            issued_at: 1,
            revocations: vec![Revocation {
                key: "ab".repeat(32),
                compromised_from: 1,
                reason: "lost".to_string(),
            }],
        },
    );
    let good: serde_json::Value = serde_json::from_str(&krate_validator_wasm::revocations_to_json(
        &serde_json::to_vec(&list).unwrap(),
    ))
    .unwrap();
    assert_eq!(good["ok"], true, "{good}");
    assert_eq!(good["root"], hex(&root.public_key()));

    let mut edited = list.clone();
    edited.list.revocations[0].key = "cd".repeat(32);
    let bad: serde_json::Value = serde_json::from_str(&krate_validator_wasm::revocations_to_json(
        &serde_json::to_vec(&edited).unwrap(),
    ))
    .unwrap();
    assert_eq!(bad["ok"], false);
    assert!(bad["problem"]
        .as_str()
        .unwrap()
        .contains("altered or forged"));

    let junk: serde_json::Value =
        serde_json::from_str(&krate_validator_wasm::revocations_to_json(b"not json")).unwrap();
    assert_eq!(junk["ok"], false);
}
