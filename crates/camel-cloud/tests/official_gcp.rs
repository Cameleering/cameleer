//! Official-client cross-verification of the GCP emulator: the Google Cloud SDK CLIs (gcloud
//! storage / pubsub, gsutil, bq) against GcpMock over HTTPS with service-account (JWT bearer)
//! authentication, and grpcio as a Pub/Sub StreamingPull client of the mock's gRPC service.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::process::Command;
use std::time::Duration;

use camel_cloud::testing::GcpMock;
use camel_tls::testing::official::{self, python_script, run};

#[test]
fn google_cloud_sdk_clis_against_gcp_mock() {
    if !official::enabled() {
        return;
    }
    let gcp = GcpMock::start_tls().unwrap();
    let dir = official::temp_dir("gcp-cli");
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, gcp.cert_pem().unwrap()).unwrap();
    let sa = gcp.write_service_account_file().unwrap();
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg(official::script("gcp_cli.sh")).arg(gcp.url()).arg(&sa).arg(&ca).arg(&work).args(["storage", "gsutil", "pubsub", "bigquery"]);
    let r = run(&mut cmd, None, Duration::from_secs(900)).ok();
    assert_eq!(r.values("ok:"), vec!["storage", "gsutil", "pubsub", "bigquery"], "{}", r.stdout);
    let expect = [
        ("auth:", "activated"),
        ("storage-buckets:", "official-cli"),
        ("storage-cat:", "hello gcs"),
        ("storage-ls:", "gs://official-cli/big.bin gs://official-cli/dir/"),
        ("storage-ls-recursive:", "gs://official-cli/big.bin gs://official-cli/dir/hello.txt"),
        ("storage-roundtrip:", "same"),
        ("storage-describe:", "10\ttext/plain"),
        ("storage-metadata:", "gcloud"),
        ("storage-after-rm:", "gs://official-cli/big.bin gs://official-cli/dir/"),
        ("storage-missing:", "matched no objects"),
        ("gsutil-cat:", "via gsutil"),
        ("gsutil-ls:", "gs://official-gsutil/g.txt"),
        ("gsutil-after-rm:", "0"),
        ("pubsub-topics:", "projects/test-project/topics/official-topic"),
        ("pubsub-publish:", "1"),
        ("pubsub-pull:", "hello pubsub/gcloud second/"),
        ("pubsub-empty:", "0"),
        ("pubsub-after-delete:", "0"),
        ("bq-ls:", "people/TABLE"),
        ("bq-show:", "name:name name:age name:city"),
        ("bq-query:", "alan ada"),
        ("bq-head:", "3"),
        ("bq-after-rm:", "0"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
    assert!(r.value("gsutil-stat:").contains("Content-Length: 11"), "{}", r.stdout);
    assert!(r.value("pubsub-describe:").starts_with("30"), "{}", r.stdout);
}

#[test]
fn grpcio_streaming_pull_against_gcp_mock() {
    if !official::enabled() {
        return;
    }
    let gcp = GcpMock::start().unwrap();
    gcp.create_topic("test-project", "events");
    gcp.create_subscription("test-project", "events-sub", "events");
    for (i, k) in ["one", "two", "three"].iter().enumerate() {
        gcp.publish("test-project", "events", format!("m{}", i).as_bytes(), &[("k", k)]);
    }
    let sub = "projects/test-project/subscriptions/events-sub";
    // without an access token the stream is refused
    let r = python_script("grpc_python.py", &["pubsub", &gcp.pubsub_grpc_address(), sub, "3"], &[]).ok();
    assert!(r.value("error:").starts_with("UNAUTHENTICATED"), "{}", r.stdout);
    assert_eq!(r.value("received:"), "");
    let sa = gcp.write_service_account_file().unwrap();
    let r = python_script("grpc_python.py", &["pubsub", &gcp.pubsub_grpc_address(), sub, "3", sa.to_str().unwrap()], &[]).ok();
    assert_eq!(r.value("received:"), "m0/one,m1/two,m2/three", "{}", r.stdout);
    assert_eq!(r.value("message-ids:"), "3");
    assert_eq!(r.value("acked:"), "3");
    // the acks went over the stream: nothing left to redeliver
    std::thread::sleep(Duration::from_millis(300));
    assert!(gcp.backlog("test-project", "events-sub").is_empty());
}
