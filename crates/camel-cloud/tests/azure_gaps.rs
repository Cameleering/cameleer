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

#[test]
fn datalake_consumer_acls_links_query_and_files() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let dl = |op: &str, extra: &str| format!("azure-storage-datalake://{}/lake?operation={}{}&{}", ACCOUNT, op, extra, az.datalake_options());
    req(&t, &dl("createFileSystem", ""), "", &[]);
    let csv = "name,age,city\nalice,34,Paris\nbob,19,Oslo\ncarol,52,Paris\n";
    req(&t, &dl("upload", "&fileName=people.csv&directoryName=data"), csv, &[]);
    // query acceleration
    let q = |expr: &str, headers: bool| s(req(&t, &dl("openQueryInputStream", "&fileName=people.csv&directoryName=data"), "", &[("CamelAzureStorageDataLakeExpression", Value::from(expr)), ("CamelAzureStorageDataLakeQueryHasHeaders", Value::Bool(headers))]).body());
    assert_eq!(q("SELECT name FROM BlobStorage WHERE city = 'Paris'", true), "alice\ncarol\n");
    assert_eq!(q("SELECT _1, _2 FROM BlobStorage WHERE _2 > 30", true), "alice,34\ncarol,52\n");
    assert_eq!(q("SELECT COUNT(*) FROM BlobStorage WHERE age >= 19 AND city != 'Paris'", true), "1\n");
    let e = call(&t, &dl("openQueryInputStream", "&fileName=people.csv&directoryName=data"), "", &[("CamelAzureStorageDataLakeExpression", Value::from("SELECT nope FROM BlobStorage"))]).unwrap_err();
    assert!(e.message.contains("Query failed") && e.message.contains("Unknown column"), "{}", e);
    req(&t, &dl("upload", "&fileName=events.json"), "{\"type\":\"a\",\"n\":1}\n{\"type\":\"b\",\"n\":2}\n", &[]);
    let out = s(req(&t, &dl("openQueryInputStream", "&fileName=events.json&expression=RAW(SELECT n FROM BlobStorage WHERE type = 'b')"), "", &[("CamelAzureStorageDataLakeQueryInputFormat", Value::from("json"))]).body());
    assert_eq!(out.trim(), "{\"n\":2}");
    // ACLs: permission + umask at creation, get / set
    req(&t, &dl("createFile", "&fileName=secret.txt&permission=0770&umask=0007"), "", &[]);
    let acl = req(&t, &dl("getAccessControl", "&fileName=secret.txt"), "", &[]).body().clone();
    assert_eq!(s(acl.get("permissions").unwrap()), "rwxrwx---");
    assert_eq!(s(acl.get("acl").unwrap()), "user::rwx,group::rwx,other::---");
    req(&t, &dl("setAccessControl", "&fileName=secret.txt"), "", &[("CamelAzureStorageDataLakeAcl", Value::from("user::rw-,group::r--,other::---,user:4a9028cf-f779-4032-b09d-970ebe3db258:r--")), ("CamelAzureStorageDataLakeOwner", Value::from("alice-oid"))]);
    let acl = req(&t, &dl("getAccessControl", "&fileName=secret.txt"), "", &[]).body().clone();
    assert_eq!(s(acl.get("permissions").unwrap()), "rw-r-----");
    assert!(s(acl.get("acl").unwrap()).ends_with("user:4a9028cf-f779-4032-b09d-970ebe3db258:r--"));
    assert_eq!(s(acl.get("owner").unwrap()), "alice-oid");
    req(&t, &dl("setAccessControl", "&directoryName=data"), "", &[("CamelAzureStorageDataLakePermission", Value::from("rwxr-x--x"))]);
    assert_eq!(s(req(&t, &dl("getAccessControl", "&directoryName=data"), "", &[]).body().get("permissions").unwrap()), "rwxr-x--x");
    assert!(call(&t, &dl("setAccessControl", "&fileName=secret.txt"), "", &[("CamelAzureStorageDataLakeAcl", Value::from("bogus"))]).unwrap_err().message.contains("InvalidAccessControlList"));
    // download link, downloadToFile, uploadFromFile
    let link = s(req(&t, &dl("downloadLink", "&fileName=people.csv&directoryName=data"), "", &[]).body());
    assert_eq!(http_get(&link), (200, csv.as_bytes().to_vec()));
    let dir = std::env::temp_dir().join(format!("camel-dl-{}", camel_cloud::util::random_hex(6)));
    let local = s(req(&t, &dl("downloadToFile", &format!("&fileName=people.csv&directoryName=data&fileDir={}", dir.display())), "", &[]).body());
    assert_eq!(std::fs::read_to_string(&local).unwrap(), csv);
    req(&t, &dl("uploadFromFile", "&fileName=copy.csv"), local.clone(), &[]);
    assert_eq!(req(&t, &dl("getFile", "&fileName=copy.csv"), "", &[]).body().to_bytes(), csv.as_bytes());
    let _ = std::fs::remove_dir_all(&dir);
    // Entra ID client secret credentials on the dfs endpoint
    let azm = AzureMock::start().unwrap();
    let aad = format!("azure-storage-datalake://{}/lake?operation=listPaths&clientId={}&clientSecret=RAW({})&tenantId={}&authorityHost={}&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, CLIENT_ID, camel_cloud::testing::azure_services::CLIENT_SECRET, TENANT_ID, azm.url(), az.dfs_url());
    assert!(req(&t, &aad, "", &[]).body().as_array().unwrap().len() >= 4);
    assert!(az.bearer_requests() >= 1);
    // consumer: files under data/ matching the regex, deleted after read
    req(&t, &dl("upload", "&fileName=extra.txt&directoryName=data"), "x", &[]);
    let from = format!("azure-storage-datalake://{}/lake?directoryName=data&regex=RAW(.*\\.csv)&deleteAfterRead=true&delay=50&initialDelay=0&{}", ACCOUNT, az.datalake_options());
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:lake");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:lake").unwrap();
    ctx.start().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while m.received_exchanges().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let rx = m.received_exchanges();
    assert_eq!(rx.len(), 1, "one csv file, delivered once (deleted after read)");
    assert_eq!(s(rx[0].header("CamelAzureStorageDataLakeFileName").unwrap()), "data/people.csv");
    assert_eq!(rx[0].body().to_bytes(), csv.as_bytes());
    ctx.stop().unwrap();
    let left: Vec<String> = req(&t, &dl("listPaths", "&directoryName=data"), "", &[]).body().as_array().unwrap().iter().map(|p| s(p.get("name").unwrap())).collect();
    assert_eq!(left, vec!["data/extra.txt"]);
}

