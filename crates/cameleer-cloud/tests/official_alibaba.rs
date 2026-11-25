//! Official-client cross-verification of the Simple Log Service emulator: Alibaba Cloud's own
//! Python SDK (aliyun-log-python-sdk, checksum-verified source; signature V1 with HMAC-SHA1 and V4,
//! lz4 and uncompressed PutLogs, GetLogs over the 2020-12-30 POST API, ListLogStores) against
//! SlsMock, and the Cameleer component reading back what the SDK wrote.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`); the SDK directory
//! is `CAMEL_OFFICIAL_SLS_SDK` (default `/opt/camel-official/aliyun-log-python-sdk-0.9.53`).

use cameleer_cloud::testing::alibaba::{ACCESS_KEY_ID, ACCESS_KEY_SECRET};
use cameleer_cloud::testing::SlsMock;
use cameleer_core::prelude::*;
use cameleer_core::value::Value;
use cameleer_tls::testing::official::{self, python_script};

#[test]
fn aliyun_log_python_sdk_against_sls_mock() {
    if !official::enabled() {
        return;
    }
    let sdk = std::env::var("CAMEL_OFFICIAL_SLS_SDK").unwrap_or_else(|_| "/opt/camel-official/aliyun-log-python-sdk-0.9.53".into());
    let m = SlsMock::start().unwrap();
    m.create_project("official", &[]);
    let r = python_script("alibaba_sls.py", &[&sdk, &m.url(), ACCESS_KEY_ID, ACCESS_KEY_SECRET], &[]);
    assert_eq!(r.values("ok:"), vec!["create-v1", "put-v1-lz4", "put-v1-raw", "get-v1", "list-v1", "create-v4", "put-v4-lz4", "put-v4-raw", "get-v4", "list-v4"], "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.value("get-v1-count:"), "2");
    assert_eq!(r.value("get-v1-messages:"), "official v1 lz4 1,official v1 raw 1");
    assert_eq!(r.value("get-v4-messages:"), "official v4 lz4 1,official v4 raw 1");
    assert_eq!(r.value("list-v4:"), "2 store-v1,store-v4");
    assert_eq!(r.value("wrong-secret:"), "SignatureNotMatch");
    // what the SDK sent: both signatures, lz4 and no compression
    let seen: std::collections::BTreeSet<(String, String)> = m.requests().into_iter().filter(|(_, p, _, _)| p.ends_with("/shards/lb")).map(|(_, _, v, c)| (v, c)).collect();
    assert_eq!(seen.len(), 4, "{:?}", seen);
    let logs = m.logs("official", "store-v1");
    assert_eq!(logs.len(), 6);
    assert_eq!((logs[0].topic.as_str(), logs[0].source.as_str()), ("sdk-topic", "10.1.2.3"));
    assert_eq!(logs[0].tags, vec![("team".to_string(), "integration".to_string())]);

    // and the component reads what the SDK wrote
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    let now = cameleer_cloud::util::now_secs();
    let uri = format!("alibaba-sls:getLogs?project=official&logStoreName=store-v4&from={}&to={}&{}", now - 120, now + 120, m.endpoint_options());
    let ex = ctx.create_producer_template().request(&uri, |ex| ex.set_header("CamelAlibabaSlsQuery", "level: ERROR")).unwrap();
    let Some(Value::Array(data)) = ex.body().get("body").and_then(|b| b.get("data")).cloned() else { panic!("{:?}", ex.body()) };
    assert_eq!(data.len(), 2);
}
