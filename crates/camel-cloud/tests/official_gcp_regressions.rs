//! Hermetic regressions for the GCP emulator gaps found with gcloud / gsutil / bq
//! (`tests/official_gcp.rs`): HTTPS serving with the token audiences Google clients sign for,
//! multipart/related uploads (quoted boundaries, LF line endings, checksum verification),
//! crc32c / metadata / mediaLink in object resources and the download route, metadata
//! PATCH/PUT, delimiter listings, ranged downloads with x-goog-hash, moveTo / rewriteTo,
//! storageLayout, Pub/Sub list endpoints and BigQuery datasets.get, tabledata.list, query jobs
//! and error bodies with an `errors` list.

use camel_cloud::gcp::{access_token, ServiceAccountKey, CLOUD_PLATFORM_SCOPE};
use camel_cloud::http::{HttpClient, Proxy, Request, Response, Url};
use camel_cloud::testing::GcpMock;
use camel_core::json::parse;
use camel_core::value::Value;
use camel_tls::TlsClientConfig;

struct Api {
    base: String,
    token: String,
    http: HttpClient,
}

impl Api {
    fn new(gcp: &GcpMock) -> Api {
        let mut http = HttpClient::new().with_proxy(Proxy::None);
        if let Some(pem) = gcp.cert_pem() {
            http = http.with_tls_config(TlsClientConfig::builder().system_roots(false).add_root_pem(pem.as_bytes()).unwrap().alpn_protocols(&["http/1.1"]).build());
        }
        let key = ServiceAccountKey::parse(&gcp.service_account_json()).unwrap();
        let token = access_token(&key, CLOUD_PLATFORM_SCOPE, &http).unwrap();
        Api { base: gcp.url(), token, http }
    }
    fn call(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
        let mut req = Request::new(method, Url::parse(&format!("{}{}", self.base, path)).unwrap()).header("Authorization", format!("Bearer {}", self.token));
        for (k, v) in headers {
            req = req.header(k, *v);
        }
        req = req.header("Content-Length", body.len().to_string());
        req.body = body.to_vec();
        self.http.send(&req).unwrap()
    }
    fn json(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let r = self.call(method, path, &[("Content-Type", "application/json")], body.as_bytes());
        let v = if r.body.is_empty() { Value::Null } else { parse(&r.text()).unwrap_or(Value::Null) };
        (r.status, v)
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).map(|x| x.to_string()).unwrap_or_default()
}

