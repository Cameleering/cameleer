//! Azure Blob checkpoint store for `azure-eventhubs` (camel-amqp10's `CheckpointStore`), with
//! the layout of the Azure SDK's `BlobCheckpointStore`: one empty block blob per partition named
//! `<fully-qualified-namespace>/<event-hub>/<consumer-group>/checkpoint/<partition>` (lower
//! case) whose metadata carries `offset` and `sequencenumber`.
//!
//! Partition ownership (load balancing between consumer instances) uses the SDK layout too:
//! one empty block blob per partition named `<namespace>/<event-hub>/<consumer-group>/ownership/<partition>`
//! with an `ownerid` metadata entry. `list_ownership` lists the blobs of that prefix with
//! their metadata (`include=metadata`, all pages; etag and last-modified time from the blob
//! properties). `claim_ownership` writes each claimed blob conditionally — `If-None-Match: *`
//! for a new claim (etag `None`), `If-Match: <etag>` otherwise — so that only one instance
//! wins a contested partition; claims rejected with `412 ConditionNotMet` / `409
//! BlobAlreadyExists` (or `404` for a vanished blob) are left out of the result.
//!
//! [`crate::register`] binds a factory so that the Camel endpoint options `blobAccountName`,
//! `blobContainerName` and `blobAccessKey` (plus the rust-camel `blobEndpoint`, e.g. an
//! Azurite URL; `https://<account>.blob.core.windows.net` otherwise) select this store. The
//! container is created on the first checkpoint when missing.

use std::sync::{Arc, Mutex};

use camel_amqp10::eventhubs::{BlobCheckpointOptions, BlobCheckpointStoreFactory, Checkpoint, CheckpointStore, PartitionOwnership, BLOB_CHECKPOINT_STORE_FACTORY};
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};

use super::{error_code, AzureConfig, Credential};
use crate::http::{Proxy, Request};
use crate::util::{kinds, parse_http_date, uri_encode, Xml};

/// The blob checkpoint store.
pub struct BlobCheckpointStore {
    pub azure: AzureConfig,
    pub container: String,
    container_ready: Mutex<bool>,
}

impl BlobCheckpointStore {
    /// A store over an account / container with Shared Key credentials.
    pub fn new(opts: &BlobCheckpointOptions) -> Result<BlobCheckpointStore> {
        let key = opts.access_key.clone().ok_or_else(|| CamelError::illegal_argument("The blob checkpoint store needs blobAccessKey"))?;
        if crate::util::base64_decode(&key).is_none() {
            return Err(CamelError::illegal_argument("blobAccessKey must be the base64 encoded storage account key"));
        }
        Ok(BlobCheckpointStore {
            azure: AzureConfig {
                account: opts.account_name.clone(),
                credential: Credential::SharedKey(key),
                uri_endpoint_override: opts.endpoint.clone(),
                override_endpoint: opts.endpoint.is_some(),
                proxy: Proxy::Environment,
                trust_all_certificates: false,
            },
            container: opts.container_name.clone(),
            container_ready: Mutex::new(false),
        })
    }

    /// Blob name of a partition checkpoint.
    pub fn blob_name(ns: &str, hub: &str, cg: &str, partition: &str) -> String {
        format!("{}/{}/{}/checkpoint/{}", ns.to_ascii_lowercase(), hub.to_ascii_lowercase(), cg.to_ascii_lowercase(), partition)
    }

    /// Blob name of a partition ownership record.
    pub fn ownership_blob_name(ns: &str, hub: &str, cg: &str, partition: &str) -> String {
        format!("{}/{}/{}/ownership/{}", ns.to_ascii_lowercase(), hub.to_ascii_lowercase(), cg.to_ascii_lowercase(), partition)
    }

    fn url(&self, blob: &str) -> Result<crate::http::Url> {
        self.azure.url("blob", &format!("{}/{}", uri_encode(&self.container, false), uri_encode(blob, true)), "")
    }

    fn ensure_container(&self) -> Result<()> {
        let mut ready = self.container_ready.lock().unwrap();
        if *ready {
            return Ok(());
        }
        match self.azure.send(Request::new("PUT", self.azure.url("blob", &uri_encode(&self.container, false), "restype=container")?), kinds::BLOB_STORAGE) {
            Ok(_) => {}
            Err(e) if error_code(&e).as_deref() == Some("ContainerAlreadyExists") => {}
            Err(e) => return Err(e),
        }
        *ready = true;
        Ok(())
    }
}

impl CheckpointStore for BlobCheckpointStore {
    fn get(&self, ns: &str, hub: &str, cg: &str, partition: &str) -> Option<Checkpoint> {
        let req = Request::new("HEAD", self.url(&Self::blob_name(ns, hub, cg, partition)).ok()?);
        let resp = match self.azure.send(req, kinds::BLOB_STORAGE) {
            Ok(r) => r,
            Err(e) => {
                if error_code(&e).as_deref() != Some("BlobNotFound") && !e.message.contains("404") {
                    camel_core::camel_warn!("azure-eventhubs", "Cannot read checkpoint of partition {}: {}", partition, e);
                }
                return None;
            }
        };
        let offset = resp.header("x-ms-meta-offset").and_then(|v| v.trim().parse().ok())?;
        let sequence_number = resp.header("x-ms-meta-sequencenumber").and_then(|v| v.trim().parse().ok()).unwrap_or(-1);
        Some(Checkpoint { offset, sequence_number })
    }

