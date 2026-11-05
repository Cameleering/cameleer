//! Third-party simulators over HTTPS: moto (15001), Azurite (Blob 20010 / Queue 20011) and
//! fake-gcs-server (14444) serve the live-interop `localhost` certificate. The cloud
//! components have no trust store option (like Camel's, which use the JVM trust store), so this
//! test binary points the process trust store (`SSL_CERT_FILE`) at the system bundle plus the
//! interop CA (`sim_aws.rs` checks that without it the server is refused unless
//! `trustAllCertificates=true`). Runs only with `CAMEL_SIMULATORS=1`.

use std::sync::Once;

use cameleer_core::prelude::*;
use cameleer_tls::testing::{live, sim};

const KEY: &str = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

fn trust_interop_ca() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut bundle = std::fs::read("/etc/ssl/certs/ca-certificates.crt").unwrap_or_default();
        bundle.extend_from_slice(b"\n");
        bundle.extend_from_slice(&std::fs::read(live::ca_file()).expect("interop CA (scripts/live-interop.sh setup)"));
        let path = std::env::temp_dir().join(format!("camel-sim-bundle-{}.pem", std::process::id()));
        std::fs::write(&path, bundle).unwrap();
        std::env::set_var("SSL_CERT_FILE", &path);
    });
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

#[test]
fn sim_tls_moto_azurite_and_fake_gcs_over_https() {
    if !sim::enabled() {
        return;
    }
    trust_interop_ca();
    let ctx = context();
    let t = ctx.create_producer_template();

    // moto over HTTPS (host name verified against the certificate's SAN)
    let aws = "accessKey=testing&secretKey=testing&region=us-east-1&uriEndpointOverride=https://localhost:15001&overrideEndpoint=true";
    let bucket = sim::unique("tls");
    t.send_body(&format!("aws2-s3://{}?operation=createBucket&{}", bucket, aws), "").unwrap();
    t.send_body_and_header(&format!("aws2-s3://{}?{}", bucket, aws), "over https", "CamelAwsS3Key", "k").unwrap();
    let ex = t.request(&format!("aws2-s3://{}?operation=getObject&{}", bucket, aws), |ex| ex.set_header("CamelAwsS3Key", "k")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "over https");
    let q = sim::unique("tls-q");
    t.send_body(&format!("aws2-sqs://{}?autoCreateQueue=true&{}", q, aws), "queued over https").unwrap();
    // the same server by IP address (SAN IP:127.0.0.1)
    let by_ip = aws.replace("localhost", "127.0.0.1");
    assert!(t.request_body(&format!("aws2-s3://{}?operation=listObjects&{}", bucket, by_ip), "").unwrap().as_array().is_some());

    // Azurite over HTTPS
    let c = sim::unique("tls").to_ascii_lowercase();
    let az = format!("accessKey=RAW({})&uriEndpointOverride=https://localhost:20010&overrideEndpoint=true", KEY);
    t.send_body(&format!("azure-storage-blob://devstoreaccount1/{}?operation=createBlobContainer&{}", c, az), "").unwrap();
    t.send_body(&format!("azure-storage-blob://devstoreaccount1/{}?operation=uploadBlockBlob&blobName=b.txt&{}", c, az), "blob over https").unwrap();
    let got = t.request_body(&format!("azure-storage-blob://devstoreaccount1/{}?operation=getBlob&blobName=b.txt&{}", c, az), "").unwrap();
    assert_eq!(got.to_string(), "blob over https");
    let azq = format!("accessKey=RAW({})&uriEndpointOverride=https://localhost:20011&overrideEndpoint=true", KEY);
    t.send_body(&format!("azure-storage-queue://devstoreaccount1/{}?createQueue=true&{}", c, azq), "message over https").unwrap();
    let peeked = t.request_body(&format!("azure-storage-queue://devstoreaccount1/{}?operation=peekMessages&{}", c, azq), "").unwrap();
    assert_eq!(peeked.as_array().unwrap()[0].get("messageText").unwrap().to_string(), "message over https");

    // fake-gcs-server over HTTPS
    std::env::set_var("GOOGLE_CLOUD_PROJECT", "camel-sim");
    let gb = sim::unique("tls").to_ascii_lowercase();
    let gcs = "uriEndpointOverride=https://localhost:14444&overrideEndpoint=true&authenticate=false";
    t.send_body_and_header(&format!("google-storage://{}?{}", gb, gcs), "object over https", "CamelGoogleCloudStorageObjectName", "o.txt").unwrap();
    let ex = t.request(&format!("google-storage://{}?operation=getObject&{}", gb, gcs), |ex| ex.set_header("CamelGoogleCloudStorageObjectName", "o.txt")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "object over https");
}
