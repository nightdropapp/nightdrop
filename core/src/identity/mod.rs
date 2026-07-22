//! Anonymous, device-held identity (`ARCHITECTURE.md` §4): a long-term Olm
//! [`Account`] (Curve25519 + Ed25519 keypairs). No registration, no accounts.
//!
//! The account also issues one-time pre-keys for the X3DH-style handshake that
//! `crypto::` uses to open a Double Ratchet session (§5a QR bundle, §5b after PAKE).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use vodozemac::olm::Account;
use vodozemac::Curve25519PublicKey;

/// One end of an X3DH handshake: the published keys another device needs to open a
/// session with us. In the QR path this is embedded in the code (§5a); in the
/// short-code path it is fetched (encrypted) from the rendezvous after the PAKE (§5b).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreKeyBundle {
    /// Long-term Curve25519 identity key (base64).
    pub identity_key: String,
    /// A one-time pre-key (base64). Consumed on first use.
    pub one_time_key: String,
}

/// This device's anonymous identity. Wraps the Olm [`Account`]; key material never
/// leaves this type except as public keys or an encrypted pickle (`storage::`).
pub struct LocalIdentity {
    account: Account,
}

impl LocalIdentity {
    /// Generate a fresh identity on this device.
    pub fn generate() -> Self {
        Self {
            account: Account::new(),
        }
    }

    /// Rebuild an identity from a restored Olm account (`storage::`).
    pub(crate) fn from_account(account: Account) -> Self {
        Self { account }
    }

    /// Stable public identifier derived from the Ed25519 identity key — not a username.
    /// Short and URL-safe so it can live in a `nightdrop://` link.
    pub fn id(&self) -> String {
        let ed = self.account.ed25519_key().as_bytes().to_vec();
        // First 8 bytes are plenty to disambiguate locally; full key is exchanged in
        // the handshake. This is a display/lookup handle, not a security boundary.
        URL_SAFE_NO_PAD.encode(&ed[..8])
    }

    /// The long-term Curve25519 identity key (used by peers to open a session with us).
    pub fn curve25519(&self) -> Curve25519PublicKey {
        self.account.curve25519_key()
    }

    /// Mint a fresh pre-key bundle to hand to a peer who wants to message us.
    pub fn publish_prekey_bundle(&mut self) -> PreKeyBundle {
        self.account.generate_one_time_keys(1);
        let otk = self
            .account
            .one_time_keys()
            .values()
            .next()
            .cloned()
            .expect("just generated a one-time key");
        let bundle = PreKeyBundle {
            identity_key: self.account.curve25519_key().to_base64(),
            one_time_key: otk.to_base64(),
        };
        self.account.mark_keys_as_published();
        bundle
    }

    /// Shared (immutable) access for outbound session creation (`crypto::`).
    pub(crate) fn account(&self) -> &Account {
        &self.account
    }

    /// Mutable access for inbound session creation, which consumes a one-time key.
    pub(crate) fn account_mut(&mut self) -> &mut Account {
        &mut self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = LocalIdentity::generate();
        assert_eq!(a.id(), a.id(), "id is stable for a given identity");
        let b = LocalIdentity::generate();
        assert_ne!(a.id(), b.id(), "fresh identities differ");
    }

    #[test]
    fn prekey_bundle_keys_are_valid_base64_curve_keys() {
        let mut id = LocalIdentity::generate();
        let bundle = id.publish_prekey_bundle();
        assert!(Curve25519PublicKey::from_base64(&bundle.identity_key).is_ok());
        assert!(Curve25519PublicKey::from_base64(&bundle.one_time_key).is_ok());
    }
}
