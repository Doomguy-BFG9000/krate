//! The signing ring (IC-958 A): the organisation's signing key, held only
//! by its owners' and admins' devices, so that every app is signed by the
//! organisation and no person ever handles a key.
//!
//! The key is an Ed25519 key pair (crates/bundle's `SigningKey`, PKCS#8).
//! Its public half is the organisation's root on the hub; its private half
//! travels exactly as the keyring does: sealed whole to each holder
//! device's X25519 public key with HPKE (RFC 9180, the suite in
//! [`crate::HPKE_SUITE`]), and to the recovery key the recovery code
//! derives, so the hub only ever stores envelopes. The ring is versioned:
//! a new root is a new version, and an envelope says which version it is.
//!
//! Separate from the data keyring on purpose: a recipient's device holds
//! the keyring (it reads the team's data) and must never hold the ring (it
//! would sign apps as the organisation).

use zeroize::Zeroize;

use crate::{b64_decode, b64_encode, hpke_open, hpke_seal, KeyError, KeyPair};

const RING_INFO: &[u8] = b"krate org signing ring v1\0";

/// One version of an organisation's signing key.
#[derive(Clone, PartialEq, Eq)]
pub struct SigningRing {
    org: String,
    version: u32,
    pkcs8: Vec<u8>,
}

impl Drop for SigningRing {
    fn drop(&mut self) {
        self.pkcs8.zeroize();
    }
}

impl std::fmt::Debug for SigningRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SigningRing({} v{})", self.org, self.version)
    }
}

fn ring_info(org: &str) -> Vec<u8> {
    let mut info = RING_INFO.to_vec();
    info.extend_from_slice(org.as_bytes());
    info
}

impl SigningRing {
    /// `pkcs8` is the private key, as crates/bundle's
    /// `SigningKey::generate_pkcs8` (or WebCrypto's PKCS#8 export) writes it.
    pub fn new(org: &str, version: u32, pkcs8: Vec<u8>) -> SigningRing {
        SigningRing {
            org: org.to_string(),
            version,
            pkcs8,
        }
    }

    pub fn org(&self) -> &str {
        &self.org
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// The private key, PKCS#8. Used on this device to sign, never sent.
    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    /// Seal the ring to one device's public key (or the recovery key's):
    /// the envelope the hub keeps for that device, as base64.
    pub fn seal_to(&self, recipient: &[u8; 32]) -> Result<String, KeyError> {
        let mut plain = serde_json::json!({
            "org": self.org, "v": self.version, "pkcs8": b64_encode(&self.pkcs8),
        })
        .to_string()
        .into_bytes();
        let sealed = hpke_seal(recipient, &ring_info(&self.org), recipient, &plain);
        plain.zeroize();
        Ok(b64_encode(&sealed?))
    }

    /// Open an envelope sealed to `recipient` for organisation `org`.
    pub fn open(envelope: &str, recipient: &KeyPair, org: &str) -> Result<SigningRing, KeyError> {
        let sealed = b64_decode(envelope).ok_or(KeyError::Malformed("envelope"))?;
        let mut plain = hpke_open(recipient, &ring_info(org), &recipient.public(), &sealed)?;
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&plain);
        plain.zeroize();
        let bad = KeyError::Malformed("signing ring");
        let value = parsed.map_err(|_| bad.clone())?;
        if value["org"].as_str() != Some(org) {
            return Err(KeyError::Open);
        }
        let version = value["v"]
            .as_u64()
            .filter(|v| *v >= 1 && *v <= u64::from(u32::MAX))
            .ok_or(bad.clone())? as u32;
        let pkcs8 = value["pkcs8"]
            .as_str()
            .and_then(b64_decode)
            .filter(|k| !k.is_empty() && k.len() <= 512)
            .ok_or(bad)?;
        Ok(SigningRing::new(org, version, pkcs8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring opens only with the device it was sealed to and only for
    /// its organisation; a changed byte does not open at all.
    #[test]
    fn the_ring_opens_only_for_its_device_and_its_organisation() {
        let ring = SigningRing::new("acme", 1, vec![7u8; 83]);
        let (mine, theirs) = (KeyPair::generate(), KeyPair::generate());
        let envelope = ring.seal_to(&mine.public()).unwrap();
        let opened = SigningRing::open(&envelope, &mine, "acme").unwrap();
        assert_eq!(opened, ring);
        assert_eq!((opened.version(), opened.pkcs8().len()), (1, 83));
        assert_eq!(
            SigningRing::open(&envelope, &theirs, "acme"),
            Err(KeyError::Open)
        );
        assert_eq!(
            SigningRing::open(&envelope, &mine, "globex"),
            Err(KeyError::Open),
            "the organisation is in what was sealed"
        );
        let mut bytes = b64_decode(&envelope).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            SigningRing::open(&b64_encode(&bytes), &mine, "acme"),
            Err(KeyError::Open)
        );
        // A keyring envelope is not a ring envelope: the suites' infos differ.
        let keyring = crate::Keyring::create("acme");
        let other = keyring.seal_to(&mine.public()).unwrap();
        assert!(SigningRing::open(&other, &mine, "acme").is_err());
        assert!(format!("{ring:?}").contains("v1") && !format!("{ring:?}").contains("7, 7"));
    }
}
