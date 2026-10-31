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

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::GcpConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{cloud_error, kinds, obj, uri_encode, Opts};

/// Header names (Camel `GoogleCloudFunctionsConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelGoogleCloudFunctionsOperation";
    /// rust-camel extension: execution id of `callFunction`.
    pub const EXECUTION_ID: &str = "CamelGoogleCloudFunctionsExecutionId";
}

/// A Cloud Functions client.
pub struct FunctionsService {
    pub gcp: GcpConfig,
    pub function: String,
    pub project: String,
    pub location: String,
    pub operation: Option<String>,
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
    Ok(Arc::new(FunctionsService { function: remaining.to_string(), project, location, operation: o.get("operation"), gcp }))
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
            other => return Err(unsupported("google-functions", other)),
        }
        Ok(())
    }
}
