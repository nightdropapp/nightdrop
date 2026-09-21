//! The TLS layer, with the backend chosen at compile time.
//!
//! - **default (pure Rust): rustls.** Correct, and it cross-compiles to Android with no C, but
//!   its ClientHello is recognisably rustls.
//! - **`chrome-proto`: BoringSSL.** `connect()` emits a ClientHello byte-identical to Chromium
//!   opening a WebSocket (verified by `tests/fingerprint.rs`), so a censor fingerprinting the
//!   handshake sees Chrome. BoringSSL is C/cmake, hence the feature gate.
//!
//! Both back ends expose the same [`connect_tls`] and [`TlsStream`], and both implement the three
//! certificate-acceptance modes lyrebird uses:
//! - default: WebPKI against Mozilla's roots, for the SNI name;
//! - `cert-domain` / `sni-imitation` (`TlsConfig::verify_name`): WebPKI, but for a name other
//!   than the SNI;
//! - `cert=` (`TlsConfig::pinned_chain_hash`): a SHA-256 pin over the presented chain, replacing
//!   CA validation.
//!
//! ALPN is `http/1.1` only. A WebTunnel server behind nginx offered `h2` would negotiate HTTP/2,
//! where the Upgrade the protocol depends on does not exist. (Chrome's WebSocket hello is also
//! `http/1.1`-only, so the boring back end matches it without deviating.)

use sha2::{Digest, Sha256};

pub(crate) use backend::{connect_tls, TlsStream};

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

/// Constant-time equality for the 32-byte chain hash / pin. Not secret, but no reason to leak.
fn hashes_equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ============================================================ rustls back end

#[cfg(not(feature = "chrome-proto"))]
mod backend {
    use super::{chain_hash, hashes_equal};
    use crate::config::TlsConfig;
    use crate::Error;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::client::WebPkiServerVerifier;
    use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
    use std::io;
    use std::sync::{Arc, LazyLock};
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    pub(crate) type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;

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

    pub(crate) async fn connect_tls(
        tcp: TcpStream,
        sni: &str,
        cfg: &TlsConfig,
    ) -> Result<TlsStream, Error> {
        let server_name = ServerName::try_from(sni.to_string())
            .map_err(|e| Error::Config(format!("server name {sni:?}: {e}")))?;
        let connector = TlsConnector::from(client_config(cfg)?);
        connector.connect(server_name, tcp).await.map_err(|e| {
            // rustls reports certificate rejections as InvalidData wrapping its own error.
            if e.kind() == io::ErrorKind::InvalidData {
                Error::Tls(e.to_string())
            } else {
                Error::io("TLS handshake", e)
            }
        })
    }

