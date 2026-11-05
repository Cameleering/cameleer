//! Key Vault (+ `{{azure:...}}`), Cosmos DB, Data Lake Gen2 and blob append/page/lease
//! operations end-to-end against the embedded emulators (Azure AD client credentials,
//! Cosmos master-key signatures and Storage Shared Key / SAS are verified).

use std::time::Duration;

use cameleer_cloud::testing::azure::ACCOUNT;
use cameleer_cloud::testing::azure_services::{CLIENT_ID, TENANT_ID};
use cameleer_cloud::testing::{AzureMock, AzuriteMock};
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

fn s(v: &Value) -> String {
    v.to_string()
}

#[test]
fn key_vault_secret_operations() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("azure-key-vault://myvault?{}", az.key_vault_options());
    let ex = t
        .request(&format!("{}&operation=createSecret", uri), |ex| {
            ex.set_body("p@ssw0rd");
            ex.set_header("CamelAzureKeyVaultSecretName", "db-password");
        })
        .unwrap();
    let id = s(ex.body());
    let v1 = s(ex.header("CamelAzureKeyVaultSecretVersion").unwrap());
    assert!(id.ends_with(&format!("/secrets/db-password/{}", v1)), "{}", id);
    assert_eq!(az.secret("db-password").as_deref(), Some("p@ssw0rd"));
    // getSecret is the default operation
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAzureKeyVaultSecretName", "db-password").unwrap(), Value::from("p@ssw0rd"));
    // a second version; the first stays readable by version
    t.request(&format!("{}&operation=createSecret", uri), |ex| {
        ex.set_body("new-pass");
        ex.set_header("CamelAzureKeyVaultSecretName", "db-password");
    })
    .unwrap();
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAzureKeyVaultSecretName", "db-password").unwrap(), Value::from("new-pass"));
    let old = t
        .request(&uri, |ex| {
            ex.set_header("CamelAzureKeyVaultSecretName", "db-password");
            ex.set_header("CamelAzureKeyVaultSecretVersion", v1.as_str());
        })
        .unwrap();
    assert_eq!(s(old.body()), "p@ssw0rd");
    let names = t.request_body(&format!("{}&operation=listSecrets", uri), "").unwrap();
    assert_eq!(names, Value::Array(vec![Value::from("db-password")]));
    // one token for all calls (cached); a revoked token is replaced after the 401
    assert_eq!(az.token_requests(), 1);
    az.revoke_tokens();
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAzureKeyVaultSecretName", "db-password").unwrap(), Value::from("new-pass"));
    assert_eq!(az.token_requests(), 2);
    // delete (soft), recover, delete + purge
    let recovery = t.request_body_and_header(&format!("{}&operation=deleteSecret", uri), "", "CamelAzureKeyVaultSecretName", "db-password").unwrap();
    assert!(s(&recovery).ends_with("/deletedsecrets/db-password"));
    assert!(az.secret_is_deleted("db-password"));
    t.request_body_and_header(&format!("{}&operation=recoverDeletedSecret", uri), "", "CamelAzureKeyVaultSecretName", "db-password").unwrap();
    assert_eq!(az.secret("db-password").as_deref(), Some("new-pass"));
    t.request_body_and_header(&format!("{}&operation=deleteSecret", uri), "", "CamelAzureKeyVaultSecretName", "db-password").unwrap();
    t.request_body_and_header(&format!("{}&operation=purgeDeletedSecret", uri), "", "CamelAzureKeyVaultSecretName", "db-password").unwrap();
    assert!(!az.secret_is_deleted("db-password") && az.secret("db-password").is_none());
    let e = t.request_body_and_header(&uri, "", "CamelAzureKeyVaultSecretName", "db-password").unwrap_err().root_cause().clone();
    assert!(e.is_a("KeyVaultErrorException") && e.message.contains("SecretNotFound"), "{}", e);
}

