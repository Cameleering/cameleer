//! cameleer-tls: a std-only TLS 1.3 (RFC 8446) and TLS 1.2 (RFC 5246, ECDHE + AEAD
//! suites only) client and server, with all cryptography implemented from scratch.
//!
//! ```no_run
//! use cameleer_tls::{TlsClientConfig, TlsServerConfig, TlsStream};
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
//! let ss = cameleer_tls::self_signed("localhost");
//! let scfg = TlsServerConfig::from_pem(ss.cert_pem.as_bytes(), ss.key_pem.as_bytes()).unwrap();
//! let listener = TcpListener::bind("127.0.0.1:8443").unwrap();
//! let (sock, _) = listener.accept().unwrap();
//! let mut conn = TlsStream::accept(sock, &scfg).unwrap();
//! conn.write_all(b"hello").unwrap();
//! conn.close().unwrap();
//! ```
//!
//! # Camel integration
//!
//! * [`net::NetStream`]: a cloneable plain-or-TLS TCP stream (full duplex: clones share one
//!   session whose read and write halves are locked independently) used by the components.
//! * [`SslContextParameters`] (Camel `SSLContextParameters`) and [`camel_support`]: registry
//!   beans (`sslContextParameters=#ssl`), global parameters (`camel.ssl.*`,
//!   `useGlobalSslContextParameters=true`), embedded server parameters (`camel.server.ssl.*`),
//!   [`camel_support::endpoint_tls`] and exception mapping (`SSLHandshakeException`...).
//! * [`keystore`]: PEM, PKCS#12, JKS and JCEKS key/trust stores (read and write).
//! * [`sasl`]: PLAIN and SCRAM-SHA-256/512 client and server.
//! * [`testing`]: a cached test PKI for end-to-end component tests (incl. a CA-based
//!   mutual-TLS PKI), [`certgen`] issues certificates and CRLs.
//!
//! # Supported parameters
//!
//! * TLS 1.3: `TLS_AES_128_GCM_SHA256`, `TLS_AES_256_GCM_SHA384`,
//!   `TLS_CHACHA20_POLY1305_SHA256`; HelloRetryRequest, KeyUpdate, middlebox
//!   compatibility mode; PSK resumption (`psk_dhe_ke` with binders, also after HRR) from
//!   NewSessionTickets. No 0-RTT early data.
//! * TLS 1.2: `ECDHE_{ECDSA,RSA}_WITH_{AES_128_GCM_SHA256,AES_256_GCM_SHA384,CHACHA20_POLY1305_SHA256}`
//!   with extended master secret (RFC 7627) and secure-renegotiation signalling (renegotiation itself is refused).
//! * Groups: x25519, secp256r1, secp384r1.
//! * TLS 1.2 session resumption: RFC 5077 session tickets and session IDs (abbreviated
//!   handshake, extended master secret preserved).
//! * Signatures: ed25519, ecdsa_secp256r1_sha256, ecdsa_secp384r1_sha384, rsa_pss_rsae_sha{256,384,512},
//!   rsa_pkcs1_sha{256,384,512} (TLS 1.2 handshakes and certificates only). Certificates may also
//!   use Ed25519, RSASSA-PSS, sha1WithRSA and ECDSA-SHA1/512 signatures; Ed25519 keys work for
//!   servers and clients in TLS 1.3 and TLS 1.2 (RFC 8422).
//! * Mutual TLS: servers request client certificates ([`ClientAuth`] `Want`/`Need`, CertificateRequest
//!   with signature_algorithms and certificate_authorities) and validate them (clientAuth EKU,
//!   CRLs); clients answer with Certificate + CertificateVerify when configured
//!   ([`TlsClientConfigBuilder::client_certificate`]).
//!
//! # Session resumption
//!
//! [`TlsClientConfig`] keeps a session cache (on by default) keyed by `host:port` for
//! [`NetStream`] connections (or an explicit key: FTPS data connections use the control
//! connection's). [`TlsServerConfig`] issues AES-256-GCM encrypted tickets (TLS 1.3 PSK
//! tickets are sent with the first application data so clients that never read do not
//! reset the connection) or, with `session_tickets(false)`, keeps a server-side cache.
//! Resumed sessions restore the peer certificates; [`TlsStream::session_resumed`] and
//! [`TlsStream::session_origin_id`] expose the outcome.
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
//!   (SAN DNS/IP, single left-most wildcard label; CN only when no SAN extension), and
//!   revocation against configured CRLs (a certificate without a matching CRL is accepted).
//!   Not implemented: OCSP (incl. stapling), name constraints, certificate policies
//!   (these critical extensions are tolerated, not enforced), and RSA-PSS-only (id-RSASSA-PSS) keys.
//! * Key stores: legacy 3DES / RC2 / MD5 / SHA-1 are implemented only to read old PKCS#12 and
//!   JCEKS files; RC2/DES use table lookups and are not constant time.
//! * Secrets are not zeroised on drop.
//! * The randomness source is `/dev/urandom` (Unix) or `BCryptGenRandom` (Windows) feeding a ChaCha20 fast-key-erasure DRBG.

pub mod asn1;
pub mod camel_support;
pub mod certgen;
pub mod crypto;
pub mod keys;
pub mod keystore;
pub mod net;
pub mod ocsp;
pub mod params;
pub mod pbe;
pub mod pem;
pub mod sasl;
pub mod stream_body;
pub mod testing;
pub mod tls;
pub mod util;
pub mod x509;

pub use certgen::{self_signed, self_signed_with_sans, SelfSigned};
pub use keys::{PrivateKey, PublicKey};
pub use tls::config::{CertifiedKey, ClientAuth, ClientExport, OcspMode, ServerExport, TlsClientConfig, TlsClientConfigBuilder, TlsServerConfig, TlsServerConfigBuilder};
pub use tls::session::ClientSessionCache;
pub use net::NetStream;
pub use params::{PemSource, SslContextParameters};
pub use tls::stream::{TlsReadHalf, TlsStream, TlsWriteHalf};
pub use tls::{CipherSuite, NamedGroup, TlsError, TlsVersion};
pub use keystore::KeyStore;
pub use x509::{CertError, Certificate, Crl, RootStore};
