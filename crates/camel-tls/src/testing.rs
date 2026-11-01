//! Test PKI for end-to-end TLS tests of components: runtime-generated self-signed
//! identities (cached per process) and ready-made [`SslContextParameters`].
//!
//! * [`server_params`] - certificate for `localhost` / `127.0.0.1` / `::1` + key, and
//!   trusts it (usable by consumers and producers alike).
//! * [`wrong_host_server_params`] - a certificate for `wrong.example` that
//!   [`server_params`] clients trust: connecting to `localhost` fails hostname verification.
//! * [`untrusting_client_params`] - trusts only an unrelated certificate, so any
//!   connection to the servers above fails chain verification.
//!
//! [`bind_test_beans`] binds them as `ssl`, `sslWrongHost` and `sslUntrusted`.
//!
//! Mutual TLS: [`mtls`] is a small CA-based PKI (a CA, a `localhost` server certificate,
//! a client certificate `CN=camel-client` and a client certificate from an unrelated
//! CA); [`mtls_server_params`], [`mtls_client_params`] and
//! [`mtls_untrusted_client_params`] build parameters from it, and [`bind_mtls_beans`]
//! binds them as `sslMtlsServer`, `sslMtlsClient`, `sslMtlsUntrusted` and
//! `sslMtlsAnonymous` (trusts the CA, no client certificate).

use crate::certgen::{issue, self_signed, CertParams, SelfSigned};
use crate::keys::PrivateKey;
use crate::params::SslContextParameters;
use crate::tls::config::ClientAuth;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The cached test identities.
pub struct TestPki {
    pub localhost: SelfSigned,
    pub wrong_host: SelfSigned,
    pub unrelated: SelfSigned,
}

/// Process-wide test identities (generated once).
pub fn pki() -> &'static TestPki {
    static PKI: OnceLock<TestPki> = OnceLock::new();
    PKI.get_or_init(|| TestPki { localhost: self_signed("localhost"), wrong_host: self_signed("wrong.example"), unrelated: self_signed("localhost") })
}

/// Key pair for `localhost`, trusting the `localhost` and `wrong.example` test certificates.
pub fn server_params() -> SslContextParameters {
    let p = pki();
    SslContextParameters::new().key_pair_pem(&p.localhost.cert_pem, &p.localhost.key_pem).trust_pem(&p.localhost.cert_pem).trust_pem(&p.wrong_host.cert_pem)
}

/// Client parameters trusting the test certificates (no key material).
pub fn client_params() -> SslContextParameters {
    let p = pki();
    SslContextParameters::new().trust_pem(&p.localhost.cert_pem).trust_pem(&p.wrong_host.cert_pem)
}

/// Key pair for `wrong.example` (trusted by [`client_params`], but not valid for `localhost`).
pub fn wrong_host_server_params() -> SslContextParameters {
    let p = pki();
    SslContextParameters::new().key_pair_pem(&p.wrong_host.cert_pem, &p.wrong_host.key_pem).trust_pem(&p.wrong_host.cert_pem)
}

/// Client parameters that trust only an unrelated certificate.
pub fn untrusting_client_params() -> SslContextParameters {
    let p = pki();
    SslContextParameters::new().key_pair_pem(&p.unrelated.cert_pem, &p.unrelated.key_pem).trust_pem(&p.unrelated.cert_pem)
}

/// Binds `ssl` ([`server_params`]), `sslWrongHost` and `sslUntrusted` in the registry.
pub fn bind_test_beans(ctx: &camel_core::context::CamelContext) {
    ctx.registry().bind("ssl", server_params());
    ctx.registry().bind("sslWrongHost", wrong_host_server_params());
    ctx.registry().bind("sslUntrusted", untrusting_client_params());
}

/// The mutual-TLS test PKI.
pub struct MtlsPki {
    pub ca: SelfSigned,
    pub server: SelfSigned,
    pub client: SelfSigned,
    pub other_ca: SelfSigned,
    /// A client certificate issued by `other_ca` (not trusted by servers).
    pub untrusted_client: SelfSigned,
}

/// Process-wide mutual-TLS identities (generated once). Client certificates use
/// Ed25519 and P-256 keys so both signature paths are exercised.
pub fn mtls() -> &'static MtlsPki {
    static PKI: OnceLock<MtlsPki> = OnceLock::new();
    PKI.get_or_init(|| {
        use crate::crypto::ec::EcCurve;
        let ca = issue(&CertParams::ca("Camel mTLS Test CA"), PrivateKey::generate_ec(EcCurve::P256), None);
        let server = issue(&CertParams::server(&["localhost", "127.0.0.1", "::1"]), PrivateKey::generate_ec(EcCurve::P256), Some(&ca));
        let client = issue(&CertParams::client("camel-client"), PrivateKey::generate_ec(EcCurve::P256), Some(&ca));
        let other_ca = issue(&CertParams::ca("Unrelated CA"), PrivateKey::generate_ed25519(), None);
        let untrusted_client = issue(&CertParams::client("intruder"), PrivateKey::generate_ed25519(), Some(&other_ca));
        MtlsPki { ca, server, client, other_ca, untrusted_client }
    })
}