#[test]
fn key_vault_rejects_bad_client_credentials() {
    let az = AzureMock::start().unwrap();
    az.put_secret("s", "v");
    let ctx = context();
    let uri = format!("azure-key-vault://v?tenantId={}&clientId={}&clientSecret=wrong&authorityHost={}&uriEndpointOverride={}&operation=getSecret", TENANT_ID, CLIENT_ID, az.url(), az.url());
    let e = ctx.create_producer_template().request_body_and_header(&uri, "", "CamelAzureKeyVaultSecretName", "s").unwrap_err().root_cause().clone();
    assert!(e.is_a("ClientAuthenticationException") && e.message.contains("Invalid client secret"), "{}", e);
    assert_eq!(az.auth_failures(), 1);
    // calling the vault without a token is refused by the emulator
    let resp = cameleer_cloud::http::HttpClient::new()
        .send(&cameleer_cloud::http::Request::new("GET", cameleer_cloud::http::Url::parse(&format!("{}/secrets/s?api-version=7.4", az.url())).unwrap()))
        .unwrap();
    assert_eq!(resp.status, 401);
    assert!(resp.header("WWW-Authenticate").is_some_and(|h| h.contains("vault.azure.net")));
    assert!(context().get_endpoint(&format!("azure-key-vault://v?clientId=x&uriEndpointOverride={}", az.url())).is_err(), "tenantId/clientSecret are required");
}

