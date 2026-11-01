//! Hermetic regressions for the Azure emulator gaps found with the Azure SDK for Python
//! (`tests/official_azure.rs`): ranged blob / file reads (206 + Content-Range),
//! `x-ms-blob-content-type`, case-preserving metadata, delimiter listings, snapshots, Blob
//! Batch, queue metadata / prefix listing / queue service SAS, container SAS, Blob API calls
//! and renames on the DFS endpoint, Key Vault secret properties / versions / deleted secrets
//! and deleted keys, OpenID discovery and HTTPS Key Vault ids.

use camel_cloud::azure::{shared_key_signature, shared_key_string_to_sign};
use camel_cloud::http::{HttpClient, Request, Response, Url};
use camel_cloud::testing::azure::{ACCOUNT, ACCOUNT_KEY};
use camel_cloud::testing::{AzureMock, AzuriteMock};
use camel_core::value::Value;

fn signed(method: &str, base: &str, path_query: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
    let url = Url::parse(&format!("{}/{}{}", base, ACCOUNT, path_query)).unwrap();
    let mut req = Request::new(method, url.clone());
    req = req.header("x-ms-date", camel_cloud::util::http_date(camel_cloud::util::now_secs())).header("x-ms-version", "2021-08-06");
    for (k, v) in headers {
        req = req.header(k, *v);
    }
    if !body.is_empty() {
        req = req.header("Content-Length", body.len().to_string());
    }
    req.body = body.to_vec();
    let sts = shared_key_string_to_sign(method, &req.headers, ACCOUNT, &url.path, url.query.as_deref().unwrap_or(""));
    let sig = shared_key_signature(ACCOUNT_KEY, &sts).unwrap();
    req = req.header("Authorization", format!("SharedKey {}:{}", ACCOUNT, sig));
    HttpClient::new().send(&req).unwrap()
}

