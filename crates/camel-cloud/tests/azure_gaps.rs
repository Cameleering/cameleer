//! Remaining Camel Azure capabilities against the verifying emulators (Azurite + the Entra ID /
//! Key Vault / Cosmos emulator): managed identity and workload identity federation tokens,
//! blob block lists / download links, Data Lake consumer / ACLs / query, Key Vault key
//! cryptography and secret refresh, Cosmos throughput and lease containers, Azure Files
//! FTP-like semantics and Azure OpenAI streaming / images.

use camel_cloud::testing::azure::ACCOUNT;
use camel_cloud::testing::azure_services::{CLIENT_ID, TENANT_ID};
use camel_cloud::testing::{AzureMock, AzuriteMock};
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
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
    let p = std::env::temp_dir().join(format!("camel-cloud-{}-{}", camel_cloud::util::random_hex(6), name));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn storage_and_key_vault_with_managed_and_workload_identity() {
    let az = AzureMock::start().unwrap();
    let azurite = AzuriteMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    // system-assigned managed identity through IMDS
    let mi = format!("credentialType=AZURE_IDENTITY&managedIdentityEndpoint={}", az.imds_endpoint());
    let blob = |cred: &str, op: &str| format!("azure-storage-blob://{}/mi?operation={}&blobName=a.txt&{}&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, op, cred, azurite.blob_url());
    req(&t, &blob(&mi, "createBlobContainer"), "", &[]);
    req(&t, &blob(&mi, "uploadBlockBlob"), "hello identity", &[]);
    assert_eq!(azurite.blob("mi", "a.txt").unwrap(), b"hello identity");
    let reqs = az.identity_requests();
    assert_eq!(reqs[0], ("imds".to_string(), "https://storage.azure.com".to_string(), String::new()));
    assert_eq!(reqs.len(), 1, "the token is cached");
    // user-assigned identity: unknown first, then registered
    let ua = format!("{}&managedIdentityClientId=ua-1", mi);
    let e = call(&t, &blob(&ua, "getBlob"), "", &[]).unwrap_err();
    assert!(e.message.contains("Identity not found"), "{}", e);
    az.add_managed_identity("ua-1");
    assert_eq!(req(&t, &blob(&ua, "getBlob"), "", &[]).body().to_bytes(), b"hello identity");
    // queue through the App Service identity endpoint
    let (ep, header) = az.app_service_identity();
    let app = format!("credentialType=AZURE_IDENTITY&managedIdentityEndpoint={}&managedIdentityHeader={}", ep, header);
    let q = |op: &str| format!("azure-storage-queue://{}/jobs?operation={}&{}&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, op, app, azurite.queue_url());
    req(&t, &q("createQueue"), "", &[]);
    req(&t, &q("sendMessage"), "job-1", &[]);
    let got = req(&t, &q("receiveMessages"), "", &[]).body().clone();
    assert_eq!(s(got.as_array().unwrap()[0].get("messageText").or_else(|| got.as_array().unwrap()[0].get("body")).unwrap_or(&got.as_array().unwrap()[0])).contains("job-1"), true, "{:?}", got);
    assert!(az.identity_requests().iter().any(|r| r.0 == "appservice"));
    let bad = format!("credentialType=AZURE_IDENTITY&managedIdentityEndpoint={}&managedIdentityHeader=wrong&managedIdentityClientId=ua-1", ep);
    assert!(call(&t, &format!("azure-storage-queue://{}/jobs?operation=listQueues&{}&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, bad, azurite.queue_url()), "", &[]).is_err());
    // workload identity federation: federated token file -> client_assertion
    let file = temp_file("federated.jwt", "eyJhbGciOiJSUzI1NiJ9.federated-subject.sig\n");
    let wi = format!("credentialType=AZURE_IDENTITY&tenantId={}&clientId={}&federatedTokenFile={}&authorityHost={}", TENANT_ID, CLIENT_ID, file.display(), az.url());
    let e = call(&t, &blob(&wi, "getBlob"), "", &[]).unwrap_err();
    assert!(e.message.contains("AADSTS70021"), "{}", e);
    az.add_federated_credential(TENANT_ID, CLIENT_ID, "eyJhbGciOiJSUzI1NiJ9.federated-subject.sig");
    assert_eq!(req(&t, &blob(&wi, "getBlob"), "", &[]).body().to_bytes(), b"hello identity");
    assert!(azurite.bearer_requests() >= 4);
    // a token for another audience is rejected by storage
    let forged = camel_cloud::testing::issue_entra_token("https://vault.azure.net/.default", "x", 60);
    assert!(camel_cloud::testing::verify_entra_token(&forged, "https://storage.azure.com/.default").is_none());
    // Key Vault with a managed identity
    az.put_secret("db-password", "s3cret");
    let kv = format!("azure-key-vault://v?operation=getSecret&credentialType=AZURE_IDENTITY&managedIdentityEndpoint={}&uriEndpointOverride={}&overrideEndpoint=true", az.imds_endpoint(), az.url());
    assert_eq!(s(req(&t, &kv, "", &[("CamelAzureKeyVaultSecretName", Value::from("db-password"))]).body()), "s3cret");
    assert!(az.identity_requests().iter().any(|r| r.1 == "https://vault.azure.net"));
    let _ = std::fs::remove_file(file);
}

fn http_get(url: &str) -> (u16, Vec<u8>) {
    let r = camel_cloud::http::HttpClient::new().send(&camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(url).unwrap())).unwrap();
    (r.status, r.body)
}

#[test]
fn blob_block_lists_download_links_files_and_copies() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("docs");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str, extra: &str| format!("azure-storage-blob://{}/docs?operation={}&blobName=big.txt{}&{}", ACCOUNT, op, extra, az.blob_options());
    // stage two blocks (not visible yet), list them, commit
    let ids = req(&t, &uri("stageBlockBlobList", ""), Value::Array(vec![Value::from("hello "), Value::from("world")]), &[]).body().clone();
    let ids: Vec<String> = ids.as_array().unwrap().iter().map(s).collect();
    assert_eq!(ids.len(), 2);
    assert!(az.blob("docs", "big.txt").is_none(), "staged blocks are uncommitted");
    let un = req(&t, &uri("getBlobBlockList", "&blockListType=UNCOMMITTED"), "", &[]).body().clone();
    assert_eq!(un.as_array().unwrap().len(), 2);
    assert_eq!(un.as_array().unwrap()[1].get("size").and_then(|v| v.as_i64()), Some(5));
    req(&t, &uri("commitBlobBlockList", ""), Value::from(ids.clone()), &[("CamelAzureStorageBlobContentType", Value::from("text/plain"))]);
    assert_eq!(az.blob("docs", "big.txt").unwrap(), b"hello world");
    let committed = req(&t, &uri("getBlobBlockList", ""), "", &[]).body().clone();
    assert_eq!(committed.as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect::<Vec<_>>(), ids);
    // re-commit the first committed block plus a newly staged one
    let new_ids = req(&t, &uri("stageBlockBlobList", ""), "!", &[]).body().clone();
    req(&t, &uri("commitBlobBlockList", ""), Value::from(vec![ids[0].clone(), s(&new_ids.as_array().unwrap()[0])]), &[]);
    assert_eq!(az.blob("docs", "big.txt").unwrap(), b"hello !");
    // staging with commitBlockListLater=false commits at once
    req(&t, &format!("azure-storage-blob://{}/docs?operation=stageBlockBlobList&blobName=now.txt&commitBlockListLater=false&{}", ACCOUNT, az.blob_options()), Value::Array(vec![Value::from("a"), Value::from("b")]), &[]);
    assert_eq!(az.blob("docs", "now.txt").unwrap(), b"ab");
    let e = call(&t, &uri("commitBlobBlockList", ""), Value::from(vec!["bm9wZQ=="]), &[]).unwrap_err();
    assert!(e.message.contains("InvalidBlockList"), "{}", e);
    // download link: a read-only service SAS
    let link = s(req(&t, &uri("downloadLink", "&downloadLinkExpiration=60000"), "", &[]).body());
    assert!(link.contains("sr=b") && link.contains("sp=r"), "{}", link);
    assert_eq!(http_get(&link), (200, b"hello !".to_vec()));
    let tampered = link.replace("sp=r", "sp=rw");
    assert_eq!(http_get(&tampered).0, 403);
    let put = camel_cloud::http::HttpClient::new().send(&camel_cloud::http::Request::new("PUT", camel_cloud::http::Url::parse(&link).unwrap()).header("x-ms-blob-type", "BlockBlob").body(b"x".to_vec())).unwrap();
    assert_eq!(put.status, 403, "read-only link");
    // download to a file
    let dir = std::env::temp_dir().join(format!("camel-blob-{}", camel_cloud::util::random_hex(6)));
    let path = s(req(&t, &uri("downloadBlobToFile", &format!("&fileDir={}", dir.display())), "", &[]).body());
    assert_eq!(std::fs::read(&path).unwrap(), b"hello !");
    let _ = std::fs::remove_dir_all(&dir);
    // server-side copy (same account), and from a SAS-authorized source
    az.create_container("backup");
    let ex = req(&t, &format!("azure-storage-blob://{}/backup?operation=copyBlob&blobName=copy.txt&{}", ACCOUNT, az.blob_options()), "", &[("CamelAzureStorageBlobSourceBlobContainerName", Value::from("docs")), ("CamelAzureStorageBlobSourceBlobName", Value::from("big.txt"))]);
    assert_eq!(s(ex.header("CamelAzureStorageBlobCopyStatus").unwrap()), "success");
    assert!(!s(ex.body()).is_empty());
    assert_eq!(az.blob("backup", "copy.txt").unwrap(), b"hello !");
}