#[test]
fn azure_secrets_properties_function() {
    let az = AzureMock::start().unwrap();
    az.put_secret("database", r#"{"username":"admin","password":"secret"}"#);
    az.put_secret("token", "abc123");
    let ctx = context();
    let p = ctx.properties();
    p.set("camel.vault.azure.vaultName", "myvault");
    p.set("camel.vault.azure.tenantId", TENANT_ID);
    p.set("camel.vault.azure.clientId", CLIENT_ID);
    p.set("camel.vault.azure.clientSecret", cameleer_cloud::testing::azure_services::CLIENT_SECRET);
    p.set("camel.vault.azure.authorityHost", az.url());
    p.set("camel.vault.azure.uriEndpointOverride", az.url());
    assert_eq!(ctx.resolve_placeholders("{{azure:token}}").unwrap(), "abc123");
    assert_eq!(ctx.resolve_placeholders("{{azure:database/username}}:{{azure:database/password}}").unwrap(), "admin:secret");
    assert_eq!(ctx.resolve_placeholders("{{azure:nope:default-value}}").unwrap(), "default-value");
    assert!(ctx.resolve_placeholders("{{azure:nope}}").is_err());
    ctx.add_routes(|b| {
        b.from("direct:a").set_header("auth", simple("Bearer {{azure:token}}")).set_body(header("auth"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:a", "").unwrap(), Value::from("Bearer abc123"));
    ctx.stop().unwrap();
    p.set("camel.vault.azure.clientSecret", "bad");
    cameleer_cloud::testing::azure_services::AzureMock::revoke_tokens(&az);
    // the cached token was revoked and the new secret is wrong: the error surfaces
    let e = ctx.resolve_placeholders("{{azure:token}}").unwrap_err();
    assert!(e.message.contains("Invalid client secret") || e.message.contains("Unauthorized"), "{}", e);
}

#[test]
fn cosmosdb_databases_containers_items_and_queries() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = format!("azure-cosmosdb://shop/people?{}", az.cosmos_options());
    let op = |o: &str| format!("{}&operation={}", base, o);
    t.request_body(&op("createDatabase"), "").unwrap();
    // createDatabaseIfNotExists tolerates an existing database
    t.request_body(&format!("{}&createDatabaseIfNotExists=true", op("createDatabase")), "").unwrap();
    let e = t.request_body(&op("createDatabase"), "").unwrap_err().root_cause().clone();
    assert!(e.is_a("CosmosException") && e.message.contains("Conflict"), "{}", e);
    let ex = t.request(&op("createContainer"), |ex| ex.set_header("CamelAzureCosmosDbContainerPartitionKeyPath", "/city")).unwrap();
    assert!(ex.header("CamelAzureCosmosDbResourceId").is_some());
    // items: partition key derived from the item and the container definition
    for (id, name, city, age) in [("1", "Ada", "London", 36), ("2", "Linus", "Helsinki", 54), ("3", "Grace", "London", 85)] {
        let ex = t.request(&op("createItem"), |ex| ex.set_body(Value::object([("id", Value::from(id)), ("name", Value::from(name)), ("city", Value::from(city)), ("age", Value::Int(age))]))).unwrap();
        assert_eq!(ex.header("CamelAzureCosmosDbStatusCode").unwrap().as_i64(), Some(201));
        assert!(ex.header("CamelAzureCosmosDbEtag").is_some());
    }
    let e = t.request(&op("createItem"), |ex| ex.set_body(Value::object([("id", "1"), ("city", "London")]))).unwrap_err().root_cause().clone();
    assert!(e.message.contains("Conflict"), "{}", e);
    // readItem needs the partition key
    let item = t
        .request(&op("readItem"), |ex| {
            ex.set_header("CamelAzureCosmosDbItemId", "2");
            ex.set_header("CamelAzureCosmosDbItemPartitionKey", "Helsinki");
        })
        .unwrap();
    assert_eq!(s(item.body().get("name").unwrap()), "Linus");
    // upsert and replace
    t.request(&op("upsertItem"), |ex| ex.set_body(r#"{"id":"2","name":"Linus T.","city":"Helsinki","age":55}"#)).unwrap();
    t.request(&op("replaceItem"), |ex| ex.set_body(Value::object([("id", Value::from("3")), ("name", Value::from("Grace H.")), ("city", Value::from("London")), ("age", Value::Int(85))]))).unwrap();
    // queries with parameters
    let q = t
        .request(&op("queryItems"), |ex| {
            ex.set_header("CamelAzureCosmosDbQuery", "SELECT * FROM c WHERE c.city = @city ORDER BY c.age DESC");
            ex.set_header("CamelAzureCosmosDbQueryParameters", Value::object([("city", "London")]));
        })
        .unwrap();
    let names: Vec<String> = q.body().as_array().unwrap().iter().map(|d| s(d.get("name").unwrap())).collect();
    assert_eq!(names, vec!["Grace H.", "Ada"]);
    let older = t.request_body(&format!("{}&query=SELECT * FROM c WHERE c.age > 50", op("queryItems")), "").unwrap();
    assert_eq!(older.as_array().unwrap().len(), 2);
    let all = t.request_body(&op("readAllItems"), "").unwrap();
    assert_eq!(all.as_array().unwrap().len(), 3);
    t.request(&op("deleteItem"), |ex| {
        ex.set_header("CamelAzureCosmosDbItemId", "1");
        ex.set_header("CamelAzureCosmosDbItemPartitionKey", "London");
    })
    .unwrap();
    assert_eq!(az.cosmos_items("shop", "people").len(), 2);
    // database / container listings and queries
    let dbs = t.request_body(&op("listDatabases"), "").unwrap();
    assert_eq!(s(dbs.as_array().unwrap()[0].get("id").unwrap()), "shop");
    let found = t.request_body_and_header(&op("queryDatabases"), "", "CamelAzureCosmosDbQuery", "SELECT * FROM root r WHERE r.id = 'shop'").unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1);
    let colls = t.request_body(&op("listContainers"), "").unwrap();
    let paths = colls.as_array().unwrap()[0].get("partitionKey").unwrap().get("paths").unwrap().clone();
    assert_eq!(paths, Value::Array(vec![Value::from("/city")]));
    t.request_body(&op("deleteContainer"), "").unwrap();
    t.request_body(&op("deleteDatabase"), "").unwrap();
    assert!(az.cosmos_databases().is_empty());
    assert_eq!(az.auth_failures(), 0);
}

#[test]
fn cosmosdb_rejects_a_wrong_master_key() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let wrong = cameleer_cloud::util::base64_encode(b"definitely-not-the-account-key-0123456789");
    let uri = format!("azure-cosmosdb://db?databaseEndpoint={}&accountKey=RAW({})&operation=createDatabase", az.url(), wrong);
    let e = ctx.create_producer_template().request_body(&uri, "").unwrap_err().root_cause().clone();
    assert!(e.is_a("CosmosException") && e.message.contains("Status code 401"), "{}", e);
    assert_eq!(az.auth_failures(), 1);
    assert!(az.cosmos_databases().is_empty());
}

#[test]
fn cosmosdb_change_feed_consumer() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = az.cosmos_options();
    let base = format!("azure-cosmosdb://feeds/orders?createDatabaseIfNotExists=true&createContainerIfNotExists=true&containerPartitionKeyPath=/customer&{}", opts);
    t.request_body(&format!("{}&operation=createItem", base), Value::object([("id", "o1"), ("customer", "acme")])).unwrap();
    t.request_body(&format!("{}&operation=createItem", base), Value::object([("id", "o2"), ("customer", "globex")])).unwrap();
    let o2 = opts.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("azure-cosmosdb://feeds/orders?changeFeedStartFromBeginning=true&delay=20&initialDelay=0&{}", o2)).to("mock:changes");
    })
    .unwrap();
    let changes = ctx.mock_endpoint("mock:changes").unwrap();
    changes.expected_message_count(1);
    changes.message(0, |ex| ex.body().as_array().is_some_and(|a| a.len() == 2));
    ctx.start().unwrap();
    changes.assert_is_satisfied().unwrap();
    // updates and new items arrive (in one or more batches, depending on poll timing);
    // unchanged items are not re-sent
    changes.reset();
    t.request_body(&format!("{}&operation=createItem", base), Value::object([("id", "o3"), ("customer", "acme")])).unwrap();
    t.request_body(&format!("{}&operation=upsertItem", base), Value::object([("id", "o1"), ("customer", "acme"), ("status", "shipped")])).unwrap();
    let ids = || -> Vec<String> {
        changes
            .received_exchanges()
            .iter()
            .flat_map(|ex| ex.body().as_array().cloned().unwrap_or_default())
            .map(|d| d.get("id").unwrap().to_string())
            .collect()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ids().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(150)); // nothing else (e.g. o2) may follow
    assert_eq!(ids(), vec!["o3".to_string(), "o1".to_string()]);
    assert!(changes
        .received_exchanges()
        .iter()
        .all(|ex| ex.header("CamelAzureCosmosDbContainerName").is_some_and(|c| c.to_string() == "orders")));
    ctx.stop().unwrap();
}

