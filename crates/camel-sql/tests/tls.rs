//! PostgreSQL `sslmode` (SSLRequest negotiation + TLS) against the embedded server.

mod common;

use camel_sql::pg::{PgConfig, PgConnection, SslMode};
use camel_sql::testing::{PgAuth, PgTestServer};
use camel_sql::{datasource_from_url, Connection};
use camel_tls::testing;
use common::*;

fn tls_server(params: camel_tls::SslContextParameters) -> PgTestServer {
    PgTestServer::start_tls(&unique("pgtls"), PgAuth::ScramSha256, &[("camel", "s3cret")], params.server_config().unwrap()).unwrap()
}

fn url(s: &PgTestServer, host: &str, extra: &str) -> String {
    format!("jdbc:postgresql://{}:{}/{}?user=camel&password=s3cret&{}", host, s.port(), s.database(), extra)
}

#[test]
fn sslmode_require_and_verify_full() {
    let s = tls_server(testing::server_params());
    let (ca, _) = testing::write_localhost_pem_files("pg");

    // sslmode=disable: the server refuses plain sessions
    let err = PgConnection::connect(&PgConfig::from_url(&url(&s, "localhost", "sslmode=disable")).unwrap()).err().expect("plain refused");
    assert!(err.message.contains("SSL off"), "{}", err.message);

    // require (no verification), verify-ca and verify-full with the CA file
    for extra in ["sslmode=require".to_string(), format!("sslmode=verify-ca&sslrootcert={}", ca.display()), format!("sslmode=verify-full&sslrootcert={}", ca.display())] {
        let cfg = PgConfig::from_url(&url(&s, "localhost", &extra)).unwrap();
        let mut c = PgConnection::connect(&cfg).unwrap_or_else(|e| panic!("{}: {}", extra, e));
        let r = c.execute("SELECT 40 + 2 AS answer", &[]).unwrap();
        assert_eq!(r.scalar().and_then(|v| v.as_i64()), Some(42), "{}", extra);
    }

    // a pooled DataSource over TLS
    let ds = datasource_from_url(&url(&s, "127.0.0.1", &format!("ssl=true&sslrootcert={}", ca.display()))).unwrap();
    exec(&ds, "CREATE TABLE t (id INT, name VARCHAR(20))");
    exec(&ds, "INSERT INTO t VALUES (1, 'secure')");
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM t"), 1);

    // programmatic SslContextParameters
    let mut cfg = PgConfig::from_url(&url(&s, "localhost", "")).unwrap();
    cfg.ssl_mode = SslMode::VerifyFull;
    cfg.ssl_params = Some(testing::client_params());
    PgConnection::connect(&cfg).unwrap();
}

#[test]
fn sslmode_failures() {
    let s = tls_server(testing::wrong_host_server_params());
    // verify-full: the certificate is for wrong.example
    let mut cfg = PgConfig::from_url(&url(&s, "localhost", "sslmode=verify-full")).unwrap();
    cfg.ssl_params = Some(testing::client_params());
    let err = PgConnection::connect(&cfg).err().expect("hostname mismatch");
    assert!(err.chain().any(|c| c.is_a("SSLException")), "{:?}", err);
    // verify-ca accepts it (chain only)
    cfg.ssl_mode = SslMode::VerifyCa;
    PgConnection::connect(&cfg).unwrap();
    // untrusted certificate
    cfg.ssl_mode = SslMode::VerifyFull;
    cfg.ssl_params = Some(testing::untrusting_client_params());
    let err = PgConnection::connect(&cfg).err().expect("untrusted");
    assert!(err.chain().any(|c| c.is_a("SSLException")), "{:?}", err);

    // a server without TLS: require fails, prefer falls back to plain
    let plain = PgTestServer::start(&unique("pgplain"), PgAuth::Md5, &[("camel", "s3cret")]).unwrap();
    let err = PgConnection::connect(&PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=require")).unwrap()).err().expect("no ssl");
    assert!(err.message.contains("does not support SSL"), "{}", err.message);
    PgConnection::connect(&PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=prefer")).unwrap()).unwrap();
    assert!(PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=bogus")).is_err());
}
