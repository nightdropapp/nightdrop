//! The TLS layer.
//!
//! Three ways to accept a server certificate, matching lyrebird:
//! - default: a normal WebPKI check against Mozilla's roots, for the SNI name;
//! - `cert-domain` / `sni-imitation`: the same check, but for a different name than the SNI;
//! - `cert=`: a SHA-256 pin over the whole presented chain, replacing CA validation.
//!
//! ALPN is deliberately left empty. A WebTunnel server behind nginx would otherwise be free to
//! pick `h2`, and the HTTP/1.1 Upgrade the protocol depends on does not exist in HTTP/2. lyrebird
//! makes the same choice (`hellorandomizednoalpn`).
//!
//! NOTE: this is rustls's own ClientHello, which is recognisable as rustls. Disguising it is the
//! separate, second step of the design; until then this layer is correct but not stealthy.

use crate::config::TlsConfig;
use crate::Error;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::{Arc, LazyLock};

static PROVIDER: LazyLock<Arc<CryptoProvider>> =
    LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));

static WEBPKI: LazyLock<Arc<WebPkiServerVerifier>> = LazyLock::new(|| {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), PROVIDER.clone())
        .build()
        .expect("Mozilla root store is non-empty")
});

/// The rustls client config for one connection.
pub(crate) fn client_config(tls: &TlsConfig) -> Result<Arc<rustls::ClientConfig>, Error> {
    let verifier: Arc<dyn ServerCertVerifier> = match (&tls.pinned_chain_hash, &tls.verify_name) {
        (Some(pin), _) => Arc::new(PinVerifier { pin: *pin }),
        (None, Some(name)) => Arc::new(OtherNameVerifier {
            name: ServerName::try_from(name.clone())
                .map_err(|e| Error::Config(format!("cert-domain {name:?}: {e}")))?,
        }),
        (None, None) => WEBPKI.clone(),
    };
    let config = rustls::ClientConfig::builder_with_provider(PROVIDER.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// lyrebird's certificate-chain hash (`certiChainHashCalc.GenerateCertChainHash`):
/// `h = sha256(c0)`, then `h = sha256(h ‖ sha256(ci))` for each further certificate, in the
/// order the server sent them.
pub fn chain_hash<'a>(chain: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut hash: Option<[u8; 32]> = None;
    for cert in chain {
        let this: [u8; 32] = Sha256::digest(cert).into();
        hash = Some(match hash {
            None => this,
            Some(prev) => {
                let mut h = Sha256::new();
                h.update(prev);
                h.update(this);
                h.finalize().into()
            }
        });
    }
    hash.unwrap_or_default()
}

/// Handshake signatures are still checked in every mode; only *which certificate is trusted*
/// differs.
macro_rules! delegate_signatures {
    () => {
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(
                message,
                cert,
                dss,
                &PROVIDER.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(
                message,
                cert,
                dss,
                &PROVIDER.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            PROVIDER
                .signature_verification_algorithms
                .supported_schemes()
        }
    };
}

#[derive(Debug)]
struct PinVerifier {
    pin: [u8; 32],
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let chain =
            std::iter::once(end_entity.as_ref()).chain(intermediates.iter().map(|c| c.as_ref()));
        let got = chain_hash(chain);
        // Not secret, but there's no reason to leak timing either.
        let diff = got
            .iter()
            .zip(self.pin.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if diff == 0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate chain does not match the bridge's cert= pin".into(),
            ))
        }
    }

    delegate_signatures!();
}

#[derive(Debug)]
struct OtherNameVerifier {
    name: ServerName<'static>,
}

impl ServerCertVerifier for OtherNameVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        WEBPKI.verify_server_cert(end_entity, intermediates, &self.name, ocsp, now)
    }

    delegate_signatures!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Reference values from the Go implementation's `GenerateCertChainHash` (webtunnel at
    /// 11334a2), run over the byte strings "a", "b", "c".
    #[test]
    fn chain_hash_matches_go() {
        let h = |parts: &[&[u8]]| hex(&chain_hash(parts.iter().copied()));
        assert_eq!(
            h(&[b"a"]),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
        assert_eq!(
            h(&[b"a", b"b"]),
            "e5a01fee14e0ed5c48714f22180f25ad8365b53f9779f79dc4a3d7e93963f94a"
        );
        assert_eq!(
            h(&[b"a", b"b", b"c"]),
            "7075152d03a5cd92104887b476862778ec0c87be5c2fa1c0a90f87c49fad6eff"
        );
    }
}