#[test]
fn key_vault_key_cryptography() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let kv = |op: &str| format!("azure-key-vault://v?operation={}&{}", op, az.key_vault_options());
    let key = |n: &str| ("CamelAzureKeyVaultKeyName", Value::from(n));
    // RSA: encrypt / decrypt with each algorithm, wrap / unwrap, RS256 signatures
    let created = req(&t, &kv("createKey"), "", &[key("rsa"), ("CamelAzureKeyVaultKeyType", Value::from("RSA")), ("CamelAzureKeyVaultKeySize", Value::Int(2048))]).body().clone();
    assert_eq!(s(created.get("key").unwrap().get("kty").unwrap()), "RSA");
    assert!(created.get("key").unwrap().get("n").is_some());
    for alg in ["RSA-OAEP", "RSA-OAEP-256", "RSA1_5"] {
        let alg_h = ("CamelAzureKeyVaultEncryptionAlgorithm", Value::from(alg));
        let c = req(&t, &kv("encrypt"), "attack at dawn", &[key("rsa"), alg_h.clone()]).body().to_bytes();
        assert_eq!(c.len(), 256);
        assert_ne!(c, b"attack at dawn");
        let p = req(&t, &kv("decrypt"), Value::Bytes(c.clone()), &[key("rsa"), alg_h]).body().to_bytes();
        assert_eq!(p, b"attack at dawn", "{}", alg);
    }
    let c = req(&t, &kv("encrypt"), "x", &[key("rsa")]).body().to_bytes();
    assert!(call(&t, &kv("decrypt"), Value::Bytes(c), &[key("rsa"), ("CamelAzureKeyVaultEncryptionAlgorithm", Value::from("RSA-OAEP-256"))]).is_err(), "wrong padding");
    let dek: Vec<u8> = (0u8..32).collect();
    let wrapped = req(&t, &kv("wrapKey"), Value::Bytes(dek.clone()), &[key("rsa")]).body().to_bytes();
    assert_eq!(req(&t, &kv("unwrapKey"), Value::Bytes(wrapped), &[key("rsa")]).body().to_bytes(), dek);
    let sig = req(&t, &kv("signData"), "doc", &[key("rsa"), ("CamelAzureKeyVaultSignatureAlgorithm", Value::from("RS256"))]).body().to_bytes();
    assert_eq!(sig.len(), 256);
    let ok = req(&t, &kv("verifyData"), "doc", &[key("rsa"), ("CamelAzureKeyVaultSignatureAlgorithm", Value::from("RS256")), ("CamelAzureKeyVaultSignature", Value::Bytes(sig))]);
    assert_eq!(ok.body(), &Value::Bool(true));
    // symmetric (Managed HSM) keys: AES-GCM with AAD, AES key wrap
    req(&t, &kv("createKey"), "", &[key("aes"), ("CamelAzureKeyVaultKeyType", Value::from("oct-HSM")), ("CamelAzureKeyVaultKeySize", Value::Int(256))]);
    let gcm = ("CamelAzureKeyVaultEncryptionAlgorithm", Value::from("A256GCM"));
    let aad = ("CamelAzureKeyVaultAdditionalAuthenticatedData", Value::Bytes(b"header".to_vec()));
    let enc = req(&t, &kv("encrypt"), "top secret", &[key("aes"), gcm.clone(), aad.clone()]);
    let (iv, tag) = (enc.header("CamelAzureKeyVaultIv").unwrap().clone(), enc.header("CamelAzureKeyVaultAuthenticationTag").unwrap().clone());
    assert_eq!(iv.to_bytes().len(), 12);
    let cipher = enc.body().to_bytes();
    let dec = req(&t, &kv("decrypt"), Value::Bytes(cipher.clone()), &[key("aes"), gcm.clone(), aad.clone(), ("CamelAzureKeyVaultIv", iv.clone()), ("CamelAzureKeyVaultAuthenticationTag", tag.clone())]);
    assert_eq!(dec.body().to_bytes(), b"top secret");
    let e = call(&t, &kv("decrypt"), Value::Bytes(cipher), &[key("aes"), gcm, ("CamelAzureKeyVaultAdditionalAuthenticatedData", Value::Bytes(b"other".to_vec())), ("CamelAzureKeyVaultIv", iv), ("CamelAzureKeyVaultAuthenticationTag", tag)]).unwrap_err();
    assert!(e.message.contains("authentication tag"), "{}", e);
    let kw = ("CamelAzureKeyVaultEncryptionAlgorithm", Value::from("A256KW"));
    let w = req(&t, &kv("wrapKey"), Value::Bytes(dek.clone()), &[key("aes"), kw.clone()]).body().to_bytes();
    assert_eq!(w.len(), 40);
    assert_eq!(req(&t, &kv("unwrapKey"), Value::Bytes(w), &[key("aes"), kw]).body().to_bytes(), dek);
    // importKey: an oct JWK with a known key, then RFC 3394 interop
    let kek: Vec<u8> = (0u8..16).collect();
    let jwk = Value::object([("kty", Value::from("oct-HSM")), ("k", Value::from(camel_cloud::util::base64url_encode(&kek)))]);
    req(&t, &kv("importKey"), jwk, &[key("imported")]);
    let w = req(&t, &kv("wrapKey"), Value::Bytes(vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]), &[key("imported"), ("CamelAzureKeyVaultEncryptionAlgorithm", Value::from("A128KW"))]).body().to_bytes();
    assert_eq!(camel_cloud::util::hex(&w), "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    // EC keys cannot encrypt
    req(&t, &kv("createKey"), "", &[key("ec")]);
    assert!(call(&t, &kv("encrypt"), "x", &[key("ec")]).is_err());
}

