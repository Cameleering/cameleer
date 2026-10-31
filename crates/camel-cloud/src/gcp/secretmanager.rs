//! The `google-secret-manager` component (Camel `camel-google-secret-manager`, producer) over
//! the Secret Manager REST API, and the `{{gcp:...}}` properties function.
//!
//! `google-secret-manager://projectId?serviceAccountKey=...&operation=...`
//!
//! Operations (`operation` / `CamelGoogleSecretManagerOperation`, default `getSecretVersion`),
//! secret id from `CamelGoogleSecretManagerSecretId`:
//! * `createSecret`: creates the secret (automatic replication) and adds the body as its first
//!   version; body = the version name (`projects/p/secrets/s/versions/1`).
//! * `addSecretVersion` (rust-camel): adds the body as a new version.
//! * `getSecretVersion`: body = the payload of `CamelGoogleSecretManagerVersionId` (default
//!   `latest`), as a string.
//! * `deleteSecret`, `listSecrets` (body = list of secret ids).
//!
//! Properties function: `{{gcp:name}}`, `{{gcp:name/field}}` (JSON field), `{{gcp:name@version}}`,
//! `{{gcp:name:default}}`, configured with `camel.vault.gcp.serviceAccountKey` (or
//! `camel.vault.gcp.useDefaultInstance=true` = `GOOGLE_APPLICATION_CREDENTIALS`),
//! `camel.vault.gcp.projectId`, and (rust-camel) `camel.vault.gcp.uriEndpointOverride`.

use std::sync::Arc;

use camel_core::context::{CamelContext, PropertiesFunction};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::{error_status, GcpConfig, ServiceAccountKey};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::Proxy;
use crate::util::{base64_decode, base64_encode, header_str, kinds, obj, uri_encode, Opts};

/// Header names (Camel `GoogleSecretManagerConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelGoogleSecretManagerOperation";
    pub const SECRET_ID: &str = "CamelGoogleSecretManagerSecretId";
    pub const VERSION_ID: &str = "CamelGoogleSecretManagerVersionId";
}

/// A Secret Manager client.
pub struct SecretManagerService {
    pub gcp: GcpConfig,
    pub project: String,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    let project = Some(remaining.to_string()).filter(|p| !p.is_empty()).or_else(|| gcp.project_id()).ok_or_else(|| CamelError::illegal_argument("Project id must be specified: google-secret-manager://projectId"))?;
    o.ignore(&["client"]);
    Ok(Arc::new(SecretManagerService { project, operation: o.get("operation"), gcp }))
}

impl SecretManagerService {
    fn base(&self) -> String {
        format!("{}/v1/projects/{}", self.gcp.base("https://secretmanager.googleapis.com"), self.project)
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        self.gcp.json(method, &format!("{}{}", self.base(), path), body, kinds::GOOGLE_API)
    }

    /// Add a version; returns its name.
    pub fn add_version(&self, secret: &str, data: &[u8]) -> Result<String> {
        let v = self.call("POST", &format!("/secrets/{}:addVersion", uri_encode(secret, false)), Some(&obj([("payload", obj([("data", Value::from(base64_encode(data)))]))])))?;
        Ok(v.get("name").map(|n| n.to_string()).unwrap_or_default())
    }

    /// The payload of a version (`latest` by default).
    pub fn access(&self, secret: &str, version: &str) -> Result<Vec<u8>> {
        let v = self.call("GET", &format!("/secrets/{}/versions/{}:access", uri_encode(secret, false), uri_encode(version, false)), None)?;
        Ok(v.get("payload").and_then(|p| p.get("data")).and_then(|d| base64_decode(&d.to_string())).unwrap_or_default())
    }
}

impl CloudService for SecretManagerService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "getSecretVersion");
        let id = || header_str(ex, headers::SECRET_ID).ok_or_else(|| CamelError::illegal_argument("Secret id must be specified (CamelGoogleSecretManagerSecretId)"));
        match op.as_str() {
            "createSecret" => {
                let secret = id()?;
                self.call("POST", &format!("/secrets?secretId={}", uri_encode(&secret, false)), Some(&obj([("replication", obj([("automatic", obj([]))]))])))?;
                let name = self.add_version(&secret, &ex.body().to_bytes())?;
                ex.set_body(name);
            }
            "addSecretVersion" => {
                let name = self.add_version(&id()?, &ex.body().to_bytes())?;
                ex.set_body(name);
            }
            "getSecretVersion" => {
                let version = header_str(ex, headers::VERSION_ID).unwrap_or_else(|| "latest".into());
                let data = self.access(&id()?, &version)?;
                ex.set_body(String::from_utf8_lossy(&data).into_owned());
            }
            "deleteSecret" => {
                self.call("DELETE", &format!("/secrets/{}", uri_encode(&id()?, false)), None)?;
            }
            "listSecrets" => {
                let mut out = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let path = match &token {
                        Some(t) => format!("/secrets?pageToken={}", uri_encode(t, false)),
                        None => "/secrets".to_string(),
                    };
                    let v = self.call("GET", &path, None)?;
                    for s in v.get("secrets").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                        out.push(Value::from(s.get("name").map(|n| n.to_string()).unwrap_or_default().rsplit('/').next().unwrap_or("").to_string()));
                    }
                    token = v.get("nextPageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(out));
            }
            other => return Err(unsupported("google-secret-manager", other)),
        }
        Ok(())
    }
}

/// The `gcp:` properties function for a context.
pub fn properties_function(ctx: &CamelContext) -> PropertiesFunction {
    let weak = ctx.downgrade();
    Arc::new(move |r: &str| {
        let ctx = weak.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let p = ctx.properties();
        let get = |k: &str| p.get(&format!("camel.vault.gcp.{}", k)).filter(|v| !v.is_empty());
        let key_loc = get("serviceAccountKey").or_else(|| {
            get("useDefaultInstance").filter(|v| v.eq_ignore_ascii_case("true")).and_then(|_| std::env::var("GOOGLE_APPLICATION_CREDENTIALS").ok().filter(|v| !v.is_empty()))
        });
        let key = key_loc.map(|l| ServiceAccountKey::load(&l).map(Arc::new)).transpose()?;
        let project = get("projectId").or_else(|| key.as_ref().map(|k| k.project_id.clone()).filter(|p| !p.is_empty())).ok_or_else(|| CamelError::illegal_argument("camel.vault.gcp.projectId must be set to use {{gcp:...}} placeholders"))?;
        let gcp = GcpConfig { authenticate: key.is_some(), key, endpoint: get("uriEndpointOverride"), proxy: Proxy::Environment, trust_all_certificates: false, subject: None };
        let svc = SecretManagerService { gcp, project, operation: None };
        let (name, field, version) = crate::aws::secretsmanager::parse_secret_ref(r);
        match svc.access(&name, version.as_deref().unwrap_or("latest")) {
            Ok(data) => Ok(crate::aws::secretsmanager::secret_field(&String::from_utf8_lossy(&data), field.as_deref())),
            Err(e) if error_status(&e) == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    })
}