#[test]
fn blob_ranges_content_type_metadata_listing_snapshots_and_batch() {
    let az = AzuriteMock::start().unwrap();
    let b = az.blob_url();
    assert_eq!(signed("PUT", &b, "/c?restype=container", &[], b"").status, 201);
    let r = signed("PUT", &b, "/c/dir/a.txt", &[("x-ms-blob-type", "BlockBlob"), ("Content-Type", "application/octet-stream"), ("x-ms-blob-content-type", "text/plain"), ("x-ms-meta-MixedCase", "v")], b"hello azure");
    assert_eq!(r.status, 201);
    let r = signed("GET", &b, "/c/dir/a.txt", &[("x-ms-range", "bytes=6-")], b"");
    assert_eq!(r.status, 206);
    assert_eq!(r.header("Content-Range"), Some("bytes 6-10/11"));
    assert_eq!(r.body, b"azure");
    assert_eq!(r.header("Content-Type"), Some("text/plain"), "x-ms-blob-content-type is the stored type");
    assert!(r.headers.iter().any(|(k, v)| k == "x-ms-meta-MixedCase" && v == "v"), "metadata names keep their case");
    assert_eq!(signed("GET", &b, "/c/dir/a.txt", &[("x-ms-range", "bytes=0-3")], b"").body, b"hell");
    assert_eq!(signed("GET", &b, "/c/dir/a.txt", &[("x-ms-range", "bytes=50-60")], b"").status, 416);
    signed("PUT", &b, "/c/dir/b.txt", &[("x-ms-blob-type", "BlockBlob")], b"b");
    signed("PUT", &b, "/c/top.txt", &[("x-ms-blob-type", "BlockBlob")], b"t");
    let x = signed("GET", &b, "/c?restype=container&comp=list&delimiter=%2F", &[], b"").text();
    assert!(x.contains("<BlobPrefix><Name>dir/</Name></BlobPrefix>") && x.contains("<Name>top.txt</Name>") && !x.contains("dir/a.txt"), "{}", x);
    // snapshots stay readable after the base blob changes
    let r = signed("PUT", &b, "/c/top.txt?comp=snapshot", &[], b"");
    assert_eq!(r.status, 201);
    let snap = r.header("x-ms-snapshot").unwrap().to_string();
    signed("PUT", &b, "/c/top.txt", &[("x-ms-blob-type", "BlockBlob")], b"changed");
    assert_eq!(signed("GET", &b, &format!("/c/top.txt?snapshot={}", camel_cloud::util::uri_encode(&snap, false)), &[], b"").body, b"t");
    // Blob Batch: two signed DELETE sub-requests (paths relative to the account endpoint)
    let sub = |path: &str| {
        let url = Url::parse(&format!("{}{}", b, path)).unwrap();
        let date = camel_cloud::util::http_date(camel_cloud::util::now_secs());
        let headers = vec![("x-ms-date".to_string(), date.clone()), ("Content-Length".to_string(), "0".to_string())];
        let sts = shared_key_string_to_sign("DELETE", &headers, ACCOUNT, &url.path, "");
        let sig = shared_key_signature(ACCOUNT_KEY, &sts).unwrap();
        format!("DELETE {} HTTP/1.1\r\nx-ms-date: {}\r\nAuthorization: SharedKey {}:{}\r\nContent-Length: 0\r\n\r\n", path, date, ACCOUNT, sig)
    };
    let body = format!(
        "--batch_x\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\nContent-ID: 0\r\n\r\n{}\r\n--batch_x\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\nContent-ID: 1\r\n\r\n{}\r\n--batch_x--\r\n",
        sub("/c/dir/a.txt"),
        sub("/c/dir/b.txt")
    );
    let r = signed("POST", &b, "/c?restype=container&comp=batch", &[("Content-Type", "multipart/mixed; boundary=batch_x")], body.as_bytes());
    assert_eq!(r.status, 202);
    assert!(r.header("Content-Type").unwrap().starts_with("multipart/mixed; boundary=batchresponse_"));
    assert_eq!(r.text().matches("HTTP/1.1 202").count(), 2, "{}", r.text());
    assert_eq!(az.blob_names("c"), vec!["top.txt"]);
}

#[test]
fn queue_metadata_prefix_listing_and_service_sas() {
    let az = AzuriteMock::start().unwrap();
    let q = az.queue_url();
    assert_eq!(signed("PUT", &q, "/jobs", &[("x-ms-meta-purpose", "test")], b"").status, 201);
    assert_eq!(signed("PUT", &q, "/jobs", &[("x-ms-meta-purpose", "test")], b"").status, 204, "same metadata: no-op");
    assert_eq!(signed("PUT", &q, "/jobs", &[("x-ms-meta-purpose", "other")], b"").status, 409);
    signed("PUT", &q, "/other", &[], b"");
    let r = signed("GET", &q, "/jobs?comp=metadata", &[], b"");
    assert_eq!(r.header("x-ms-meta-purpose"), Some("test"));
    assert_eq!(r.header("x-ms-approximate-messages-count"), Some("0"));
    let x = signed("GET", &q, "/?comp=list&prefix=jo&include=metadata", &[], b"").text();
    assert!(x.contains("<Name>jobs</Name>") && !x.contains("<Name>other</Name>") && x.contains("<purpose>test</purpose>"), "{}", x);
    // queue service SAS (no sr): signed over /queue/{account}/{queue}
    let exp = camel_cloud::util::iso8601(camel_cloud::util::now_secs() + 3600);
    let sts = ["ap".to_string(), String::new(), exp.clone(), format!("/queue/{}/jobs", ACCOUNT), String::new(), String::new(), String::new(), "2021-08-06".to_string()].join("\n");
    let sig = shared_key_signature(ACCOUNT_KEY, &sts).unwrap();
    let sas = format!("sv=2021-08-06&se={}&sp=ap&sig={}", camel_cloud::util::uri_encode(&exp, false), camel_cloud::util::uri_encode(&sig, false));
    let post = |queue: &str| {
        let mut req = Request::new("POST", Url::parse(&format!("{}/{}/{}/messages?{}", q, ACCOUNT, queue, sas)).unwrap()).header("x-ms-version", "2021-08-06");
        req.body = b"<QueueMessage><MessageText>via sas</MessageText></QueueMessage>".to_vec();
        HttpClient::new().send(&req).unwrap()
    };
    assert_eq!(post("jobs").status, 201);
    assert_eq!(az.queue_messages("jobs"), vec!["via sas"]);
    assert_eq!(post("other").status, 403, "the SAS is bound to its queue");
}

