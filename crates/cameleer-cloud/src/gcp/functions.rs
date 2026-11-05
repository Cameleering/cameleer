//! The `google-functions` component (Camel `camel-google-functions`, producer) over the Cloud
//! Functions v1 REST API.
//!
//! `google-functions://functionName?project=...&location=us-central1&operation=callFunction`
//!
//! Operations (`operation` / `CamelGoogleCloudFunctionsOperation`, default `callFunction`):
//! `callFunction` (body sent as the call data; body = the result, the execution id in
//! `CamelGoogleCloudFunctionsExecutionId`; a function error raises `ApiException`),
//! `listFunctions` (body = list of function names), `getFunction` (body = the function
//! resource), `deleteFunction`. The project defaults to the service account's project.
//!
//! Management (Camel's remaining operations): `generateUploadUrl` (body = the signed upload URL;
//! with a byte body the source archive is uploaded to it right away with `Content-Type:
//! application/zip` and `x-goog-content-length-range`), `generateDownloadUrl` (body = the URL of
//! the function's source), `createFunction` (`CamelGoogleCloudFunctionsEntryPoint` /
//! `entryPoint`, `CamelGoogleCloudFunctionsRuntime` / `runtime`, source =
//! `CamelGoogleCloudFunctionsSourceArchiveUrl` (`gs://`) or `CamelGoogleCloudFunctionsSourceUploadUrl`,
//! HTTPS trigger by default, or a map body = the CloudFunction resource) and `updateFunction`
//! (the same fields, `updateMask` from the given ones): the long-running operation is polled
//! (`/v1/operations/...`) until done; body = the function resource.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use super::GcpConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{cloud_error, kinds, obj, uri_encode, Opts};

/// Header names (Camel `GoogleCloudFunctionsConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelGoogleCloudFunctionsOperation";
    /// Cameleer extension: execution id of `callFunction`.
    pub const EXECUTION_ID: &str = "CamelGoogleCloudFunctionsExecutionId";
    pub const ENTRY_POINT: &str = "CamelGoogleCloudFunctionsEntryPoint";
    pub const RUNTIME: &str = "CamelGoogleCloudFunctionsRuntime";
    pub const SOURCE_ARCHIVE_URL: &str = "CamelGoogleCloudFunctionsSourceArchiveUrl";
    pub const SOURCE_UPLOAD_URL: &str = "CamelGoogleCloudFunctionsSourceUploadUrl";
    pub const RESPONSE_OBJECT: &str = "CamelGoogleCloudFunctionsResponseObject";
}

/// A Cloud Functions client.
pub struct FunctionsService {
    pub gcp: GcpConfig,
    pub function: String,
    pub project: String,
    pub location: String,
    pub operation: Option<String>,
    pub entry_point: Option<String>,
    pub runtime: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Function name must be specified: google-functions://functionName"));
    }
    let gcp = GcpConfig::from_opts(o)?;
    let project = o.get("project").or_else(|| gcp.project_id()).ok_or_else(|| CamelError::illegal_argument("The project option is required"))?;
    let location = o.string("location", "us-central1");
    o.ignore(&["client", "pojoRequest"]);
    Ok(Arc::new(FunctionsService { function: remaining.to_string(), project, location, operation: o.get("operation"), entry_point: o.get("entryPoint"), runtime: o.get("runtime"), gcp }))
}

impl FunctionsService {
    fn base(&self) -> String {
        format!("{}/v1/projects/{}/locations/{}/functions", self.gcp.base("https://cloudfunctions.googleapis.com"), self.project, self.location)
    }
}