#[test]
fn datalake_file_system_paths_and_files() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = format!("azure-storage-datalake://{}/lake?{}", ACCOUNT, az.datalake_options());
    let op = |o: &str| format!("{}&operation={}", base, o);
    t.request_body(&op("createFileSystem"), "").unwrap();
    assert!(az.container_exists("lake"));
    let ex = t
        .request(&op("upload"), |ex| {
            ex.set_body("hello data lake");
            ex.set_header("CamelAzureStorageDataLakeDirectoryName", "raw/2026");
            ex.set_header("CamelAzureStorageDataLakeFileName", "events.json");
        })
        .unwrap();
    assert_eq!(ex.header("CamelAzureStorageDataLakeContentLength").unwrap().as_i64(), Some(15));
    assert_eq!(az.blob("lake", "raw/2026/events.json").unwrap(), b"hello data lake");
    // append + flush
    let ex = t
        .request(&op("appendToFile"), |ex| {
            ex.set_body(" + more");
            ex.set_header("CamelAzureStorageDataLakeFileName", "raw/2026/events.json");
        })
        .unwrap();
    let pos = ex.header("CamelAzureStorageDataLakePosition").unwrap().as_i64().unwrap();
    assert_eq!(pos, 22);
    assert_eq!(az.blob("lake", "raw/2026/events.json").unwrap(), b"hello data lake", "not committed before flush");
    t.request(&op("flushToFile"), |ex| {
        ex.set_header("CamelAzureStorageDataLakeFileName", "raw/2026/events.json");
        ex.set_header("CamelAzureStorageDataLakePosition", pos);
    })
    .unwrap();
    let body = t.request_body_and_header(&op("getFile"), "", "CamelAzureStorageDataLakeFileName", "raw/2026/events.json").unwrap();
    assert_eq!(body, Value::Bytes(b"hello data lake + more".to_vec()));
    // a wrong flush position is rejected by the service
    let e = t
        .request(&op("flushToFile"), |ex| {
            ex.set_header("CamelAzureStorageDataLakeFileName", "raw/2026/events.json");
            ex.set_header("CamelAzureStorageDataLakePosition", 3);
        })
        .unwrap_err()
        .root_cause()
        .clone();
    assert!(e.is_a("DataLakeStorageException") && e.message.contains("InvalidFlushPosition"), "{}", e);
    t.request(&op("upload"), |ex| {
        ex.set_body("x");
        ex.set_header("CamelAzureStorageDataLakeFileName", "top.txt");
    })
    .unwrap();
    // listPaths: recursive and per directory
    let all = t.request_body(&format!("{}&recursive=true", op("listPaths")), "").unwrap();
    let names: Vec<String> = all.as_array().unwrap().iter().map(|p| s(p.get("name").unwrap())).collect();
    assert_eq!(names, vec!["raw", "raw/2026", "raw/2026/events.json", "top.txt"]);
    let dirs: Vec<bool> = all.as_array().unwrap().iter().map(|p| p.get("isDirectory").unwrap().as_bool().unwrap()).collect();
    assert_eq!(dirs, vec![true, true, false, false]);
    let top = t.request_body(&op("listPaths"), "").unwrap();
    assert_eq!(top.as_array().unwrap().len(), 2);
    let sub = t.request_body_and_header(&op("listPaths"), "", "CamelAzureStorageDataLakePath", "raw/2026").unwrap();
    assert_eq!(s(sub.as_array().unwrap()[0].get("contentLength").unwrap()), "22");
    let fss = t.request_body(&op("listFileSystem"), "").unwrap();
    assert_eq!(s(fss.as_array().unwrap()[0].get("name").unwrap()), "lake");
    t.request_body_and_header(&op("deleteFile"), "", "CamelAzureStorageDataLakeFileName", "top.txt").unwrap();
    t.request_body_and_header(&op("deleteDirectory"), "", "CamelAzureStorageDataLakeDirectoryName", "raw").unwrap();
    assert!(az.blob_names("lake").is_empty());
    // SAS works too, a wrong key does not
    let sas = format!("azure-storage-datalake://{}/lake?credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true&operation=listPaths", ACCOUNT, az.sas_token(), az.dfs_url());
    assert_eq!(t.request_body(&sas, "").unwrap(), Value::Array(vec![]));
    assert_eq!(az.auth_failures(), 0);
    let bad = format!("azure-storage-datalake://{}/lake?accountKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true&operation=listPaths", ACCOUNT, cameleer_cloud::util::base64_encode(b"wrong key"), az.dfs_url());
    let e = t.request_body(&bad, "").unwrap_err().root_cause().clone();
    assert!(e.message.contains("AuthenticationFailed"), "{}", e);
    assert_eq!(az.auth_failures(), 1);
    t.request_body(&op("deleteFileSystem"), "").unwrap();
    assert!(!az.container_exists("lake"));
}

