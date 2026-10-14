//! The signing ring, a device's side (IC-958 A; hub side
//! cloud/worker/src/org-signing.js).
//!
//! An owner's or admin's device that holds the ring does, whenever it runs
//! Krate for the organisation: pass the ring to every new owner or admin
//! device the hub says is waiting, sign a permission slip (a delegation)
//! for every developer whose release key is waiting to publish, and sign a
//! withdrawal of every release key whose owner left. Each of those is
//! checked against what this device can check itself (a device id is its
//! key's; a slip is only for the organisation's own namespace), sent to the
//! hub, and written into the organisation's audit there.
//!
//! A developer's device asks to publish ([`Agent::ask_to_publish`]) and
//! reads its answer ([`Agent::publisher`]); it never needs anyone to copy a
//! file or a key.

use serde_json::Value;

use super::{Agent, AgentError};
use crate::signing::SigningRing;
use crate::{device_id, hex32, recovery_keypair, KeyError, RecoveryCode, RecoveryParams};

/// The ring this device holds, and the envelope it came in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRing {
    pub ring: SigningRing,
    pub envelope: String,
}

/// Where this device stands with the signing ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingStatus {
    /// This person's role does not hold the ring (only owners and admins).
    NotAHolder,
    /// The organisation has no ring yet: its first owner device makes it.
    NoRing,
    /// Registered and waiting for a holder device to pass it on.
    Waiting,
    /// The organisation's signing key was replaced by hand (`krate org
    /// root-key`), so the ring no longer signs for it.
    Replaced,
    /// This device holds the current version and did its duties.
    Holds(HeldRing),
}

/// What one round of duties did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Duties {
    /// Owner or admin devices that were given the ring.
    pub ring_grants: usize,
    /// Whose release keys were approved to publish, by principal.
    pub approved: Vec<String>,
    /// Release keys withdrawn.
    pub withdrawn: usize,
}

/// A developer's standing to publish, from the hub's record of their
/// release key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publisher {
    /// `waiting`, `approved` (by hand, not signed yet), `signed`,
    /// `revoked` or `closed`.
    pub state: String,
    /// The signed permission slip, as JSON text, once `signed`.
    pub delegation: Option<String>,
    /// Who can approve it: the owners and admins whose devices hold the
    /// ring, as people read them.
    pub can_sign: Vec<String>,
    /// The organisation approves new publishers by hand.
    pub by_hand: bool,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    crate::hex(bytes)
}

fn signing_key(ring: &SigningRing) -> Result<krate_bundle::signing::SigningKey, AgentError> {
    krate_bundle::signing::SigningKey::from_pkcs8(ring.pkcs8())
        .map_err(|_| AgentError::Key(KeyError::Malformed("signing key")))
}

