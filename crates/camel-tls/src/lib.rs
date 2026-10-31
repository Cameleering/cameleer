//! camel-tls: a std-only TLS 1.3 (RFC 8446) and TLS 1.2 (RFC 5246, ECDHE + AEAD
//! suites only) client and server, with all cryptography implemented from scratch.
//!
//! ```no_run
//! use camel_tls::{TlsClientConfig, TlsServerConfig, TlsStream};
//! use std::io::{Read, Write};
//! use std::net::{TcpListener, TcpStream};
//!
//! // client (system trust store, hostname verification)
//! let cfg = TlsClientConfig::builder().alpn_protocols(&["http/1.1"]).build();
//! let tcp = TcpStream::connect("example.com:443").unwrap();
//! let mut tls = TlsStream::connect(tcp, "example.com", &cfg).unwrap();
//! tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n").unwrap();
//! let mut resp = Vec::new();
//! tls.read_to_end(&mut resp).unwrap();
//!
//! // server (self-signed certificate generated at runtime)
//! let ss = camel_tls::self_signed("localhost");
//! let scfg = TlsServerConfig::from_pem(ss.cert_pem.as_bytes(), ss.key_pem.as_bytes()).unwrap();
//! let listener = TcpListener::bind("127.0.0.1:8443").unwrap();
//! let (sock, _) = listener.accept().unwrap();
//! let mut conn = TlsStream::accept(sock, &scfg).unwrap();
//! conn.write_all(b"hello").unwrap();
//! conn.close().unwrap();
//! ```
//!
//! # Supported parameters
//!
//! * TLS 1.3: `TLS_AES_128_GCM_SHA256`, `TLS_AES_256_GCM_SHA384`,
//!   `TLS_CHACHA20_POLY1305_SHA256`; HelloRetryRequest, KeyUpdate, middlebox
//!   compatibility mode; NewSessionTicket is accepted and ignored (no resumption, no 0-RTT, no PSK).
//! * TLS 1.2: `ECDHE_{ECDSA,RSA}_WITH_{AES_128_GCM_SHA256,AES_256_GCM_SHA384,CHACHA20_POLY1305_SHA256}`
//!   with extended master secret (RFC 7627) and secure-renegotiation signalling (renegotiation itself is refused).
//! * Groups: x25519, secp256r1, secp384r1.
//! * Signatures: ecdsa_secp256r1_sha256, ecdsa_secp384r1_sha384, rsa_pss_rsae_sha{256,384,512},
//!   rsa_pkcs1_sha{256,384,512} (TLS 1.2 handshakes and certificates only). Certificates may also
//!   use RSASSA-PSS, sha1WithRSA and ECDSA-SHA1/512 signatures.
//! * Client certificates are not supported (a CertificateRequest is answered with an empty chain).
//!
//! # Security notes and limitations
//!
//! This is an independent implementation that has not been audited. Constant-time
//! behaviour is a best effort:
//! * ChaCha20, Poly1305, X25519, GHASH, the P-256/P-384 field and scalar arithmetic
//!   (Montgomery multiplication, branch-free reductions, complete addition formulas,
//!   fixed-length double-and-add-always ladder with masked selection) and MAC/Finished
//!   comparisons are written without secret-dependent branches or indices.
//! * AES uses a table-based S-box and is therefore exposed to cache-timing attacks
//!   by co-located attackers. Prefer ChaCha20-Poly1305 on shared hardware.
//! * RSA private-key operations use a square-and-multiply-always exponentiation with
//!   masked selection, but no CRT and no blinding, and `Vec`-based limbs; they are
//!   slower than optimised libraries and not hardened against cache/power analysis.
//! * Field inversion uses Fermat exponentiation with a public exponent (constant time);
//!   modular helpers allocate, which is not a timing leak but costs performance.
//! * Certificate validation covers signatures, validity periods, basicConstraints/CA,
//!   keyUsage, extendedKeyUsage (serverAuth), pathLenConstraint and hostnames
//!   (SAN DNS/IP, single left-most wildcard label; CN only when no SAN extension).
//!   Not implemented: revocation (CRL/OCSP), name constraints, certificate policies
//!   (these critical extensions are tolerated, not enforced), and Ed25519/RSA-PSS-only keys.
//! * Secrets are not zeroised on drop.
//! * The randomness source is `/dev/urandom` feeding a ChaCha20 fast-key-erasure DRBG.

pub mod asn1;
pub mod certgen;
pub mod crypto;
pub mod keys;
pub mod pem;
pub mod tls;
pub mod util;
pub mod x509;

pub use certgen::{self_signed, self_signed_with_sans, SelfSigned};
pub use keys::{PrivateKey, PublicKey};
pub use tls::config::{TlsClientConfig, TlsClientConfigBuilder, TlsServerConfig, TlsServerConfigBuilder};
pub use tls::stream::TlsStream;
pub use tls::{CipherSuite, NamedGroup, TlsError, TlsVersion};
pub use x509::{CertError, Certificate, RootStore};