    fn client_config(tls: &TlsConfig) -> Result<Arc<rustls::ClientConfig>, Error> {
        let verifier: Arc<dyn ServerCertVerifier> = match (&tls.pinned_chain_hash, &tls.verify_name)
        {
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
            let chain = std::iter::once(end_entity.as_ref())
                .chain(intermediates.iter().map(|c| c.as_ref()));
            if hashes_equal(&chain_hash(chain), &self.pin) {
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
}

// ========================================================== BoringSSL back end

#[cfg(feature = "chrome-proto")]
mod backend {
    use super::{chain_hash, hashes_equal};
    use crate::config::TlsConfig;
    use crate::Error;
    use boring::ssl::{
        CertificateCompressionAlgorithm, CertificateCompressor, SslAlert, SslConnector, SslMethod,
        SslRef, SslVerifyError, SslVerifyMode, SslVersion,
    };
    use boring::x509::store::{X509Store, X509StoreBuilder};
    use boring::x509::X509;
    use std::io::Write;
    use std::sync::LazyLock;
    use tokio::net::TcpStream;

    pub(crate) type TlsStream = tokio_boring::SslStream<TcpStream>;

    /// Chrome's TLS 1.2 cipher list; BoringSSL prepends the three TLS 1.3 suites. Matches the
    /// profile validated against a real Chromium hello in `tests/fingerprint.rs`.
    const CHROME_CIPHERS: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:\
ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:\
AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

    /// One connector, built once: the Chrome ClientHello profile plus PEER verification against
    /// Mozilla's roots. Per connection we override only the acceptance decision (pin, or a
    /// different verify-host) on the derived `ConnectConfiguration`.
    static CHROME_CONNECTOR: LazyLock<SslConnector> = LazyLock::new(build_chrome_connector);

    pub(crate) async fn connect_tls(
        tcp: TcpStream,
        sni: &str,
        cfg: &TlsConfig,
    ) -> Result<TlsStream, Error> {
        let mut cc = CHROME_CONNECTOR
            .configure()
            .map_err(|e| Error::Tls(format!("TLS configure: {e}")))?;
        cc.set_enable_ech_grease(true);

        match cfg.pinned_chain_hash {
            // cert= : the chain hash *is* the identity, so replace CA + hostname validation.
            Some(pin) => {
                cc.set_custom_verify_callback(SslVerifyMode::PEER, move |ssl| {
                    verify_pin(ssl, &pin)
                });
            }
            // CA validation (inherited PEER + Mozilla store), but against verify_name when the
            // certificate's name differs from the SNI we send.
            None => {
                let host = cfg.verify_name.clone().unwrap_or_else(|| sni.to_string());
                cc.set_verify_hostname(false);
                cc.param_mut()
                    .set_host(&host)
                    .map_err(|e| Error::Config(format!("verify host {host:?}: {e}")))?;
            }
        }

        // `sni` is the SNI sent; verification is governed by the block above, not by this name.
        tokio_boring::connect(cc, sni, tcp)
            .await
            .map_err(|e| Error::Tls(e.to_string()))
    }

    fn verify_pin(ssl: &mut SslRef, pin: &[u8; 32]) -> Result<(), SslVerifyError> {
        let chain = ssl
            .peer_cert_chain()
            .ok_or(SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN))?;
        let ders: Vec<Vec<u8>> = chain
            .iter()
            .map(|c| c.to_der())
            .collect::<Result<_, _>>()
            .map_err(|_| SslVerifyError::Invalid(SslAlert::BAD_CERTIFICATE))?;
        if hashes_equal(&chain_hash(ders.iter().map(|d| d.as_slice())), pin) {
            Ok(())
        } else {
            Err(SslVerifyError::Invalid(SslAlert::BAD_CERTIFICATE))
        }
    }

    fn build_chrome_connector() -> SslConnector {
        let mut b = SslConnector::builder(SslMethod::tls()).expect("tls client method");
        b.set_min_proto_version(Some(SslVersion::TLS1_2))
            .expect("min version");
        b.set_max_proto_version(Some(SslVersion::TLS1_3))
            .expect("max version");
        b.set_grease_enabled(true);
        b.set_permute_extensions(true);
        b.set_cipher_list(CHROME_CIPHERS).expect("cipher list");
        b.set_curves_list("X25519MLKEM768:X25519:P-256:P-384")
            .expect("curves");
        b.set_alpn_protos(b"\x08http/1.1").expect("alpn");
        b.enable_ocsp_stapling();
        b.enable_signed_cert_timestamps();
        b.add_certificate_compression_algorithm(Brotli)
            .expect("cert compression");
        b.set_verify(SslVerifyMode::PEER);
        b.set_verify_cert_store(mozilla_roots())
            .expect("root store");
        b.build()
    }

    /// The same Mozilla root set as the rustls path's `webpki-roots`, as DER, loaded into a
    /// BoringSSL store. Deterministic and Android-safe (BoringSSL's default path lookup finds no
    /// roots on Android).
    fn mozilla_roots() -> X509Store {
        let mut store = X509StoreBuilder::new().expect("store builder");
        for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
            let cert = X509::from_der(der.as_ref()).expect("bundled root is valid DER");
            store.add_cert(cert).expect("add root");
        }
        store.build()
    }

    /// Brotli certificate decompression (RFC 8879, algorithm 2) — advertised so the ClientHello
    /// carries the `compress_certificate` extension Chrome sends; only decompression is needed on
    /// a client.
    struct Brotli;
    impl CertificateCompressor for Brotli {
        const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
        const CAN_COMPRESS: bool = false;
        const CAN_DECOMPRESS: bool = true;
        fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
            std::io::copy(&mut brotli::Decompressor::new(input, 4096), output).map(|_| ())
        }
    }
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