fn publisher_of(value: &Value) -> Result<Publisher, AgentError> {
    let state = value["state"]
        .as_str()
        .ok_or_else(|| AgentError::Hub("the hub's publisher record has no state".into()))?
        .to_string();
    Ok(Publisher {
        delegation: value
            .get("delegation")
            .filter(|d| d.is_object())
            .map(|d| d.to_string()),
        can_sign: value["can_sign"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect(),
        by_hand: value["by_hand"].as_bool().unwrap_or(false),
        state,
    })
}

impl Agent {
    /// The hub's view of the signing ring, for this device.
    pub fn signing_view(&self) -> Result<Value, AgentError> {
        self.call_org(
            "GET",
            &format!("/signing?device={}", self.device.id()),
            None,
        )
    }

    /// Make the organisation's signing key on this device (an owner's),
    /// register its public half as the organisation's, and seal it for this
    /// device and for the recovery code. Nothing is shown: there is nothing
    /// for a person to keep.
    pub fn signing_init(&self) -> Result<HeldRing, AgentError> {
        self.registered_view()?;
        let view = self.signing_view()?;
        let pkcs8 = krate_bundle::signing::SigningKey::generate_pkcs8()
            .map_err(|_| AgentError::Key(KeyError::Random))?;
        let ring = SigningRing::new(&self.slug, 1, pkcs8);
        let public = hex(&signing_key(&ring)?.public_key());
        let envelope = ring.seal_to(&self.device.public())?;
        let recovery_blob = match view["recovery"]["public_key"].as_str() {
            Some(key) => Some(ring.seal_to(&hex32(key)?)?),
            None => None,
        };
        self.call_org(
            "POST",
            "/signing/init",
            Some(&serde_json::json!({
                "device_id": self.device.id(), "version": 1, "public_key": public,
                "envelope": envelope, "recovery_blob": recovery_blob,
            })),
        )?;
        Ok(HeldRing { ring, envelope })
    }

    /// Bring this device's hold of the ring up to date and do a holder's
    /// duties. Safe to call as often as Krate likes: with nothing waiting it
    /// is two reads.
    pub fn signing_refresh(&self) -> Result<(RingStatus, Duties), AgentError> {
        self.registered_view()?;
        let view = self.signing_view()?;
        let mut duties = Duties::default();
        if view["may_hold"].as_bool() != Some(true) {
            return Ok((RingStatus::NotAHolder, duties));
        }
        let Some(public) = view["ring"]["public_key"].as_str() else {
            return Ok((RingStatus::NoRing, duties));
        };
        let Some(blob) = view["envelope"]["blob"].as_str() else {
            return Ok((RingStatus::Waiting, duties));
        };
        let ring = SigningRing::open(blob, &self.device, &self.slug)?;
        let key = signing_key(&ring)?;
        // The hub's public key for the ring is the one this device holds:
        // a hub cannot swap in a key of its own.
        if hex(&key.public_key()) != public
            || view["ring"]["version"].as_u64() != Some(u64::from(ring.version()))
        {
            return Err(AgentError::Key(KeyError::Malformed(
                "signing ring: the hub's differs from the one this device holds",
            )));
        }
        let held = HeldRing {
            ring,
            envelope: blob.to_string(),
        };
        if view["ring"]["current"].as_bool() != Some(true) {
            return Ok((RingStatus::Replaced, duties));
        }
        for device in view["ring_requests"].as_array().into_iter().flatten() {
            if self.grant_ring(&held.ring, device).is_ok() {
                duties.ring_grants += 1;
            }
        }
        let at = now();
        for request in view["publisher_requests"].as_array().into_iter().flatten() {
            if let Some(who) = self.approve_publisher(&key, request, at)? {
                duties.approved.push(who);
            }
        }
        let withdraw: Vec<krate_bundle::delegation::Revocation> = view["revoke"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let k = r["key"].as_str()?;
                (k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit())).then(|| {
                    krate_bundle::delegation::Revocation {
                        key: k.to_ascii_lowercase(),
                        compromised_from: r["since"].as_u64().unwrap_or(at),
                        reason: r["reason"]
                            .as_str()
                            .unwrap_or("left the organisation")
                            .into(),
                    }
                })
            })
            .collect();
        if !withdraw.is_empty() {
            let count = withdraw.len();
            let list = krate_bundle::delegation::SignedRevocationList::create(
                &key,
                krate_bundle::delegation::RevocationList {
                    schema: String::new(),
                    root: String::new(),
                    issued_at: at,
                    revocations: withdraw,
                },
            );
            self.call_org(
                "POST",
                "/signing/revocations",
                Some(&serde_json::json!({ "list": list, "by_device": self.device.id() })),
            )?;
            duties.withdrawn = count;
        }
        Ok((RingStatus::Holds(held), duties))
    }

    fn grant_ring(&self, ring: &SigningRing, device: &Value) -> Result<(), AgentError> {
        let id = device["device_id"].as_str().unwrap_or("");
        let public = hex32(device["public_key"].as_str().unwrap_or(""))?;
        // The id is the key's: the hub cannot name one device and hand over
        // another's public key.
        if device_id(&public) != id {
            return Err(AgentError::Key(KeyError::Malformed("device id")));
        }
        let blob = ring.seal_to(&public)?;
        self.call_org(
            "POST",
            "/signing/grants",
            Some(&serde_json::json!({
                "device_id": id, "version": ring.version(), "blob": blob, "by_device": self.device.id(),
            })),
        )
        .map(|_| ())
    }

    /// Sign a slip for one waiting release key: the organisation's whole
    /// namespace, the standard 365 days. Answers whose it was. A refusal of
    /// one request (its person was removed a moment ago) does not stop the
    /// others; being offline does.
    fn approve_publisher(
        &self,
        root: &krate_bundle::signing::SigningKey,
        request: &Value,
        at: u64,
    ) -> Result<Option<String>, AgentError> {
        let Some(key) = request["key"]
            .as_str()
            .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return Ok(None);
        };
        let key = key.to_ascii_lowercase();
        let slip = krate_bundle::delegation::org_delegation(root, &self.slug, &key, at);
        match self.call_org(
            "POST",
            &format!("/publishers/{key}/delegation"),
            Some(&serde_json::json!({ "delegation": slip, "by_device": self.device.id() })),
        ) {
            Ok(_) => Ok(Some(
                request["principal"].as_str().unwrap_or("").to_string(),
            )),
            Err(AgentError::Refused { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Restore the ring on this device from the recovery code (an owner's
    /// or admin's new device), the way [`Agent::recover`] restores the
    /// keyring.
    pub fn signing_recover(&self, code: &RecoveryCode) -> Result<HeldRing, AgentError> {
        self.registered_view()?;
        let stored = self.call("GET", "/recovery", None)?;
        let params = RecoveryParams::from_json(&stored["params"])?;
        let recovery = recovery_keypair(code, &params)?;
        if Some(recovery.public_hex().as_str()) != stored["public_key"].as_str() {
            return Err(AgentError::Key(KeyError::Open));
        }
        let sealed = self.call_org("GET", "/signing/recovery", None)?;
        let ring = SigningRing::open(sealed["blob"].as_str().unwrap_or(""), &recovery, &self.slug)?;
        let envelope = ring.seal_to(&self.device.public())?;
        self.call_org(
            "POST",
            "/signing/grants",
            Some(&serde_json::json!({
                "device_id": self.device.id(), "version": ring.version(), "blob": envelope, "by_device": "recovery",
            })),
        )?;
        Ok(HeldRing { ring, envelope })
    }

    /// Ask the organisation to let this release key publish. The same key
    /// again answers the record it already has.
    pub fn ask_to_publish(&self, key: &str, name: &str) -> Result<Publisher, AgentError> {
        let answer = self.call_org(
            "POST",
            "/publishers",
            Some(&serde_json::json!({ "public_key": key, "name": name })),
        )?;
        publisher_of(&answer)
    }

    /// The hub's record of this release key.
    pub fn publisher(&self, key: &str) -> Result<Publisher, AgentError> {
        let answer = self.call_org("GET", &format!("/publishers/{key}"), None)?;
        publisher_of(&answer)
    }
}
