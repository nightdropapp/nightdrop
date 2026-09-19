//! Desktop prototype: tune a BoringSSL ClientHello to match Chrome's, measured by JA4.
//!   cargo run -p webtunnel-client --example boring_hello
use boring::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, SslConnector, SslMethod, SslVersion,
};
use std::io::Write;
use std::net::TcpStream;

/// Brotli certificate (de)compression (RFC 8879, algorithm 2) — what Chrome advertises.
struct Brotli;
impl CertificateCompressor for Brotli {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = false; // a client only advertises + decompresses server certs
    const CAN_DECOMPRESS: bool = true;
    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let mut r = brotli::Decompressor::new(input, 4096);
        std::io::copy(&mut r, output).map(|_| ())
    }
}

// Chrome's cipher list (12 TLS1.2 suites; the 3 TLS1.3 suites are added by boring).
const CIPHERS: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:\
ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:\
AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

fn main() {
    let mut b = SslConnector::builder(SslMethod::tls()).unwrap();
    b.set_min_proto_version(Some(SslVersion::TLS1_2)).unwrap();
    b.set_max_proto_version(Some(SslVersion::TLS1_3)).unwrap();
    b.set_grease_enabled(true);
    b.set_permute_extensions(true);
    b.set_cipher_list(CIPHERS).unwrap();
    b.set_curves_list("X25519MLKEM768:X25519:P-256:P-384")
        .unwrap();
    b.set_alpn_protos(b"\x08http/1.1").unwrap();
    b.enable_ocsp_stapling();
    b.enable_signed_cert_timestamps();
    b.add_certificate_compression_algorithm(Brotli).unwrap();
    let conn = b.build();
    let mut cc = conn.configure().unwrap();
    cc.set_use_server_name_indication(true);
    cc.set_verify_hostname(false);
    cc.set_enable_ech_grease(true);
    let sock = TcpStream::connect("127.0.0.1:18443").unwrap();
    let _ = cc.connect("ws.test", sock);
    eprintln!("hello sent");
}
