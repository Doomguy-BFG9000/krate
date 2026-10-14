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

    // Every candidate fixture carries acme's mark (Addendum 5) unless its
    // name says otherwise; the mark goes in before the signature, which
    // covers it.
    let make_marked = |name: &str,
                       signer: &SigningKey,
                       version: &str,
                       slip: Option<SignedDelegation>,
                       mark: Option<&str>| {
        let path = out.join(name);
        fs::copy(&base, &path).unwrap();
        if let Some(org) = mark {
            krate_bundle::add_org_mark(&path, org).unwrap();
        }
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
    let make = |name: &str, signer: &SigningKey, version: &str, slip: Option<SignedDelegation>| {
        make_marked(name, signer, version, slip, Some("acme"))
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
    // Validly signed for acme, but with no mark at all, and with another
    // organisation's mark (Addendum 5).
    make_marked("unmarked.krate", &release, "1.0.3", Some(good()), None);
    make_marked(
        "marked-globex.krate",
        &release,
        "1.0.4",
        Some(good()),
        Some("globex"),
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
    let mark = |name: &str| -> serde_json::Value {
        let bytes = fs::read(dir.join(name)).unwrap();
        let answer: serde_json::Value =
            serde_json::from_str(&krate_validator_wasm::judge_to_json(&bytes)).unwrap();
        answer["judgement"]["org_mark"].clone()
    };
    assert_eq!(mark("candidate-v1.krate"), "acme");
    assert_eq!(mark("revoked.krate"), "acme");
    assert!(mark("unmarked.krate").is_null());
    assert_eq!(mark("marked-globex.krate"), "globex");
    assert_eq!(judge("unmarked.krate")["state"], "valid");
    assert_eq!(judge("marked-globex.krate")["state"], "valid");
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

/// The bundles the hub's company-connection tests run against (IC-952):
/// one app that asks for `net.connection:finance-api`, a later version of
/// it that does not, and another app that asks too, each marked for acme
/// and signed by a release key acme's own (fresh) root delegated.
///
///   KRATE_WRITE_ORG_FIXTURES=1 cargo test -p krate-validator-wasm \
///     --test org_fixtures -- --ignored
#[test]
#[ignore = "writes cloud/worker/test/fixtures/org-connections; run by hand with KRATE_WRITE_ORG_FIXTURES=1"]
fn write_connection_fixtures() {
    if std::env::var("KRATE_WRITE_ORG_FIXTURES").as_deref() != Ok("1") {
        eprintln!("set KRATE_WRITE_ORG_FIXTURES=1 to write the fixtures");
        return;
    }
    let out = repo().join("cloud/worker/test/fixtures/org-connections");
    fs::create_dir_all(&out).unwrap();
    let component = repo().join("crates/bundle/tests/fixtures/minimal-run.wasm");
    let root = key();
    let release = key();
    let work = std::env::temp_dir().join(format!("krate-conn-fixtures-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    let make = |name: &str, id: &str, version: &str, asks: bool| {
        let mut manifest = format!(
            "[app]\nid = \"{id}\"\nname = \"Ledger\"\nversion = \"{version}\"\n\
             entry = \"code.wasm\"\nworld = \"krate:app/cli@0.1.0\"\n"
        );
        if asks {
            manifest.push_str(
                "\n[[capabilities]]\ncap = \"net.connection:finance-api\"\n\
                 rationale = \"read invoices\"\nrequired = false\n",
            );
        }
        let manifest_path = work.join(format!("{name}.toml"));
        fs::write(&manifest_path, manifest).unwrap();
        let path = out.join(name);
        krate_bundle::pack(&manifest_path, &component, &path).unwrap();
        krate_bundle::add_org_mark(&path, "acme").unwrap();
        krate_bundle::sign_bundle_delegated(
            &path,
            &release,
            "acme/ledger",
            version,
            SIGNED_AT,
            slip(&root, &root, &release, SLIP_FROM, SLIP_UNTIL),
        )
        .unwrap();
    };
    make("ledger.krate", "com.acme.ledger", "1.0.0", true);
    make("ledger-plain.krate", "com.acme.ledger", "1.0.1", false);
    make("other.krate", "com.acme.other", "1.0.0", true);
    let _ = fs::remove_dir_all(&work);
    fs::write(
        out.join("keys.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "root": hex(&root.public_key()),
            "release": hex(&release.public_key()),
        }))
        .unwrap(),
    )
    .unwrap();
}

/// The committed connection fixtures say what their names say.
#[test]
fn the_connection_fixtures_say_what_their_names_say() {
    let dir = repo().join("cloud/worker/test/fixtures/org-connections");
    let keys: serde_json::Value = serde_json::from_slice(
        &fs::read(dir.join("keys.json"))
            .expect("the committed fixtures; write them with KRATE_WRITE_ORG_FIXTURES=1"),
    )
    .unwrap();
    for (name, id, asks) in [
        ("ledger.krate", "com.acme.ledger", true),
        ("ledger-plain.krate", "com.acme.ledger", false),
        ("other.krate", "com.acme.other", true),
    ] {
        let answer: serde_json::Value = serde_json::from_str(&krate_validator_wasm::judge_to_json(
            &fs::read(dir.join(name)).unwrap(),
        ))
        .unwrap();
        assert_eq!(answer["ok"], true, "{name}: {answer}");
        let j = &answer["judgement"];
        assert_eq!(j["org_mark"], "acme", "{name}");
        assert_eq!(j["manifest"]["app"]["id"], id, "{name}");
        assert_eq!(j["signature"]["state"], "valid", "{name}");
        assert_eq!(j["signature"]["chain"]["root"], keys["root"], "{name}");
        let caps: Vec<String> = j["manifest"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cap"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            caps.contains(&"net.connection:finance-api".to_string()),
            asks,
            "{name}: {caps:?}"
        );
    }
}

/// What each inventory fixture carries: file, app id, version, its build
/// record (provider, model, kept request, revisions), whether it asks for
/// the finance-api connection.
type Built = Option<(&'static str, &'static str, Option<&'static str>, u32)>;
type InventoryFixture = (&'static str, &'static str, &'static str, Built, bool);
const INVENTORY: [InventoryFixture; 3] = [
    (
        "expenses-1.krate",
        "com.acme.expenses",
        "1.0.0",
        Some((
            "claude",
            "claude-opus-5",
            Some("an expenses tracker for finance"),
            0,
        )),
        true,
    ),
    (
        "expenses-2.krate",
        "com.acme.expenses",
        "1.0.1",
        Some(("claude", "claude-opus-5", None, 1)),
        true,
    ),
    ("rota.krate", "com.acme.rota", "1.0.0", None, false),
];

/// The bundles the hub's inventory tests run against (IC-954): two
/// versions of an app made with an AI, the first keeping its request, and
/// one made by hand; each marked for acme and signed by a release key a
/// fresh acme root delegated.
///
///   KRATE_WRITE_ORG_FIXTURES=1 cargo test -p krate-validator-wasm \
///     --test org_fixtures -- --ignored
#[test]
#[ignore = "writes cloud/worker/test/fixtures/org-inventory; run by hand with KRATE_WRITE_ORG_FIXTURES=1"]
fn write_inventory_fixtures() {
    if std::env::var("KRATE_WRITE_ORG_FIXTURES").as_deref() != Ok("1") {
        eprintln!("set KRATE_WRITE_ORG_FIXTURES=1 to write the fixtures");
        return;
    }
    let out = repo().join("cloud/worker/test/fixtures/org-inventory");
    fs::create_dir_all(&out).unwrap();
    let component = repo().join("crates/bundle/tests/fixtures/minimal-run.wasm");
    let root = key();
    let release = key();
    let work = std::env::temp_dir().join(format!("krate-inv-fixtures-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    for (name, id, version, built, asks) in INVENTORY {
        let label = if id.ends_with("rota") {
            "Rota"
        } else {
            "Expenses"
        };
        let mut manifest = format!(
            "[app]\nid = \"{id}\"\nname = \"{label}\"\nversion = \"{version}\"\n\
             entry = \"code.wasm\"\nworld = \"krate:app/cli@0.1.0\"\n"
        );
        if asks {
            manifest.push_str(
                "\n[[capabilities]]\ncap = \"net.connection:finance-api\"\n\
                 rationale = \"read invoices\"\nrequired = false\n",
            );
        }
        let manifest_path = work.join(format!("{name}.toml"));
        fs::write(&manifest_path, manifest).unwrap();
        let path = out.join(name);
        krate_bundle::pack(&manifest_path, &component, &path).unwrap();
        if let Some((provider, model, prompt, revisions)) = built {
            krate_bundle::add_build_provenance(
                &path,
                &krate_bundle::BuildProvenance {
                    v: 1,
                    tool: "krate studio".into(),
                    krate_version: "0.5.5".into(),
                    provider: Some(provider.into()),
                    model: Some(model.into()),
                    started_at: SIGNED_AT - 600,
                    finished_at: SIGNED_AT - 300,
                    prompt_sha256: krate_bundle::sha256_hex(b"an expenses tracker for finance"),
                    prompt: prompt.map(str::to_string),
                    revisions,
                },
            )
            .unwrap();
        }
        krate_bundle::add_org_mark(&path, "acme").unwrap();
        krate_bundle::sign_bundle_delegated(
            &path,
            &release,
            &format!("acme/{}", id.rsplit('.').next().unwrap()),
            version,
            SIGNED_AT,
            slip(&root, &root, &release, SLIP_FROM, SLIP_UNTIL),
        )
        .unwrap();
    }
    let _ = fs::remove_dir_all(&work);
    fs::write(
        out.join("keys.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "root": hex(&root.public_key()),
            "release": hex(&release.public_key()),
        }))
        .unwrap(),
    )
    .unwrap();
}

/// The committed inventory fixtures say what their names say, and the
/// validator puts each build record in its judgement.
#[test]
fn the_inventory_fixtures_say_what_their_names_say() {
    let dir = repo().join("cloud/worker/test/fixtures/org-inventory");
    let keys: serde_json::Value = serde_json::from_slice(
        &fs::read(dir.join("keys.json"))
            .expect("the committed fixtures; write them with KRATE_WRITE_ORG_FIXTURES=1"),
    )
    .unwrap();
    for (name, id, version, built, _) in INVENTORY {
        let answer: serde_json::Value = serde_json::from_str(&krate_validator_wasm::judge_to_json(
            &fs::read(dir.join(name)).unwrap(),
        ))
        .unwrap();
        assert_eq!(answer["ok"], true, "{name}: {answer}");
        let j = &answer["judgement"];
        assert_eq!(j["org_mark"], "acme", "{name}");
        assert_eq!(j["manifest"]["app"]["id"], id, "{name}");
        assert_eq!(j["manifest"]["app"]["version"], version, "{name}");
        assert_eq!(j["signature"]["state"], "valid", "{name}");
        assert_eq!(j["signature"]["chain"]["root"], keys["root"], "{name}");
        match built {
            None => assert!(j["provenance"].is_null(), "{name}: made by hand"),
            Some((provider, model, prompt, revisions)) => {
                let p = &j["provenance"];
                assert_eq!(p["tool"], "krate studio", "{name}");
                assert_eq!(p["provider"], provider, "{name}");
                assert_eq!(p["model"], model, "{name}");
                assert_eq!(p["prompt"].as_str(), prompt, "{name}");
                assert_eq!(p["revisions"], revisions, "{name}");
            }
        }
    }
}
