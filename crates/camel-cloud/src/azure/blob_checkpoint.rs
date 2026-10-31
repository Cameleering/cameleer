//! Azure Blob checkpoint store for `azure-eventhubs` (camel-amqp10's `CheckpointStore`), with
//! the layout of the Azure SDK's `BlobCheckpointStore`: one empty block blob per partition named
//! `<fully-qualified-namespace>/<event-hub>/<consumer-group>/checkpoint/<partition>` (lower
//! case) whose metadata carries `offset` and `sequencenumber`.
//!
//! [`crate::register`] binds a factory so that the Camel endpoint options `blobAccountName`,
//! `blobContainerName` and `blobAccessKey` (plus the rust-camel `blobEndpoint`, e.g. an
//! Azurite URL; `https://<account>.blob.core.windows.net` otherwise) select this store. The
//! container is created on the first checkpoint when missing.

use std::sync::{Arc, Mutex};

use camel_amqp10::eventhubs::{BlobCheckpointOptions, BlobCheckpointStoreFactory, Checkpoint, CheckpointStore, BLOB_CHECKPOINT_STORE_FACTORY};
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};

use super::{error_code, AzureConfig, Credential};
use crate::http::{Proxy, Request};
use crate::util::{kinds, uri_encode};

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
}

/// Bind the blob checkpoint store factory used by `azure-eventhubs` endpoints of this context.
pub fn register_factory(ctx: &CamelContext) {
    if ctx.registry().contains(BLOB_CHECKPOINT_STORE_FACTORY) {
        return;
    }
    let f: BlobCheckpointStoreFactory = Arc::new(|_ctx: &CamelContext, o: &BlobCheckpointOptions| Ok(Arc::new(BlobCheckpointStore::new(o)?) as Arc<dyn CheckpointStore>));
    ctx.registry().bind(BLOB_CHECKPOINT_STORE_FACTORY, f);
}