impl CloudService for FunctionsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "callFunction");
        let one = format!("{}/{}", self.base(), uri_encode(&self.function, false));
        match op.as_str() {
            "callFunction" => {
                let data = match ex.body() {
                    v @ (Value::Object(_) | Value::Array(_)) => v.to_json(),
                    v => v.to_string(),
                };
                let r = self.gcp.json("POST", &format!("{}:call", one), Some(&obj([("data", Value::from(data))])), kinds::GOOGLE_API)?;
                ex.set_header(headers::EXECUTION_ID, r.get("executionId").cloned().unwrap_or_default());
                if let Some(e) = r.get("error").filter(|e| !e.is_null()) {
                    return Err(cloud_error(kinds::GOOGLE_API, format!("Function {} failed: {} (code: 500, status: FUNCTION_ERROR)", self.function, e)));
                }
                ex.set_body(r.get("result").cloned().unwrap_or_default());
            }
            "listFunctions" => {
                let r = self.gcp.json("GET", &self.base(), None, kinds::GOOGLE_API)?;
                let names: Vec<Value> = r.get("functions").and_then(|f| f.as_array()).cloned().unwrap_or_default().iter().map(|f| f.get("name").cloned().unwrap_or_default()).collect();
                ex.set_body(Value::Array(names));
            }
            "getFunction" => {
                let r = self.gcp.json("GET", &one, None, kinds::GOOGLE_API)?;
                ex.set_body(r);
            }
            "deleteFunction" => {
                let r = self.gcp.json("DELETE", &one, None, kinds::GOOGLE_API)?;
                ex.set_body(r);
            }
            "generateUploadUrl" => {
                let r = self.gcp.json("POST", &format!("{}:generateUploadUrl", self.base()), Some(&obj([])), kinds::GOOGLE_API)?;
                let url = r.get("uploadUrl").map(|u| u.to_string()).unwrap_or_default();
                if let Value::Bytes(zip) = ex.body() {
                    let req = crate::http::Request::new("PUT", crate::http::Url::parse(&url)?).header("Content-Type", "application/zip").header("x-goog-content-length-range", "0,104857600").body(zip.clone());
                    let resp = self.gcp.http().send(&req)?;
                    if !resp.is_success() {
                        return Err(super::google_error(kinds::GOOGLE_API, &resp));
                    }
                }
                ex.set_header(headers::SOURCE_UPLOAD_URL, url.clone());
                ex.set_body(url);
            }
            "generateDownloadUrl" => {
                let r = self.gcp.json("POST", &format!("{}:generateDownloadUrl", one), Some(&obj([])), kinds::GOOGLE_API)?;
                ex.set_body(r.get("downloadUrl").cloned().unwrap_or_default());
            }
            "createFunction" | "updateFunction" => {
                let name = format!("projects/{}/locations/{}/functions/{}", self.project, self.location, self.function);
                let mut f = match ex.body() {
                    Value::Object(m) => m.clone(),
                    _ => cameleer_core::value::ObjectMap::new(),
                };
                f.insert("name".into(), Value::from(name));
                let mut set = |k: &str, v: Option<String>| {
                    if let Some(v) = v {
                        f.insert(k.to_string(), Value::from(v));
                    }
                };
                set("entryPoint", crate::util::header_str(ex, headers::ENTRY_POINT).or_else(|| self.entry_point.clone()));
                set("runtime", crate::util::header_str(ex, headers::RUNTIME).or_else(|| self.runtime.clone()));
                set("sourceArchiveUrl", crate::util::header_str(ex, headers::SOURCE_ARCHIVE_URL));
                set("sourceUploadUrl", crate::util::header_str(ex, headers::SOURCE_UPLOAD_URL));
                let op_resp = if op == "createFunction" {
                    if !f.contains_key("httpsTrigger") && !f.contains_key("eventTrigger") {
                        f.insert("httpsTrigger".into(), obj([]));
                    }
                    self.gcp.json("POST", &self.base(), Some(&Value::Object(f)), kinds::GOOGLE_API)?
                } else {
                    let mask: Vec<String> = f.keys().filter(|k| *k != "name").cloned().collect();
                    self.gcp.json("PATCH", &format!("{}?updateMask={}", one, uri_encode(&mask.join(","), false)), Some(&Value::Object(f)), kinds::GOOGLE_API)?
                };
                let resource = self.wait_operation(op_resp)?;
                ex.set_header(headers::RESPONSE_OBJECT, resource.clone());
                ex.set_body(resource);
            }
            other => return Err(unsupported("google-functions", other)),
        }
        Ok(())
    }
}

impl FunctionsService {
    /// Poll a long-running operation until done; returns its `response`.
    fn wait_operation(&self, mut op: Value) -> Result<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            if op.get("done").is_some_and(|d| d.is_truthy()) {
                if let Some(e) = op.get("error").filter(|e| !e.is_null()) {
                    return Err(cloud_error(kinds::GOOGLE_API, format!("Operation failed: {}", e.to_json())));
                }
                return Ok(op.get("response").cloned().unwrap_or_default());
            }
            if std::time::Instant::now() > deadline {
                return Err(cloud_error(kinds::GOOGLE_API, "Timed out waiting for the Cloud Functions operation"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            let name = op.get("name").map(|n| n.to_string()).unwrap_or_default();
            op = self.gcp.json("GET", &format!("{}/v1/{}", self.gcp.base("https://cloudfunctions.googleapis.com"), name), None, kinds::GOOGLE_API)?;
        }
    }
}
