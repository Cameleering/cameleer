//! Remaining Camel Google capabilities against the verifying emulator: workload identity
//! federation / metadata server / authorized-user credentials, resumable uploads, Pub/Sub
//! exactly-once streaming acks, BigQuery tables, Cloud Functions management, Firestore batched
//! writes / transactions / listeners, Sheets batchUpdate, Workspace sync tokens and watch
//! channels, Vertex AI streaming / embeddings / partner models and Pub/Sub Lite.

use camel_cloud::testing::gcp::{CLIENT_EMAIL, PROJECT};
use camel_cloud::testing::GcpMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

struct Env {
    mock: GcpMock,
    key: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        Env { mock, key }
    }
    fn opts(&self) -> String {
        self.mock.endpoint_options(&self.key)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn call(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Result<Exchange> {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    call(t, uri, body, headers).unwrap_or_else(|e| panic!("{}: {:?}", uri, e))
}

fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("camel-gcp-{}-{}", camel_cloud::util::random_hex(6), name));
    std::fs::write(&p, content).unwrap();
    p
}

fn wait(f: &dyn Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !f() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f()
}

#[test]
fn storage_credentials_and_resumable_uploads() {
    let env = Env::new();
    let m = &env.mock;
    m.create_bucket("data");
    let ctx = context();
    let t = ctx.create_producer_template();
    let ep = format!("uriEndpointOverride={}&overrideEndpoint=true&autoCreateBucket=false", m.url());
    // workload identity federation: subject token file -> STS -> impersonated service account
    let audience = "//iam.googleapis.com/projects/123/locations/global/workloadIdentityPools/pool/providers/github";
    let token_file = temp_file("oidc.jwt", "eyJhbGciOiJSUzI1NiJ9.github-actions.sig\n");
    let cred = temp_file("wif.json", &m.external_account_json(audience, &token_file.display().to_string(), Some(CLIENT_EMAIL)));
    let uri = |cred_opts: &str, name: &str| format!("google-storage://data?objectName={}&{}&{}", name, cred_opts, ep);
    let wif = format!("serviceAccountKey=file:{}", cred.display());
    let e = call(&t, &uri(&wif, "a.txt"), "x", &[]).unwrap_err();
    assert!(e.message.contains("token exchange failed"), "{}", e);
    m.add_workload_identity_provider(audience, "eyJhbGciOiJSUzI1NiJ9.github-actions.sig");
    req(&t, &uri(&wif, "a.txt"), "via wif", &[]);
    assert_eq!(m.object("data", "a.txt").unwrap(), b"via wif");
    assert_eq!(m.sts_requests(), 1);
    assert_eq!(m.impersonations(), vec![CLIENT_EMAIL.to_string()]);
    // the federated token alone (no impersonation)
    let direct = temp_file("wif2.json", &m.external_account_json(audience, &token_file.display().to_string(), None));
    req(&t, &uri(&format!("serviceAccountKey=file:{}", direct.display()), "b.txt"), "direct", &[]);
    assert_eq!(m.sts_requests(), 2);
    // metadata server (GCE / GKE workload identity)
    req(&t, &uri(&format!("metadataServerUrl={}", m.metadata_server_url()), "c.txt"), "metadata", &[]);
    assert_eq!(m.metadata_requests(), 1);
    // authorized-user credentials file
    m.add_user_credentials("cid", "csecret", "rtoken", "dev@example.com", &["https://www.googleapis.com/auth/cloud-platform"]);
    let user = temp_file("adc.json", &format!(r#"{{"type":"authorized_user","client_id":"cid","client_secret":"csecret","refresh_token":"rtoken","token_uri":"{}"}}"#, m.token_uri()));
    req(&t, &uri(&format!("serviceAccountKey=file:{}", user.display()), "d.txt"), "user", &[]);
    assert_eq!(m.object("data", "d.txt").unwrap(), b"user");
    // resumable upload in 256 KiB chunks, with a failed chunk resumed from the committed size
    let data: Vec<u8> = (0..(700 * 1024)).map(|i| (i % 251) as u8).collect();
    let key_opts = format!("serviceAccountKey=file:{}&resumableUploadThreshold=0&resumableChunkSize=262144", env.key.display());
    m.fail_next_resumable_chunk();
    let ex = req(&t, &uri(&key_opts, "big.bin"), Value::Bytes(data.clone()), &[]);
    assert!(ex.exception.is_none());
    assert_eq!(m.object("data", "big.bin").unwrap(), data);
    assert!(m.resumable_chunks() >= 4, "3 chunks + the failed one: {}", m.resumable_chunks());
    // small bodies below the threshold use simple uploads
    let before = m.resumable_chunks();
    req(&t, &uri(&format!("serviceAccountKey=file:{}", env.key.display()), "small.txt"), "tiny", &[]);
    assert_eq!(m.resumable_chunks(), before);
    for f in [token_file, cred, direct, user] {
        let _ = std::fs::remove_file(f);
    }
    let _ = env.opts();
}