#[test]
fn blob_append_page_blobs_and_leases() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("c");
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = format!("azure-storage-blob://{}/c?{}", ACCOUNT, az.blob_options());
    let op = |o: &str| format!("{}&operation={}", base, o);
    // append blob: created on first append
    for line in ["line 1\n", "line 2\n"] {
        t.request_body_and_header(&op("commitAppendBlob"), line, "CamelAzureStorageBlobBlobName", "log.txt").unwrap();
    }
    let ex = t.request(&op("commitAppendBlob"), |ex| {
        ex.set_body("line 3\n");
        ex.set_header("CamelAzureStorageBlobBlobName", "log.txt");
    })
    .unwrap();
    assert_eq!(ex.header("CamelAzureStorageBlobCommittedBlockCount").unwrap().as_i64(), Some(3));
    assert_eq!(az.blob("c", "log.txt").unwrap(), b"line 1\nline 2\nline 3\n");
    assert_eq!(az.blob_state("c", "log.txt").unwrap().0, "AppendBlob");
    let e = t.request(&format!("{}&createAppendBlob=false", op("commitAppendBlob")), |ex| {
        ex.set_body("x");
        ex.set_header("CamelAzureStorageBlobBlobName", "missing.txt");
    })
    .unwrap_err();
    assert!(e.root_cause().message.contains("BlobNotFound"), "{}", e);
    // page blob
    t.request(&op("createPageBlob"), |ex| {
        ex.set_header("CamelAzureStorageBlobBlobName", "disk.vhd");
        ex.set_header("CamelAzureStorageBlobPageBlobSize", 1024);
    })
    .unwrap();
    t.request(&op("uploadPageBlob"), |ex| {
        ex.set_body(Value::Bytes(vec![7u8; 512]));
        ex.set_header("CamelAzureStorageBlobBlobName", "disk.vhd");
        ex.set_header("CamelAzureStorageBlobPageBlobRange", "512-1023");
    })
    .unwrap();
    let ranges = t.request_body_and_header(&op("getPageBlobRanges"), "", "CamelAzureStorageBlobBlobName", "disk.vhd").unwrap();
    assert_eq!(ranges, Value::Array(vec![Value::object([("start", 512), ("end", 1023)])]));
    let data = az.blob("c", "disk.vhd").unwrap();
    assert_eq!((data.len(), data[0], data[600]), (1024, 0, 7));
    t.request(&op("resizePageBlob"), |ex| {
        ex.set_header("CamelAzureStorageBlobBlobName", "disk.vhd");
        ex.set_header("CamelAzureStorageBlobPageBlobSize", 2048);
    })
    .unwrap();
    assert_eq!(az.blob("c", "disk.vhd").unwrap().len(), 2048);
    t.request(&op("clearPageBlob"), |ex| {
        ex.set_header("CamelAzureStorageBlobBlobName", "disk.vhd");
        ex.set_header("CamelAzureStorageBlobPageBlobRange", Value::object([("start", 512), ("end", 1023)]));
    })
    .unwrap();
    assert_eq!(t.request_body_and_header(&op("getPageBlobRanges"), "", "CamelAzureStorageBlobBlobName", "disk.vhd").unwrap(), Value::Array(vec![]));
    assert!(t.request(&op("uploadPageBlob"), |ex| {
        ex.set_body(Value::Bytes(vec![1; 100]));
        ex.set_header("CamelAzureStorageBlobBlobName", "disk.vhd");
    })
    .is_err(), "unaligned pages are rejected");

    // leases
    t.request(&op("uploadBlockBlob"), |ex| {
        ex.set_body("v1");
        ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
    })
    .unwrap();
    let lease_id = t
        .request(&op("acquireBlobLease"), |ex| {
            ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
            ex.set_header("CamelAzureStorageBlobLeaseDuration", -1);
        })
        .unwrap()
        .header("CamelAzureStorageBlobLeaseId")
        .unwrap()
        .to_string();
    assert!(az.blob_state("c", "locked.txt").unwrap().1);
    let e = t
        .request(&op("uploadBlockBlob"), |ex| {
            ex.set_body("v2");
            ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
        })
        .unwrap_err()
        .root_cause()
        .clone();
    assert!(e.message.contains("LeaseIdMissing") && e.message.contains("412"), "{}", e);
    t.request(&op("uploadBlockBlob"), |ex| {
        ex.set_body("v2");
        ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
        ex.set_header("CamelAzureStorageBlobLeaseId", lease_id.as_str());
    })
    .unwrap();
    assert_eq!(az.blob("c", "locked.txt").unwrap(), b"v2");
    t.request(&op("renewBlobLease"), |ex| {
        ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
        ex.set_header("CamelAzureStorageBlobLeaseId", lease_id.as_str());
    })
    .unwrap();
    t.request(&op("releaseBlobLease"), |ex| {
        ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
        ex.set_header("CamelAzureStorageBlobLeaseId", lease_id.as_str());
    })
    .unwrap();
    assert!(!az.blob_state("c", "locked.txt").unwrap().1);
    // leaseBlob=true: the producer takes (and releases) a lease around the write
    t.request(&format!("{}&leaseBlob=true&leaseDurationInSeconds=15", op("uploadBlockBlob")), |ex| {
        ex.set_body("v3");
        ex.set_header("CamelAzureStorageBlobBlobName", "locked.txt");
    })
    .unwrap();
    assert_eq!(az.blob("c", "locked.txt").unwrap(), b"v3");
    assert!(!az.blob_state("c", "locked.txt").unwrap().1);
    // break a lease, then delete under a lease
    t.request_body_and_header(&op("acquireBlobLease"), "", "CamelAzureStorageBlobBlobName", "locked.txt").unwrap();
    t.request_body_and_header(&op("breakBlobLease"), "", "CamelAzureStorageBlobBlobName", "locked.txt").unwrap();
    t.request_body_and_header(&format!("{}&leaseBlob=true", op("deleteBlob")), "", "CamelAzureStorageBlobBlobName", "locked.txt").unwrap();
    assert!(az.blob("c", "locked.txt").is_none());
    assert_eq!(az.auth_failures(), 0);
}
