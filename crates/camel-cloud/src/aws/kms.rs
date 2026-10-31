//! The `aws2-kms` component (Camel `camel-aws2-kms`, producer) over the KMS AWS JSON 1.1
//! protocol (signing name `kms`).
//!
//! `aws2-kms://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsKMSOperation`):
//! * `listKeys` (`CamelAwsKMSLimit` page size; all pages): body = list of `{KeyId, KeyArn}`.
//! * `createKey` (`CamelAwsKMSDescription`): body = key metadata, sets `CamelAwsKMSKeyId`.
//! * `describeKey`, `enableKey`, `disableKey` (`CamelAwsKMSKeyId`), `scheduleKeyDeletion`
//!   (`CamelAwsKMSKeyId`, `CamelAwsKMSPendingWindowInDays`).
//! * rust-camel extensions `encrypt` (`CamelAwsKMSKeyId`, body bytes → ciphertext blob bytes)
//!   and `decrypt` (body = ciphertext blob → plaintext bytes; `CamelAwsKMSKeyId` set to the
//!   key used).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, header_i64, header_str, kinds, obj, Opts};

/// Header names (Camel `KMS2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsKMSOperation";
    pub const LIMIT: &str = "CamelAwsKMSLimit";
    pub const DESCRIPTION: &str = "CamelAwsKMSDescription";
    pub const KEY_ID: &str = "CamelAwsKMSKeyId";
    pub const PENDING_WINDOW_IN_DAYS: &str = "CamelAwsKMSPendingWindowInDays";
}

const API: JsonApi = JsonApi { host: "kms", service: "kms", target: "TrentService", version: "1.1", kind: kinds::KMS };

/// The KMS endpoint configuration and client.
pub struct KmsService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["kmsClient", "pojoRequest"]);
    Ok(Arc::new(KmsService { operation: o.get("operation"), aws }))
}

impl KmsService {
    /// Invoke a KMS action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.api(&API, action, &body)
    }
}

impl CloudService for KmsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let key = |ex: &Exchange| -> Result<Value> { Ok(Value::from(need(ex, headers::KEY_ID)?)) };
        match op.as_str() {
            "listKeys" => {
                let mut all = Vec::new();
                let mut marker: Option<String> = None;
                loop {
                    let mut m = ObjectMap::new();
                    put(&mut m, "Limit", header_i64(ex, headers::LIMIT));
                    put(&mut m, "Marker", marker.clone());
                    let r = self.call("ListKeys", Value::Object(m))?;
                    all.extend(r.get("Keys").and_then(|k| k.as_array()).cloned().unwrap_or_default());
                    marker = r.get("NextMarker").map(|t| t.to_string()).filter(|t| !t.is_empty() && r.get("Truncated").is_some_and(|t| t.is_truthy()));
                    if marker.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "createKey" => {
                let mut m = ObjectMap::new();
                put(&mut m, "Description", header_str(ex, headers::DESCRIPTION));
                let r = self.call("CreateKey", Value::Object(m))?;
                let md = r.get("KeyMetadata").cloned().unwrap_or_default();
                if let Some(id) = md.get("KeyId") {
                    ex.set_header(headers::KEY_ID, id.clone());
                }
                ex.set_body(md);
            }
            "describeKey" => {
                let r = self.call("DescribeKey", obj([("KeyId", key(ex)?)]))?;
                ex.set_body(r.get("KeyMetadata").cloned().unwrap_or_default());
            }
            "enableKey" => {
                self.call("EnableKey", obj([("KeyId", key(ex)?)]))?;
            }
            "disableKey" => {
                self.call("DisableKey", obj([("KeyId", key(ex)?)]))?;
            }
            "scheduleKeyDeletion" => {
                let mut m = ObjectMap::new();
                m.insert("KeyId".into(), key(ex)?);
                put(&mut m, "PendingWindowInDays", header_i64(ex, headers::PENDING_WINDOW_IN_DAYS));
                let r = self.call("ScheduleKeyDeletion", Value::Object(m))?;
                ex.set_body(r);
            }
            "encrypt" => {
                let r = self.call("Encrypt", obj([("KeyId", key(ex)?), ("Plaintext", Value::from(base64_encode(&ex.body().to_bytes())))]))?;
                let blob = r.get("CiphertextBlob").and_then(|b| base64_decode(&b.to_string())).ok_or_else(|| CamelError::illegal_state("Encrypt returned no CiphertextBlob"))?;
                ex.set_body(Value::Bytes(blob));
            }
            "decrypt" => {
                let mut m = ObjectMap::new();
                m.insert("CiphertextBlob".into(), Value::from(base64_encode(&ex.body().to_bytes())));
                put(&mut m, "KeyId", header_str(ex, headers::KEY_ID));
                let r = self.call("Decrypt", Value::Object(m))?;
                if let Some(k) = r.get("KeyId") {
                    ex.set_header(headers::KEY_ID, k.clone());
                }
                ex.set_body(Value::Bytes(r.get("Plaintext").and_then(|b| base64_decode(&b.to_string())).unwrap_or_default()));
            }
            other => return Err(unsupported("KMS", other)),
        }
        Ok(())
    }
}
