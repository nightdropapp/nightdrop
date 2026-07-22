//! Hybrid post-quantum key encapsulation for the short-code pairing handshake (`ARCHITECTURE.md`
//! §5b, IMPROVEMENT_PLAN §4.1). The short-code rendezvous payload — the inviter's pre-keys and
//! onion address — travels through the **untrusted relay**, sealed under the SPAKE2 shared secret.
//! SPAKE2 is classical, so a *harvest-now-decrypt-later* adversary who records the rendezvous today
//! could recover that payload once a quantum computer breaks the classical handshake.
//!
//! To close that gap we mix an **ML-KEM-768 (FIPS 203)** shared secret into the seal key. The joiner
//! ships a fresh ML-KEM public key with its opener; the inviter encapsulates to it; the payload seal
//! key is `HKDF(pake_secret ‖ kem_secret)`. Recovering it now requires breaking **both** the classical
//! PAKE **and** ML-KEM — it is a true hybrid, never PQ-only, so a flaw in the (younger) lattice scheme
//! never *weakens* today's classical guarantee. The symmetric Double Ratchet already has a strong
//! quantum margin and is untouched.
//!
//! The QR pairing path is deliberately *not* wrapped: a QR code is scanned optically, in person — it
//! never crosses the network, so there is nothing to harvest.

use ml_kem::array::Array;
use ml_kem::kem::{Encapsulate, Kem, KeyExport, TryDecapsulate};
use ml_kem::{Ciphertext, Key, MlKem768};

use crate::Result;

/// ML-KEM-768 encapsulation-key (public key) length in bytes.
pub const KEM_PUBLIC_LEN: usize = 1184;
/// ML-KEM-768 ciphertext length in bytes.
pub const KEM_CIPHERTEXT_LEN: usize = 1088;

type EncapKey = ml_kem::EncapsulationKey<MlKem768>;
type DecapKey = ml_kem::DecapsulationKey<MlKem768>;

/// Copy a 32-byte ML-KEM shared secret out of its array wrapper.
fn ss_bytes(ss: impl AsRef<[u8]>) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(ss.as_ref());
    out
}

/// A joiner-side ML-KEM keypair. The `public` bytes go out with the SPAKE2 opener; the secret
/// `decap` stays local and is dropped as soon as the handshake finishes (held only transiently).
pub struct KemKeypair {
    decap: DecapKey,
    pub public: Vec<u8>,
}

/// Generate a fresh ML-KEM-768 keypair for one pairing attempt.
pub fn generate() -> KemKeypair {
    let (decap, encap) = MlKem768::generate_keypair();
    KemKeypair {
        decap,
        public: encap.to_bytes().as_slice().to_vec(),
    }
}

/// Inviter side: encapsulate to the joiner's public key. Returns `(ciphertext, shared_secret)` —
/// post the ciphertext back, mix the shared secret into the seal key ([`hybrid_seal_key`]).
pub fn encapsulate(peer_public: &[u8]) -> Result<(Vec<u8>, [u8; 32])> {
    let encoded: Key<EncapKey> = Array::try_from(peer_public)
        .map_err(|_| anyhow::anyhow!("bad ML-KEM public key length"))?;
    let encap =
        EncapKey::new(&encoded).map_err(|_| anyhow::anyhow!("invalid ML-KEM public key"))?;
    let (ct, ss) = encap.encapsulate();
    Ok((ct.as_slice().to_vec(), ss_bytes(ss)))
}

impl KemKeypair {
    /// Joiner side: recover the shared secret the inviter encapsulated to us.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32]> {
        let ct: Ciphertext<MlKem768> = Array::try_from(ciphertext)
            .map_err(|_| anyhow::anyhow!("bad ML-KEM ciphertext length"))?;
        let ss = self
            .decap
            .try_decapsulate(&ct)
            .map_err(|_| anyhow::anyhow!("ML-KEM decapsulation failed"))?;
        Ok(ss_bytes(ss))
    }
}

/// Combine the classical PAKE secret with the ML-KEM shared secret into the 32-byte payload-seal
/// key (§4.1). HKDF-SHA256 over both inputs, domain-separated — the output is secure unless **both**
/// the PAKE and ML-KEM are broken (hybrid).
pub fn hybrid_seal_key(pake_secret: &[u8], kem_secret: &[u8; 32]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let mut ikm = Vec::with_capacity(pake_secret.len() + kem_secret.len());
    ikm.extend_from_slice(pake_secret);
    ikm.extend_from_slice(kem_secret);
    let hk = Hkdf::<Sha256>::new(Some(b"nightdrop/rendezvous/hybrid/v1"), &ikm);
    let mut okm = [0u8; 32];
    hk.expand(b"payload-seal", &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encapsulate_decapsulate_round_trips() {
        let kp = generate();
        assert_eq!(kp.public.len(), KEM_PUBLIC_LEN);
        let (ct, ss_sender) = encapsulate(&kp.public).unwrap();
        assert_eq!(ct.len(), KEM_CIPHERTEXT_LEN);
        let ss_receiver = kp.decapsulate(&ct).unwrap();
        assert_eq!(
            ss_sender, ss_receiver,
            "both sides derive the same ML-KEM secret"
        );
    }

    #[test]
    fn a_different_keypair_decapsulates_to_a_different_secret() {
        // ML-KEM is IND-CCA2: decapsulating someone else's ciphertext yields an unrelated secret
        // (implicit rejection), never an error a probing attacker could use.
        let alice = generate();
        let (ct, ss) = encapsulate(&alice.public).unwrap();
        let mallory = generate();
        assert_ne!(ss, mallory.decapsulate(&ct).unwrap());
    }

    #[test]
    fn hybrid_key_depends_on_both_inputs() {
        let ss = [7u8; 32];
        let base = hybrid_seal_key(b"pake-secret", &ss);
        // Changing either input changes the key; matching inputs reproduce it.
        assert_eq!(base, hybrid_seal_key(b"pake-secret", &ss));
        assert_ne!(base, hybrid_seal_key(b"pake-secret2", &ss));
        assert_ne!(base, hybrid_seal_key(b"pake-secret", &[8u8; 32]));
    }

    #[test]
    fn a_malformed_public_key_is_rejected_not_panicked() {
        assert!(encapsulate(b"too short").is_err());
    }
}