#[test]
fn dfs_endpoint_serves_blob_api_and_renames_and_files_ranges() {
    let az = AzuriteMock::start().unwrap();
    let d = az.dfs_url();
    // Blob API container create on the dfs endpoint
    assert_eq!(signed("PUT", &d, "/lake?restype=container", &[], b"").status, 201);
    assert_eq!(signed("PUT", &d, "/lake/raw/f.csv?resource=file", &[], b"").status, 201);
    signed("PATCH", &d, "/lake/raw/f.csv?action=append&position=0", &[], b"a,b\n");
    signed("PATCH", &d, "/lake/raw/f.csv?action=flush&position=4", &[], b"");
    let r = signed("GET", &d, "/lake/raw/f.csv", &[("x-ms-range", "bytes=0-1")], b"");
    assert_eq!((r.status, r.body.as_slice()), (206, &b"a,"[..]));
    let r = signed("PUT", &d, "/lake/raw/g.csv?mode=legacy", &[("x-ms-rename-source", "/lake/raw/f.csv")], b"");
    assert_eq!(r.status, 201);
    assert!(az.blob("lake", "raw/f.csv").is_none());
    assert_eq!(az.blob("lake", "raw/g.csv").unwrap(), b"a,b\n");
    // Azure Files ranged read
    az.put_share_file("docs", "r/q.txt", b"quarterly");
    let r = signed("GET", &az.file_url(), "/docs/r/q.txt", &[("x-ms-range", "bytes=0-6")], b"");
    assert_eq!((r.status, r.header("Content-Range")), (206, Some("bytes 0-6/9")));
    assert_eq!(r.body, b"quarter");
}

/// A Key Vault token from the emulator's client-credentials endpoint.
fn vault_token(m: &AzureMock, client: &HttpClient) -> String {
    use camel_cloud::testing::azure_services::{CLIENT_ID, CLIENT_SECRET, TENANT_ID};
    let mut req = Request::new("POST", Url::parse(&format!("{}/{}/oauth2/v2.0/token", m.url(), TENANT_ID)).unwrap()).header("Content-Type", "application/x-www-form-urlencoded");
    req.body = format!("grant_type=client_credentials&client_id={}&client_secret={}&scope=https%3A%2F%2Fvault.azure.net%2F.default", CLIENT_ID, camel_cloud::util::uri_encode(CLIENT_SECRET, false)).into_bytes();
    let v = camel_core::json::parse(&client.send(&req).unwrap().text()).unwrap();
    v.get("access_token").unwrap().to_string()
}

fn kv(m: &AzureMock, method: &str, path: &str, body: &str) -> (u16, Value) {
    let tok = vault_token(m, &HttpClient::new());
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut req = Request::new(method, Url::parse(&format!("{}{}{}api-version=7.4", m.url(), path, sep)).unwrap()).header("Authorization", format!("Bearer {}", tok)).header("Content-Type", "application/json");
    req.body = body.as_bytes().to_vec();
    let r = HttpClient::new().send(&req).unwrap();
    (r.status, camel_core::json::parse(&r.text()).unwrap_or(Value::Null))
}