#[test]
fn https_mock_accepts_google_token_audiences() {
    let gcp = GcpMock::start_tls().unwrap();
    assert!(gcp.url().starts_with("https://"));
    let api = Api::new(&gcp);
    assert_eq!(api.json("POST", "/storage/v1/b?project=test-project", r#"{"name":"tls"}"#).0, 200);
    // resumable upload sessions point back at https
    let r = api.call("POST", "/upload/storage/v1/b/tls/o?uploadType=resumable&name=r.txt", &[("Content-Type", "application/json")], b"{}");
    assert!(r.header("Location").unwrap().starts_with("https://"), "{:?}", r.header("Location"));
    // gcloud signs its JWT for Google's token endpoint even when the key names another token_uri
    let mut key = ServiceAccountKey::parse(&gcp.service_account_json()).unwrap();
    key.token_uri = "https://oauth2.googleapis.com/token".into();
    let assertion = key.jwt(CLOUD_PLATFORM_SCOPE, camel_cloud::util::now_secs()).unwrap();
    let form = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}", assertion);
    let mut req = Request::new("POST", Url::parse(&gcp.token_uri()).unwrap()).header("Content-Type", "application/x-www-form-urlencoded");
    req.body = form.into_bytes();
    let r = api.http.send(&req).unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
}

#[test]
fn storage_multipart_metadata_listing_ranges_and_moves() {
    let gcp = GcpMock::start().unwrap();
    let api = Api::new(&gcp);
    gcp.create_bucket("b");
    // multipart/related as Python's email package writes it: single-quoted boundary, LF endings
    let body = b"--==b==\nContent-Type: application/json\n\n{\"name\":\"dir/a.txt\",\"contentType\":\"text/plain\",\"crc32c\":\"yZRlqg==\",\"metadata\":{\"owner\":\"cli\"}}\n--==b==\nContent-Type: text/plain\n\nhello world\n--==b==--\n".to_vec();
    let r = api.call("POST", "/upload/storage/v1/b/b/o?uploadType=multipart", &[("Content-Type", "multipart/related; boundary=\"'==b=='\"")], &body);
    assert_eq!(r.status, 200, "{}", r.text());
    let o = parse(&r.text()).unwrap();
    assert_eq!(s(&o, "name"), "dir/a.txt");
    assert_eq!(s(&o, "size"), "11");
    assert_eq!(s(&o, "crc32c"), "yZRlqg==");
    assert_eq!(o.get("metadata").map(|m| s(m, "owner")), Some("cli".into()));
    assert!(s(&o, "mediaLink").starts_with("https://storage.googleapis.com/download/storage/v1/b/b/o/dir%2Fa.txt?generation="));
    assert_eq!(gcp.object("b", "dir/a.txt").unwrap(), b"hello world");
    // a wrong checksum is refused, with the computed value in the message
    let bad = String::from_utf8(body.clone()).unwrap().replace("yZRlqg==", "AAAAAA==").replace("dir/a.txt", "bad.txt");
    let r = api.call("POST", "/upload/storage/v1/b/b/o?uploadType=multipart", &[("Content-Type", "multipart/related; boundary='==b=='")], bad.as_bytes());
    assert_eq!(r.status, 400);
    assert!(r.text().contains("yZRlqg=="), "{}", r.text());
    assert!(gcp.object("b", "bad.txt").is_none());
    // the mediaLink download path and ranged downloads with x-goog-hash
    let r = api.call("GET", "/download/storage/v1/b/b/o/dir%2Fa.txt?alt=media", &[], b"");
    assert_eq!(r.body, b"hello world");
    assert!(r.header("x-goog-hash").unwrap().contains("crc32c=yZRlqg=="));
    let r = api.call("GET", "/storage/v1/b/b/o/dir%2Fa.txt?alt=media", &[("Range", "bytes=6-")], b"");
    assert_eq!((r.status, r.body.as_slice(), r.header("Content-Range")), (206, &b"world"[..], Some("bytes 6-10/11")));
    let r = api.call("GET", "/storage/v1/b/b/o/dir%2Fa.txt?alt=media", &[("Range", "bytes=-5")], b"");
    assert_eq!(r.body, b"world");
    assert_eq!(api.call("GET", "/storage/v1/b/b/o/dir%2Fa.txt?alt=media", &[("Range", "bytes=50-")], b"").status, 416);
    // metadata: PATCH merges (null removes), PUT replaces
    let (st, o) = api.json("PATCH", "/storage/v1/b/b/o/dir%2Fa.txt", r#"{"metadata":{"team":"x","owner":null}}"#);
    assert_eq!(st, 200);
    assert_eq!(o.get("metadata").map(|m| (s(m, "team"), m.get("owner").is_none())), Some(("x".into(), true)));
    let (_, o) = api.json("PUT", "/storage/v1/b/b/o/dir%2Fa.txt", r#"{"metadata":{"only":"1"}}"#);
    assert_eq!(o.get("metadata").map(|m| (s(m, "only"), m.get("team").is_none())), Some(("1".into(), true)));
    // delimiter listings roll names up into prefixes
    gcp.put_object("b", "top.txt", b"t");
    gcp.put_object("b", "dir/sub/c.txt", b"c");
    let (_, l) = api.json("GET", "/storage/v1/b/b/o?delimiter=%2F", "");
    assert_eq!(l.get("prefixes").map(|p| p.to_json()), Some(r#"["dir/"]"#.into()));
    assert_eq!(l.get("items").and_then(|i| i.as_array()).map(|i| i.iter().map(|o| s(o, "name")).collect::<Vec<_>>()), Some(vec!["top.txt".to_string()]));
    let (_, l) = api.json("GET", "/storage/v1/b/b/o?delimiter=%2F&prefix=dir%2F", "");
    assert_eq!(l.get("prefixes").map(|p| p.to_json()), Some(r#"["dir/sub/"]"#.into()));
    // rewriteTo copies, moveTo renames; storageLayout answers
    gcp.create_bucket("c");
    let (st, r) = api.json("POST", "/storage/v1/b/b/o/top.txt/rewriteTo/b/c/o/copy.txt", "{}");
    assert_eq!((st, r.get("done").map(|d| d.is_truthy())), (200, Some(true)));
    assert_eq!(gcp.object("c", "copy.txt").unwrap(), b"t");
    let (st, _) = api.json("POST", "/storage/v1/b/c/o/copy.txt/moveTo/o/moved.txt", "");
    assert_eq!(st, 200);
    assert_eq!(gcp.object_names("c"), vec!["moved.txt"]);
    let (st, l) = api.json("GET", "/storage/v1/b/b/storageLayout", "");
    assert_eq!((st, s(&l, "kind")), (200, "storage#storageLayout".into()));
    let (_, b) = api.json("GET", "/storage/v1/b/b", "");
    assert_eq!(s(&b, "locationType"), "region");
}

#[test]
fn pubsub_list_endpoints() {
    let gcp = GcpMock::start().unwrap();
    let api = Api::new(&gcp);
    gcp.create_topic("test-project", "t1");
    gcp.create_topic("test-project", "t2");
    gcp.create_subscription("test-project", "s1", "t1");
    let (_, v) = api.json("GET", "/v1/projects/test-project/topics?pageSize=1", "");
    assert_eq!(v.get("topics").map(|t| t.to_json()), Some(r#"[{"name":"projects/test-project/topics/t1"}]"#.into()));
    let tok = s(&v, "nextPageToken");
    let (_, v) = api.json("GET", &format!("/v1/projects/test-project/topics?pageSize=1&pageToken={}", camel_cloud::util::uri_encode(&tok, true)), "");
    assert_eq!(v.get("topics").map(|t| t.to_json()), Some(r#"[{"name":"projects/test-project/topics/t2"}]"#.into()));
    let (_, v) = api.json("GET", "/v1/projects/test-project/subscriptions", "");
    assert_eq!(v.get("subscriptions").and_then(|a| a.as_array()).map(|a| a.iter().map(|x| (s(x, "name"), s(x, "topic"))).collect::<Vec<_>>()), Some(vec![("projects/test-project/subscriptions/s1".to_string(), "projects/test-project/topics/t1".to_string())]));
    let (_, v) = api.json("GET", "/v1/projects/test-project/topics/t1/subscriptions", "");
    assert_eq!(v.get("subscriptions").map(|t| t.to_json()), Some(r#"["projects/test-project/subscriptions/s1"]"#.into()));
    assert_eq!(api.json("GET", "/v1/projects/test-project/topics/nope/subscriptions", "").0, 404);
}

#[test]
fn bigquery_datasets_get_tabledata_query_jobs_and_error_format() {
    let gcp = GcpMock::start().unwrap();
    let api = Api::new(&gcp);
    let bq = "/bigquery/v2/projects/test-project";
    assert_eq!(api.json("POST", &format!("{}/datasets", bq), r#"{"datasetReference":{"datasetId":"d"},"location":"EU"}"#).0, 200);
    let (st, d) = api.json("GET", &format!("{}/datasets/d", bq), "");
    assert_eq!((st, s(&d, "id"), s(&d, "location")), (200, "test-project:d".into(), "EU".into()));
    // errors carry an `errors` list with the reason, as bq and the client libraries read them
    let (st, e) = api.json("GET", &format!("{}/datasets/missing", bq), "");
    assert_eq!(st, 404);
    let err = e.get("error").unwrap();
    assert_eq!(s(err, "status"), "NOT_FOUND");
    assert_eq!(err.get("errors").and_then(|l| l.as_array()).map(|l| s(&l[0], "reason")), Some("notFound".into()));
    gcp.create_bq_table("test-project", "d", "p", &[("name", "STRING"), ("age", "INTEGER")]);
    let rows = r#"{"rows":[{"json":{"name":"ada","age":36}},{"json":{"name":"alan","age":41}}]}"#;
    assert_eq!(api.json("POST", &format!("{}/datasets/d/tables/p/insertAll", bq), rows).0, 200);
    let (_, l) = api.json("GET", &format!("{}/datasets/d/tables", bq), "");
    let t = &l.get("tables").and_then(|t| t.as_array()).unwrap()[0];
    assert_eq!((s(t, "id"), s(t, "type")), ("test-project:d.p".into(), "TABLE".into()));
    let (_, td) = api.json("GET", &format!("{}/datasets/d/tables/p/data?maxResults=1&startIndex=1", bq), "");
    assert_eq!(s(&td, "totalRows"), "2");
    assert_eq!(td.get("rows").map(|r| r.to_json()), Some(r#"[{"f":[{"v":"alan"},{"v":"41"}]}]"#.into()));
    // a query job (jobs.insert with configuration.query), results via getQueryResults
    let job = r#"{"jobReference":{"jobId":"q1"},"configuration":{"query":{"query":"SELECT name FROM d.p WHERE age > 40","useLegacySql":false}}}"#;
    let (st, j) = api.json("POST", &format!("{}/jobs", bq), job);
    assert_eq!(st, 200, "{}", j.to_json());
    let (_, q) = api.json("GET", &format!("{}/queries/q1", bq), "");
    assert_eq!(q.get("rows").map(|r| r.to_json()), Some(r#"[{"f":[{"v":"alan"}]}]"#.into()), "{}", q.to_json());
    let legacy = r#"{"configuration":{"query":{"query":"SELECT 1","useLegacySql":true}}}"#;
    let (_, j) = api.json("POST", &format!("{}/jobs", bq), legacy);
    let id = j.get("jobReference").map(|r| s(r, "jobId")).unwrap();
    // RUNNING once, then DONE with the error result
    api.json("GET", &format!("{}/jobs/{}", bq, id), "");
    let (_, j) = api.json("GET", &format!("{}/jobs/{}", bq, id), "");
    let status = j.get("status").unwrap();
    assert_eq!(s(status, "state"), "DONE");
    assert!(status.get("errorResult").map(|e| s(e, "message")).unwrap_or_default().contains("Legacy SQL"), "{}", j.to_json());
}