#[test]
fn key_vault_secret_refresh_through_event_hubs() {
    use camel_amqp10::testing::Amqp10Broker;
    const KEY_NAME: &str = "RootManageSharedAccessKey";
    const KEY: &str = "c2VjcmV0K2tleS8xMjM=";
    let az = AzureMock::start().unwrap();
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("kv-events", 2).start().unwrap();
    let cs = format!("{};EntityPath=kv-events", b.connection_string(KEY_NAME, KEY));
    az.put_secret("db-password", "v1");
    let ctx = context();
    camel_amqp10::register(&ctx);
    let p = ctx.properties();
    p.set("camel.vault.azure.vaultName", "v");
    p.set("camel.vault.azure.tenantId", TENANT_ID);
    p.set("camel.vault.azure.clientId", CLIENT_ID);
    p.set("camel.vault.azure.clientSecret", camel_cloud::testing::azure_services::CLIENT_SECRET);
    p.set("camel.vault.azure.authorityHost", &az.url());
    p.set("camel.vault.azure.uriEndpointOverride", &az.url());
    p.set("camel.vault.azure.refreshEnabled", "true");
    p.set("camel.vault.azure.refreshPeriod", "100");
    p.set("camel.vault.azure.eventhubConnectionString", &cs);
    p.set("camel.vault.azure.secrets", "db-*");
    ctx.add_routes(|r| {
        r.from("timer:t?period=20&repeatCount=1").route_id("uses-secret").set_body(simple("{{azure:db-password}}")).to("mock:secret");
    })
    .unwrap();
    let task = camel_cloud::azure::keyvault_refresh::refresh_task(&ctx).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    task.add_reload_listener(move |names| s2.lock().unwrap().extend(names.iter().cloned()));
    ctx.start().unwrap();
    assert!(task.is_enabled());
    let m = ctx.mock_endpoint("mock:secret").unwrap();
    let wait = |f: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !f() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        f()
    };
    assert!(wait(&|| m.received_exchanges().len() == 1));
    assert_eq!(s(m.received_exchanges()[0].body()), "v1");
    // the consumer starts at the latest position: give it a moment to attach
    std::thread::sleep(std::time::Duration::from_millis(300));
    az.put_secret("db-password", "v2");
    let t = ctx.create_producer_template();
    let eh = format!("azure-eventhubs:?connectionString=RAW({})", cs);
    // an unrelated event type and a secret outside the pattern are ignored
    t.send_body(&eh, r#"[{"eventType":"Microsoft.KeyVault.SecretNearExpiry","subject":"db-password","data":{"ObjectName":"db-password"}},{"eventType":"Microsoft.KeyVault.SecretNewVersionCreated","subject":"api-key","data":{"ObjectName":"api-key"}}]"#).unwrap();
    t.send_body(&eh, r#"{"id":"1","eventType":"Microsoft.KeyVault.SecretNewVersionCreated","subject":"db-password","data":{"Id":"https://v.vault.azure.net/secrets/db-password/2","VaultName":"v","ObjectType":"Secret","ObjectName":"db-password","Version":"2"}}"#).unwrap();
    assert!(wait(&|| task.reload_count() == 1), "reloads: {}", task.reload_count());
    assert_eq!(*seen.lock().unwrap(), vec!["db-password".to_string()]);
    // the route was restarted (its timer fired again); placeholders resolve the new version
    assert!(wait(&|| m.received_exchanges().len() == 2));
    assert_eq!(ctx.properties().resolve("{{azure:db-password}}").unwrap(), "v2");
    ctx.stop().unwrap();
    assert!(!task.is_enabled());
}

#[test]
fn cosmosdb_throughput_identity_options_and_lease_container() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let c = |op: &str, extra: &str| format!("azure-cosmosdb://shop/orders?operation={}{}&{}", op, extra, az.cosmos_options());
    // throughput at creation, then replaced (manual and autoscale)
    req(&t, &c("createDatabase", ""), "", &[("CamelAzureCosmosDbThroughputProperties", Value::Int(1000))]);
    assert_eq!(req(&t, &c("readDatabaseThroughput", ""), "", &[]).body().get("content").unwrap().get("offerThroughput").and_then(|v| v.as_i64()), Some(1000));
    let offer = req(&t, &c("replaceDatabaseThroughput", ""), "", &[("CamelAzureCosmosDbThroughputProperties", Value::Int(1500))]).body().clone();
    assert_eq!(offer.get("content").unwrap().get("offerThroughput").and_then(|v| v.as_i64()), Some(1500));
    req(&t, &c("createContainer", "&containerPartitionKeyPath=/customer&throughputProperties=400"), "", &[]);
    let offer = req(&t, &c("replaceContainerThroughput", ""), "", &[("CamelAzureCosmosDbThroughputProperties", Value::object([("autoscaleMaxThroughput", 4000)]))]).body().clone();
    assert_eq!(offer.get("content").unwrap().get("offerAutopilotSettings").unwrap().get("maxThroughput").and_then(|v| v.as_i64()), Some(4000));
    assert!(call(&t, &c("replaceContainerThroughput", ""), "", &[("CamelAzureCosmosDbThroughputProperties", Value::Int(450))]).unwrap_err().message.contains("BadRequest"));
    assert!(call(&t, &c("createContainer", "&throughputProperties=50"), "", &[("CamelAzureCosmosDbContainerName", Value::from("tiny"))]).is_err());
    // request options
    let opts = "&consistencyLevel=eventual&contentResponseOnWriteEnabled=false";
    let ex = req(&t, &c("createItem", opts), Value::object([("id", "o1"), ("customer", "acme")]), &[]);
    assert!(ex.body().is_null(), "minimal response");
    assert_eq!(s(ex.header("CamelAzureCosmosDbStatusCode").unwrap()), "201");
    assert!(az.cosmos_minimal_writes() >= 1);
    assert!(az.cosmos_consistency_levels().iter().any(|c| c == "EVENTUAL"));
    // Entra ID (managed identity) instead of the account key
    let mi = format!("azure-cosmosdb://shop/orders?operation=readAllItems&databaseEndpoint={}&credentialType=AZURE_IDENTITY&managedIdentityEndpoint={}", az.url(), az.imds_endpoint());
    assert_eq!(req(&t, &mi, "", &[]).body().as_array().unwrap().len(), 1);
    assert!(az.cosmos_aad_requests() >= 1);
    assert!(az.identity_requests().iter().any(|r| r.1 == "https://cosmos.azure.com"));
    // change feed processor: two hosts share four partition key ranges through the lease container
    az.set_cosmos_partition_ranges("shop", "orders", 4);
    let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let mk = |host: &str| {
        let ctx = context();
        let from = format!("azure-cosmosdb://shop/orders?leaseContainerName=leases&createLeaseContainerIfNotExists=true&hostName={}&delay=50&initialDelay=0&leaseExpirationInterval=1500&{}", host, az.cosmos_options());
        let (seen, host) = (seen.clone(), host.to_string());
        ctx.add_routes(move |r| {
            r.from(&from).process(move |ex: &mut Exchange| {
                for d in ex.body().as_array().cloned().unwrap_or_default() {
                    seen.lock().unwrap().push((host.clone(), s(d.get("id").unwrap())));
                }
                Ok(())
            });
        })
        .unwrap();
        ctx
    };
    let owners = || -> Vec<String> { az.cosmos_items("shop", "leases").iter().map(|l| s(l.get("Owner").unwrap())).collect() };
    let wait = |f: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !f() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        f()
    };
    let h1 = mk("host-a");
    h1.start().unwrap();
    assert!(wait(&|| owners().iter().filter(|o| *o == "host-a").count() == 4), "{:?}", owners());
    let h2 = mk("host-b");
    h2.start().unwrap();
    // host-a gives back what exceeds its fair share, host-b takes it: 2 + 2
    assert!(wait(&|| { let o = owners(); o.iter().filter(|x| *x == "host-a").count() == 2 && o.iter().filter(|x| *x == "host-b").count() == 2 }), "{:?}", owners());
    std::thread::sleep(std::time::Duration::from_millis(200));
    for i in 0..12 {
        req(&t, &c("createItem", ""), Value::object([("id", Value::from(format!("n{}", i))), ("customer", Value::from(format!("c{}", i)))]), &[]);
    }
    assert!(wait(&|| seen.lock().unwrap().len() >= 12), "{:?}", seen.lock().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(300));
    {
        let s = seen.lock().unwrap();
        let mut ids: Vec<String> = s.iter().map(|x| x.1.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12);
        assert_eq!(s.len(), 12, "each change once: {:?}", s);
        assert!(s.iter().any(|x| x.0 == "host-a") && s.iter().any(|x| x.0 == "host-b"));
    }
    // continuations are checkpointed in the leases
    assert!(az.cosmos_items("shop", "leases").iter().all(|l| !l.get("ContinuationToken").unwrap().is_null()));
    // a stopped host releases its leases, the other takes over and continues from the checkpoints
    h2.stop().unwrap();
    assert!(wait(&|| owners().iter().filter(|o| *o == "host-a").count() == 4), "{:?}", owners());
    req(&t, &c("createItem", ""), Value::object([("id", "late"), ("customer", "zz")]), &[]);
    assert!(wait(&|| seen.lock().unwrap().iter().any(|x| x.1 == "late" && x.0 == "host-a")));
    assert_eq!(seen.lock().unwrap().len(), 13, "nothing re-delivered");
    h1.stop().unwrap();
}