    fn update(&self, ns: &str, hub: &str, cg: &str, partition: &str, c: Checkpoint) -> Result<()> {
        self.ensure_container()?;
        let req = Request::new("PUT", self.url(&Self::blob_name(ns, hub, cg, partition))?)
            .header("x-ms-blob-type", "BlockBlob")
            .header("Content-Type", "application/octet-stream")
            .header("x-ms-meta-offset", c.offset.to_string())
            .header("x-ms-meta-sequencenumber", c.sequence_number.to_string())
            .body(Vec::new());
        self.azure.send(req, kinds::BLOB_STORAGE).map(|_| ())
    }

    fn list_ownership(&self, ns: &str, hub: &str, cg: &str) -> Result<Vec<PartitionOwnership>> {
        let prefix = Self::ownership_blob_name(ns, hub, cg, "");
        let mut out = Vec::new();
        let mut marker = String::new();
        loop {
            let mut query = format!("restype=container&comp=list&include=metadata&prefix={}", uri_encode(&prefix, false));
            if !marker.is_empty() {
                query.push_str(&format!("&marker={}", uri_encode(&marker, false)));
            }
            let resp = match self.azure.send(Request::new("GET", self.azure.url("blob", &uri_encode(&self.container, false), &query)?), kinds::BLOB_STORAGE) {
                Ok(r) => r,
                Err(e) if error_code(&e).as_deref() == Some("ContainerNotFound") => return Ok(out),
                Err(e) => return Err(e),
            };
            let x = Xml::parse(&resp.text())?;
            for b in x.descendants("Blob") {
                let name = x.child_text(b, "Name").unwrap_or_default();
                let Some(partition) = name.strip_prefix(&prefix) else { continue };
                let props = x.child(b, "Properties");
                let prop = |k: &str| props.and_then(|p| x.child_text(p, k));
                let owner = x.child(b, "Metadata").and_then(|m| x.child_text(m, "ownerid")).unwrap_or_default();
                out.push(PartitionOwnership {
                    namespace: ns.to_string(),
                    hub: hub.to_string(),
                    consumer_group: cg.to_string(),
                    partition_id: partition.to_string(),
                    owner_id: owner,
                    last_modified: prop("Last-Modified").and_then(|d| parse_http_date(&d)).map(|s| s * 1000).unwrap_or(0),
                    etag: prop("Etag"),
                });
            }
            marker = x.find_text("NextMarker").unwrap_or_default();
            if marker.is_empty() {
                break;
            }
        }
        out.sort_by(|a, b| a.partition_id.cmp(&b.partition_id));
        Ok(out)
    }

    fn claim_ownership(&self, claims: &[PartitionOwnership]) -> Result<Vec<PartitionOwnership>> {
        if !claims.is_empty() {
            self.ensure_container()?;
        }
        let mut won = Vec::new();
        for c in claims {
            let blob = Self::ownership_blob_name(&c.namespace, &c.hub, &c.consumer_group, &c.partition_id);
            let mut req = Request::new("PUT", self.url(&blob)?).header("x-ms-blob-type", "BlockBlob").header("Content-Type", "application/octet-stream").header("x-ms-meta-ownerid", c.owner_id.clone()).body(Vec::new());
            req = match &c.etag {
                Some(e) => req.header("If-Match", e.clone()),
                None => req.header("If-None-Match", "*"),
            };
            match self.azure.send(req, kinds::BLOB_STORAGE) {
                Ok(resp) => won.push(PartitionOwnership {
                    etag: resp.header("ETag").map(|e| e.to_string()),
                    last_modified: resp.header("Last-Modified").and_then(parse_http_date).map(|s| s * 1000).unwrap_or_else(crate::util::now_millis),
                    ..c.clone()
                }),
                Err(e) if matches!(error_code(&e).as_deref(), Some("ConditionNotMet" | "BlobAlreadyExists" | "BlobNotFound")) => {
                    camel_core::camel_debug!("azure-eventhubs", "Ownership claim of partition {} by {} lost: {}", c.partition_id, c.owner_id, e);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(won)
    }
}

/// Bind the blob checkpoint store factory used by `azure-eventhubs` endpoints of this context.
pub fn register_factory(ctx: &CamelContext) {
    if ctx.registry().contains(BLOB_CHECKPOINT_STORE_FACTORY) {
        return;
    }
    let f: BlobCheckpointStoreFactory = Arc::new(|_ctx: &CamelContext, o: &BlobCheckpointOptions| Ok(Arc::new(BlobCheckpointStore::new(o)?) as Arc<dyn CheckpointStore>));
    ctx.registry().bind(BLOB_CHECKPOINT_STORE_FACTORY, f);
}
