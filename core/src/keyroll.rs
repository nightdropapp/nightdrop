//! Rotating a signing key that released apps carry (self-audit 2026-10-13, key-compromise plan).
//!
//! The update manifest and the relay list are each signed by a key whose public half is baked into
//! every released app. Baked in alone, it could never change: a leaked key would sign forever, and a
//! lost one would end updates for every install. So each purpose ships **two** public keys:
//!
//! * the **signing** key, which signs every manifest or list (its secret lives offline, on a USB
//!   stick, and is plugged in only to sign);
//! * a **recovery** key, which signs nothing but a [`Rotation`]: "for `purpose`, the signing key is
//!   now `key`, sequence `seq`". Its secret is kept apart from the signing key and used only then.
//!
//! The app trusts the newest signing key it has been shown with a valid rotation, and **remembers
//! it** ([`Trust`], kept in the sealed store). Once it has seen sequence `n`, an older key — the
//! baked one included — is refused, so whoever holds a leaked old key cannot roll it back. Every
//! later manifest or list carries the rotation, so an app that missed it learns it on its next fetch.
//! A rotation's signature binds the purpose, so one for the relay list can never move the update key.
//!
//! What this cannot do: survive the recovery key itself leaking. That key is the root; keep it
//! offline and apart.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// What a key signs. Bound into every rotation signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Update,
    Directory,
}

impl Purpose {
    fn label(self) -> &'static str {
        match self {
            Purpose::Update => "update",
            Purpose::Directory => "directory",
        }
    }

    /// Parse the operator tool's argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "update" => Some(Purpose::Update),
            "directory" => Some(Purpose::Directory),
            _ => None,
        }
    }
}

/// The two public keys a release carries for one purpose.
#[derive(Clone, Copy, Debug)]
pub struct Roots {
    pub purpose: Purpose,
    /// The signing key at release time (sequence 0).
    pub signing: [u8; 32],
    /// The key that may name a new signing key.
    pub recovery: [u8; 32],
}

/// "For `purpose`, the signing key is now `key` (base64), sequence `seq`", signed by the recovery key.
/// Published alongside every manifest or list from the day it is made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rotation {
    pub seq: u64,
    pub key: String,
    pub sig: String,
}

/// The newest signing key this install has accepted for one purpose. Sequence 0 is the baked key;
/// it is never stored, since the baked key already says it.
///
/// Kept **with its rotation signature** and re-verified every time it is used: it comes back from
/// the state file and from backups, and a backup can be someone else's file. A bare `seq`/`key`
/// would let a crafted backup pin a bogus key with a huge sequence, after which every real manifest
/// and relay list is refused for good (self-audit 2026-10-17, P9). One without a valid signature —
/// including those written before the signature was kept — is ignored, falling back to the baked key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Trust {
    pub seq: u64,
    pub key: String,
    #[serde(default)]
    pub sig: String,
}

fn message(purpose: Purpose, seq: u64, key_b64: &str) -> Vec<u8> {
    format!(
        "nightdrop/key-rotation/v1\n{}\n{seq}\n{key_b64}",
        purpose.label()
    )
    .into_bytes()
}

/// A public key in the base64 form a [`Rotation`] names (operator tooling).
pub fn encode_key(key: &[u8; 32]) -> String {
    B64.encode(key)
}

impl Rotation {
    /// One-line JSON, as published beside a manifest or list.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s.trim()).ok()
    }
}

fn decode_key(b64: &str) -> Option<[u8; 32]> {
    B64.decode(b64.trim()).ok()?.try_into().ok()
}

/// The signing key to verify with now, and — when `rotation` moved it forward — the [`Trust`] to
/// remember. A rotation that does not verify under the recovery key, names another purpose, or is not
/// newer than what is trusted is ignored, never an error: the manifest is then checked against the
/// key already trusted, and fails there if it was signed by anything else.
pub fn current_key(
    roots: &Roots,
    learned: Option<&Trust>,
    rotation: Option<&Rotation>,
) -> ([u8; 32], Option<Trust>) {
    let learned = learned.filter(|t| {
        verify_rotation(
            roots,
            &Rotation {
                seq: t.seq,
                key: t.key.clone(),
                sig: t.sig.clone(),
            },
        )
    });
    let (mut seq, mut key) = match learned.and_then(|t| decode_key(&t.key).map(|k| (t.seq, k))) {
        Some((s, k)) if s > 0 => (s, k),
        _ => (0, roots.signing),
    };
    let mut moved = None;
    if let Some(r) = rotation {
        if r.seq > seq && verify_rotation(roots, r) {
            if let Some(k) = decode_key(&r.key) {
                seq = r.seq;
                key = k;
                moved = Some(Trust {
                    seq,
                    key: r.key.trim().to_string(),
                    sig: r.sig.trim().to_string(),
                });
            }
        }
    }
    (key, moved)
}

fn verify_rotation(roots: &Roots, r: &Rotation) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(&roots.recovery) else {
        return false;
    };
    let Some(sig) = B64
        .decode(r.sig.trim())
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
    else {
        return false;
    };
    vk.verify_strict(
        &message(roots.purpose, r.seq, r.key.trim()),
        &Signature::from_bytes(&sig),
    )
    .is_ok()
}

