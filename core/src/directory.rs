//! Signed relay directory (§3.1 / TODO #17 tail): rotate the relay set **without an app update**.
//!
//! The operator holds an Ed25519 signing key; its public key is baked into the app
//! ([`DIRECTORY_PUBKEY`]). A signed relay list is served by relays ([`Request::GetDirectory`]);
//! the app fetches it on each relay poll, verifies the signature against the baked-in key, and
//! merges the relays into the set it drains and pairs over. Because verification is against a key
//! **only the operator holds**, a malicious relay cannot inject relays — and losing any single
//! relay's onion key no longer strands users: publish a new signed list (with the new onion) from
//! any live relay and every app picks it up.
//!
//! The signed artifact carries the exact payload bytes that were signed, so signing and
//! verification operate on identical bytes (no JSON-canonicalization ambiguity). A monotonic
//! `version` lets the app ignore stale/rolled-back lists.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The operator's Ed25519 directory-signing **public** key, baked into the app. The app trusts
/// only relay lists signed by the matching private key.
///
/// This is the **Night Drop deployment** key; the operator holds the matching private key offline
/// (`relay-state/directory-signing-key`, gitignored) and uses it to `nightdrop-relay sign-directory`
/// the relay list. To run your **own** deployment, `nightdrop-relay gen-directory-key` mints a fresh
/// pair — paste its public key here and rebuild the app. An all-zero key means "no directory
/// configured" and verifies nothing (the feature stays inert until a real key is baked in).
pub const DIRECTORY_PUBKEY: [u8; 32] = [
    58, 150, 15, 87, 0, 138, 17, 48, 22, 62, 118, 173, 244, 82, 23, 184, 147, 31, 160, 136, 101,
    61, 145, 225, 209, 136, 253, 4, 131, 143, 195, 240,
];

/// The signed payload: the current shared relay set + a monotonic version and issue time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RelayDirectory {
    /// Monotonic; the app ignores a list whose version isn't newer than what it already trusts.
    pub version: u64,
    /// Unix seconds when signed (informational / freshness).
    pub issued_at: u64,
    /// The current shared relay `.onion` set that the operator publishes.
    pub relays: Vec<String>,
}

/// The wire artifact: the base64 payload bytes that were signed, plus the base64 signature.
#[derive(Serialize, Deserialize, Clone)]
pub struct SignedDirectory {
    pub payload: String,
    pub sig: String,
}

impl SignedDirectory {
    /// One-line JSON, as served by the relay and fetched by the app.
    pub fn to_wire(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    /// Verify against `pubkey`; returns the directory only if the signature checks out. An all-zero
    /// key (the "unconfigured" default) never verifies, so the feature is safely inert until a real
    /// operator key is baked in.
    pub fn verify(&self, pubkey: &[u8; 32]) -> Option<RelayDirectory> {
        if pubkey == &[0u8; 32] {
            return None;
        }
        let payload = B64.decode(self.payload.as_bytes()).ok()?;
        let sig_bytes: [u8; 64] = B64.decode(self.sig.as_bytes()).ok()?.try_into().ok()?;
        let vk = VerifyingKey::from_bytes(pubkey).ok()?;
        vk.verify(&payload, &Signature::from_bytes(&sig_bytes))
            .ok()?;
        serde_json::from_slice(&payload).ok()
    }
}

/// Sign a relay list (operator tooling — the `nightdrop-relay sign-directory` subcommand).
pub fn sign(dir: &RelayDirectory, signing_key: &ed25519_dalek::SigningKey) -> SignedDirectory {
    use ed25519_dalek::Signer as _;
    let payload = serde_json::to_vec(dir).unwrap_or_default();
    let sig = signing_key.sign(&payload);
    SignedDirectory {
        payload: B64.encode(&payload),
        sig: B64.encode(sig.to_bytes()),
    }
}

/// Operator tooling: generate a fresh Ed25519 directory-signing keypair. Returns the base64
/// **private** key (keep this secret, use it to sign lists) and the 32-byte **public** key (paste
/// into [`DIRECTORY_PUBKEY`] and rebuild the app).
pub fn generate_signing_key() -> (String, [u8; 32]) {
    let sk = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    (B64.encode(sk.to_bytes()), sk.verifying_key().to_bytes())
}

/// Operator tooling: sign a relay list with a base64 private key (from [`generate_signing_key`]).
/// Returns the one-line signed wire JSON to drop as the relay's `relay-list.json`.
pub fn sign_list(
    privkey_b64: &str,
    version: u64,
    issued_at: u64,
    relays: Vec<String>,
) -> Option<String> {
    let bytes: [u8; 32] = B64
        .decode(privkey_b64.trim().as_bytes())
        .ok()?
        .try_into()
        .ok()?;
    let sk = ed25519_dalek::SigningKey::from_bytes(&bytes);
    let dir = RelayDirectory {
        version,
        issued_at,
        relays,
    };
    Some(sign(&dir, &sk).to_wire())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn signed_directory_round_trips_and_rejects_tampering() {
        // A deterministic key (no rng needed) for the test.
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let dir = RelayDirectory {
            version: 3,
            issued_at: 1_700_000_000,
            relays: vec!["aaaa.onion".into(), "bbbb.onion".into()],
        };
        let signed = sign(&dir, &sk);

        // Round-trips over the wire and verifies with the right key.
        let wire = signed.to_wire();
        let parsed = SignedDirectory::from_wire(&wire).unwrap();
        assert_eq!(parsed.verify(&pk).as_ref(), Some(&dir));

        // Wrong key → rejected.
        let other = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(parsed.verify(&other).is_none());

        // Tampered payload (swap a relay) → signature no longer matches → rejected.
        let mut tampered = parsed.clone();
        let mut bytes = B64.decode(tampered.payload.as_bytes()).unwrap();
        if let Some(b) = bytes.iter_mut().find(|b| **b == b'a') {
            *b = b'z';
        }
        tampered.payload = B64.encode(&bytes);
        assert!(tampered.verify(&pk).is_none());

        // The unconfigured all-zero key never verifies anything.
        assert!(parsed.verify(&[0u8; 32]).is_none());
    }
}