/// Server parameters requesting client certificates (`mode`) trusted via the mTLS CA.
pub fn mtls_server_params(mode: ClientAuth) -> SslContextParameters {
    let p = mtls();
    SslContextParameters::new().key_pair_pem(&p.server.cert_pem, &p.server.key_pem).trust_pem(&p.ca.cert_pem).client_authentication(mode)
}

/// Client parameters with the trusted client certificate (and trusting the mTLS CA).
pub fn mtls_client_params() -> SslContextParameters {
    let p = mtls();
    SslContextParameters::new().key_pair_pem(&p.client.cert_pem, &p.client.key_pem).trust_pem(&p.ca.cert_pem)
}

/// Client parameters with a certificate from an unrelated CA.
pub fn mtls_untrusted_client_params() -> SslContextParameters {
    let p = mtls();
    SslContextParameters::new().key_pair_pem(&p.untrusted_client.cert_pem, &p.untrusted_client.key_pem).trust_pem(&p.ca.cert_pem)
}

/// Client parameters trusting the mTLS CA without a client certificate.
pub fn mtls_anonymous_client_params() -> SslContextParameters {
    SslContextParameters::new().trust_pem(&mtls().ca.cert_pem)
}

/// Binds `sslMtlsServer` (REQUIRE), `sslMtlsWant` (WANT), `sslMtlsClient`,
/// `sslMtlsUntrusted` and `sslMtlsAnonymous`.
pub fn bind_mtls_beans(ctx: &camel_core::context::CamelContext) {
    ctx.registry().bind("sslMtlsServer", mtls_server_params(ClientAuth::Need));
    ctx.registry().bind("sslMtlsWant", mtls_server_params(ClientAuth::Want));
    ctx.registry().bind("sslMtlsClient", mtls_client_params());
    ctx.registry().bind("sslMtlsUntrusted", mtls_untrusted_client_params());
    ctx.registry().bind("sslMtlsAnonymous", mtls_anonymous_client_params());
}

/// Writes the `localhost` certificate and key to PEM files in a fresh temp directory
/// and returns `(cert_path, key_path)` (for `keyStore`/`certFile` style options).
pub fn write_localhost_pem_files(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("camel-tls-test-{}-{}-{}", tag, std::process::id(), next_id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let p = pki();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, &p.localhost.cert_pem).expect("write cert");
    std::fs::write(&key, &p.localhost.key_pem).expect("write key");
    (cert, key)
}

fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Helpers for the live interop suites (`tests/live_*.rs`), which run against real servers
/// started by `scripts/live-interop.sh` and only when `CAMEL_LIVE_INTEROP=1` is set.
pub mod live {
    use crate::params::SslContextParameters;
    use std::path::PathBuf;

    /// The password of every test account the script creates.
    pub const PASSWORD: &str = "camelpw";

    /// Whether the live suites are enabled (`CAMEL_LIVE_INTEROP=1`); prints a skip notice otherwise.
    pub fn enabled() -> bool {
        let on = std::env::var("CAMEL_LIVE_INTEROP").map(|v| v == "1").unwrap_or(false);
        if !on {
            eprintln!("skipped: set CAMEL_LIVE_INTEROP=1 after `scripts/live-interop.sh start`");
        }
        on
    }

    /// The script's state directory (`CAMEL_INTEROP_DIR`, default `/var/lib/camel-interop`).
    pub fn dir() -> PathBuf {
        PathBuf::from(std::env::var("CAMEL_INTEROP_DIR").unwrap_or_else(|_| "/var/lib/camel-interop".into()))
    }

    /// The CA certificate (PEM) that signed the servers' `localhost` certificate.
    pub fn ca_file() -> PathBuf {
        dir().join("certs/ca.pem")
    }

    /// Client TLS parameters trusting only the interop CA.
    pub fn client_params() -> SslContextParameters {
        SslContextParameters::new().trust_file(ca_file())
    }

    /// Client TLS parameters with the interop client certificate (`CN=camel`) for mutual TLS.
    pub fn mtls_client_params() -> SslContextParameters {
        client_params().key_pair_files(dir().join("certs/client.pem"), dir().join("certs/client.key"))
    }

    /// Binds `liveSsl` (trusting the interop CA) and `liveUntrusted` (trusting only the
    /// unrelated embedded test certificates).
    pub fn bind_beans(ctx: &camel_core::context::CamelContext) {
        ctx.registry().bind("liveSsl", client_params());
        ctx.registry().bind("liveUntrusted", super::client_params());
    }
}

#[path = "testing_official.rs"]
pub mod official;