/// Operator tooling (`nightdrop-relay sign-rotation`): name `new_key_b64` the signing key for
/// `purpose` from sequence `seq`, with the base64 recovery secret. `None` for a malformed key.
pub fn sign_rotation(
    recovery_secret_b64: &str,
    purpose: Purpose,
    seq: u64,
    new_key_b64: &str,
) -> Option<Rotation> {
    let mut secret: [u8; 32] = B64
        .decode(recovery_secret_b64.trim())
        .ok()?
        .try_into()
        .ok()?;
    decode_key(new_key_b64)?;
    let sk = SigningKey::from_bytes(&secret);
    zeroize::Zeroize::zeroize(&mut secret);
    let sig = sk.sign(&message(purpose, seq, new_key_b64.trim()));
    Some(Rotation {
        seq,
        key: new_key_b64.trim().to_string(),
        sig: B64.encode(sig.to_bytes()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (String, [u8; 32]) {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        (B64.encode(sk.to_bytes()), sk.verifying_key().to_bytes())
    }

    fn roots(purpose: Purpose) -> (Roots, String, String) {
        let (signing_secret, signing) = keypair();
        let (recovery_secret, recovery) = keypair();
        (
            Roots {
                purpose,
                signing,
                recovery,
            },
            signing_secret,
            recovery_secret,
        )
    }

    /// The baked public keys must belong to the offline secrets, or a rotation (or a list) signed
    /// with them would be refused by every app. Run with the key USB plugged in:
    /// `NIGHTDROP_KEY_DIR=<usb> cargo test -p nightdrop baked_keys_match -- --ignored`. It derives
    /// each public key from its secret file and compares; nothing is signed, so nothing usable
    /// comes out of it.
    #[test]
    #[ignore]
    fn baked_keys_match_the_offline_secrets() {
        let dir = std::path::PathBuf::from(std::env::var("NIGHTDROP_KEY_DIR").unwrap());
        let public = |file: &str| -> [u8; 32] {
            let secret = std::fs::read_to_string(dir.join(file)).unwrap();
            let bytes: [u8; 32] = B64.decode(secret.trim()).unwrap().try_into().unwrap();
            SigningKey::from_bytes(&bytes).verifying_key().to_bytes()
        };
        let u = crate::update::UPDATE_ROOTS;
        let d = crate::directory::DIRECTORY_ROOTS;
        assert_eq!(
            public("update-signing-key"),
            u.signing,
            "update signing key"
        );
        assert_eq!(
            public("recovery/update-recovery-key"),
            u.recovery,
            "update recovery key"
        );
        assert_eq!(
            public("directory-signing-key"),
            d.signing,
            "directory signing key"
        );
        assert_eq!(
            public("recovery/directory-recovery-key"),
            d.recovery,
            "directory recovery key"
        );
    }

    #[test]
    fn without_a_rotation_the_baked_key_is_used() {
        let (r, _, _) = roots(Purpose::Update);
        assert_eq!(current_key(&r, None, None), (r.signing, None));
    }

    #[test]
    fn a_recovery_signed_rotation_moves_the_key_and_is_remembered() {
        let (r, _, recovery_secret) = roots(Purpose::Update);
        let (_, new_key) = keypair();
        let rot =
            sign_rotation(&recovery_secret, Purpose::Update, 1, &B64.encode(new_key)).unwrap();
        let (key, learned) = current_key(&r, None, Some(&rot));
        assert_eq!(key, new_key);
        let learned = learned.unwrap();
        assert_eq!(learned.seq, 1);
        // Remembered: a later fetch without the rotation still uses the new key, never the old one —
        // so a leaked old signing key cannot roll an install back.
        assert_eq!(current_key(&r, Some(&learned), None).0, new_key);
    }

    #[test]
    fn forged_stale_or_cross_purpose_rotations_change_nothing() {
        let (r, signing_secret, recovery_secret) = roots(Purpose::Update);
        let (_, new_key) = keypair();
        let new_b64 = B64.encode(new_key);
        // Signed by the (leaked) signing key instead of the recovery key.
        let forged = sign_rotation(&signing_secret, Purpose::Update, 1, &new_b64).unwrap();
        assert_eq!(current_key(&r, None, Some(&forged)), (r.signing, None));
        // A directory rotation cannot move the update key.
        let cross = sign_rotation(&recovery_secret, Purpose::Directory, 1, &new_b64).unwrap();
        assert_eq!(current_key(&r, None, Some(&cross)), (r.signing, None));
        // An older rotation after a newer one is ignored.
        let (_, newer) = keypair();
        let two = sign_rotation(&recovery_secret, Purpose::Update, 2, &B64.encode(newer)).unwrap();
        let (_, learned) = current_key(&r, None, Some(&two));
        let learned = learned.unwrap();
        let one = sign_rotation(&recovery_secret, Purpose::Update, 1, &new_b64).unwrap();
        assert_eq!(current_key(&r, Some(&learned), Some(&one)), (newer, None));
        // A tampered sequence number breaks the signature.
        let mut bumped = one.clone();
        bumped.seq = 9;
        assert_eq!(current_key(&r, None, Some(&bumped)), (r.signing, None));
    }

    #[test]
    fn a_remembered_key_without_a_valid_signature_is_ignored() {
        // What a crafted backup or an old state file can hand back: it must not pin anything.
        let (r, _, recovery_secret) = roots(Purpose::Update);
        let (_, bogus) = keypair();
        let unsigned = Trust {
            seq: u64::MAX,
            key: B64.encode(bogus),
            sig: String::new(),
        };
        assert_eq!(current_key(&r, Some(&unsigned), None), (r.signing, None));
        // A real rotation still moves an install that was handed one.
        let (_, new_key) = keypair();
        let rot =
            sign_rotation(&recovery_secret, Purpose::Update, 1, &B64.encode(new_key)).unwrap();
        assert_eq!(current_key(&r, Some(&unsigned), Some(&rot)).0, new_key);
        // A signed one whose key was swapped afterwards is refused too.
        let (_, learned) = current_key(&r, None, Some(&rot));
        let mut swapped = learned.unwrap();
        swapped.key = B64.encode(bogus);
        assert_eq!(current_key(&r, Some(&swapped), None), (r.signing, None));
    }
}