#[test]
fn key_vault_secret_properties_versions_and_deleted_items() {
    let m = AzureMock::start().unwrap();
    let (_, s1) = kv(&m, "PUT", "/secrets/db", r#"{"value":"v1","contentType":"text/plain","tags":{"env":"test"}}"#);
    assert_eq!(s1.get("contentType"), Some(&Value::from("text/plain")), "{:?}", s1);
    assert_eq!(s1.get("tags").unwrap().get("env"), Some(&Value::from("test")));
    kv(&m, "PUT", "/secrets/db", r#"{"value":"v2"}"#);
    let (_, versions) = kv(&m, "GET", "/secrets/db/versions", "");
    assert_eq!(versions.get("value").unwrap().as_array().unwrap().len(), 2);
    let v1 = s1.get("id").unwrap().to_string().rsplit('/').next().unwrap().to_string();
    let (_, p) = kv(&m, "PATCH", &format!("/secrets/db/{}", v1), r#"{"tags":{"env":"prod"},"attributes":{"enabled":false}}"#);
    assert_eq!(p.get("tags").unwrap().get("env"), Some(&Value::from("prod")));
    assert_eq!(kv(&m, "GET", &format!("/secrets/db/{}", v1), "").0, 403, "disabled version");
    assert_eq!(kv(&m, "GET", "/secrets/db", "").1.get("value"), Some(&Value::from("v2")));
    let (_, list) = kv(&m, "GET", "/secrets", "");
    assert!(list.get("value").unwrap().as_array().unwrap()[0].get("value").is_none(), "list items carry no value");
    kv(&m, "DELETE", "/secrets/db", "");
    let (_, deleted) = kv(&m, "GET", "/deletedsecrets", "");
    assert_eq!(deleted.get("value").unwrap().as_array().unwrap()[0].get("recoveryId").map(|v| v.to_string().ends_with("/deletedsecrets/db")), Some(true));
    assert_eq!(kv(&m, "GET", "/deletedsecrets/db", "").0, 200);
    assert_eq!(kv(&m, "PUT", "/secrets/db", r#"{"value":"x"}"#).0, 409, "name held by the deleted secret");
    // deleted keys
    let (_, k) = kv(&m, "POST", "/keys/rsa/create", r#"{"kty":"RSA","key_size":2048}"#);
    assert!(k.get("key").is_some());
    kv(&m, "DELETE", "/keys/rsa", "");
    let (_, dk) = kv(&m, "GET", "/deletedkeys", "");
    assert!(dk.get("value").unwrap().as_array().unwrap()[0].get("kid").is_some());
    assert_eq!(kv(&m, "POST", "/deletedkeys/rsa/recover", "").0, 200);
    assert_eq!(kv(&m, "GET", "/keys/rsa", "").0, 200);
}

#[test]
fn entra_openid_discovery_and_https_key_vault_ids() {
    let m = AzureMock::start_tls().unwrap();
    let tls = camel_tls::TlsClientConfig::builder().system_roots(false).add_root_pem(m.cert_pem().unwrap().as_bytes()).unwrap().build();
    let client = HttpClient::new().with_tls_config(tls);
    let r = client.send(&Request::new("GET", Url::parse(&format!("{}/tenant-x/v2.0/.well-known/openid-configuration", m.url())).unwrap())).unwrap();
    let v = camel_core::json::parse(&r.text()).unwrap();
    assert_eq!(v.get("token_endpoint").unwrap_or_else(|| panic!("{}", r.text())).to_string(), format!("{}/tenant-x/oauth2/v2.0/token", m.url()));
    let tok = vault_token(&m, &client);
    let mut req = Request::new("PUT", Url::parse(&format!("{}/secrets/s?api-version=7.4", m.url())).unwrap()).header("Authorization", format!("Bearer {}", tok)).header("Content-Type", "application/json");
    req.body = br#"{"value":"x"}"#.to_vec();
    let v = camel_core::json::parse(&client.send(&req).unwrap().text()).unwrap();
    assert!(v.get("id").unwrap().to_string().starts_with("https://"), "{:?}", v);
}
