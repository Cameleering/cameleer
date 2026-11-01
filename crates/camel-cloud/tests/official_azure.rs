//! Official-client cross-verification of the Azure emulators: the Azure SDK for Python
//! (azure-storage-blob / -queue / -file-datalake / -file-share with Shared Key and SAS,
//! azure-identity + azure-keyvault-secrets / -keys, azure-cosmos) against AzuriteMock and
//! AzureMock, plus data exchange with rust-camel's Azure Storage components.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use camel_cloud::testing::azure::ACCOUNT;
use camel_cloud::testing::{AzureMock, AzuriteMock};
use camel_core::prelude::*;
use camel_tls::testing::official::{self, python_script, Run};

fn storage(az: &AzuriteMock, checks: &[&str]) -> Run {
    let urls = [az.blob_url(), az.queue_url(), az.dfs_url(), az.file_url()];
    let mut args: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    args.extend_from_slice(checks);
    python_script("azure_storage.py", &args, &[])
}

#[test]
fn azure_sdk_storage_against_azurite_mock() {
    if !official::enabled() {
        return;
    }
    let az = AzuriteMock::start().unwrap();
    let r = storage(&az, &["blob", "queue", "datalake", "fileshare"]).ok();
    assert_eq!(r.values("ok:"), vec!["blob", "queue", "datalake", "fileshare"]);
    let expect = [
        ("blob-dup:", "ContainerAlreadyExists"),
        ("blob-download:", "hello azure"),
        ("blob-properties:", "11 text/plain [('Mixed', 'Case'), ('owner', 'sdk')] BlobType.BLOCKBLOB"),
        ("blob-range:", "azure"),
        ("blob-overwrite:", "BlobAlreadyExists"),
        ("blob-list:", "5"),
        ("blob-walk:", "['dir/', 'list/']"),
        ("blob-blocks:", "3 ABC"),
        ("blob-chunked:", "True 3"),
        ("blob-append:", "['line1', 'line2']"),
        ("blob-lease:", "StorageErrorCode.LEASE_ID_MISSING"),
        ("blob-snapshot:", "True"),
        ("blob-sas-read:", "replaced"),
        ("blob-sas-write:", "403"),
        ("blob-account-sas:", "5"),
        ("blob-account-sas-write:", "via account sas"),
        ("blob-sas-expired:", "403 StorageErrorCode.AUTHENTICATION_FAILED"),
        ("blob-bad-key:", "403 StorageErrorCode.AUTHENTICATION_FAILED"),
        ("blob-missing:", "BlobNotFound"),
        ("blob-batch-delete:", "2"),
        ("queue-peek:", "['hello queue', 'second']"),
        ("queue-receive:", "['hello queue', 'second'] [1, 1]"),
        ("queue-update:", "['updated'] True"),
        ("queue-props:", "test"),
        ("queue-binary:", "0001ff"),
        ("queue-sas:", "['via sas']"),
        ("queue-missing:", "QueueNotFound"),
        ("dfs-read:", "['a,b', '1,2']"),
        ("dfs-paths:", "['raw', 'raw/2024', 'raw/2024/data.csv']"),
        ("dfs-renamed:", "['raw/2024/renamed.csv']"),
        ("dfs-props:", "8"),
        ("file-read:", "quarterly report"),
        ("file-list:", "['q1.txt']"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
}

#[test]
fn azure_sdk_and_storage_components_exchange_data() {
    if !official::enabled() {
        return;
    }
    let az = AzuriteMock::start().unwrap();
    storage(&az, &["interop-seed"]).ok();
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let blob = format!("azure-storage-blob://{}/interop?{}", ACCOUNT, az.blob_options());
    let ex = t.request(&format!("{}&operation=getBlob", blob), |ex| ex.set_header("CamelAzureStorageBlobBlobName", "from sdk/ä b.txt")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "written by the Azure SDK");
    assert_eq!(ex.header("CamelAzureStorageBlobContentType").unwrap().to_string(), "text/plain");
    t.request(&format!("{}&operation=uploadBlockBlob&blobName=from camel/ü.txt", blob), |ex| {
        ex.set_body("written by camel");
        ex.set_header("CamelAzureStorageBlobContentType", "text/csv");
        ex.set_header("CamelAzureStorageBlobMetadata", Value::object([("origin", "camel")]));
    })
    .unwrap();
    let queue = |q: &str| format!("azure-storage-queue://{}/{}?{}", ACCOUNT, q, az.queue_options());
    let got = t.request_body(&format!("{}&operation=receiveMessages", queue("interop-in")), "").unwrap();
    assert_eq!(got.as_array().unwrap()[0].get("messageText"), Some(&Value::from("queued by the Azure SDK")));
    t.send_body(&format!("{}&createQueue=true", queue("interop-out")), "queued by camel").unwrap();
    ctx.stop().unwrap();
    let r = storage(&az, &["interop-check"]).ok();
    assert_eq!(r.value("check-blob:"), "written by camel text/csv camel");
    assert_eq!(r.value("check-queue:"), "['queued by camel']");
}

#[test]
fn azure_sdk_identity_key_vault_and_cosmos() {
    if !official::enabled() {
        return;
    }
    let m = AzureMock::start_tls().unwrap();
    let dir = official::temp_dir("azure-services");
    let ca = dir.join("azure.pem");
    std::fs::write(&ca, m.cert_pem().unwrap()).unwrap();
    let url = m.url();
    assert!(url.starts_with("https://localhost:"), "{}", url);
    let r = python_script("azure_services.py", &[&url, ca.to_str().unwrap(), "identity-bad-secret", "keyvault-secrets", "keyvault-keys", "cosmos"], &[]).ok();
    assert_eq!(r.values("ok:"), vec!["identity-bad-secret", "keyvault-secrets", "keyvault-keys", "cosmos"]);
    let expect = [
        ("identity-refused:", "True"),
        ("kv-set:", "db-password s3cr3t text/plain {'env': 'test'}"),
        ("kv-get:", "s3cr3t"),
        ("kv-version:", "rotated True"),
        ("kv-get-version:", "s3cr3t"),
        ("kv-versions:", "2"),
        ("kv-updated-tags:", "{'env': 'prod'}"),
        ("kv-after-delete:", "404"),
        ("kv-recovered:", "rotated"),
        ("kv-purged:", "[]"),
        ("kk-rsa:", "rsa-key RSA 2048"),
        ("kk-ec:", "EC P-256"),
        ("kk-decrypt:", "plaintext"),
        ("kk-verify:", "True False"),
        ("kk-unwrap:", "True"),
        ("kk-ec-verify:", "True"),
        ("kk-local-verify:", "True"),
        ("kk-local-encrypt:", "local"),
        ("cosmos-read:", "alan 41 True"),
        ("cosmos-query:", "['alan', 'ada']"),
        ("cosmos-replace:", "42"),
        ("cosmos-count:", "4"),
        ("cosmos-missing:", "404"),
        ("cosmos-conflict:", "409"),
        ("cosmos-bad-key:", "401"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
    let _ = std::fs::remove_dir_all(dir);
}
