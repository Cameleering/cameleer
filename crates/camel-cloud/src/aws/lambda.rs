//! The `aws2-lambda` component (Camel `camel-aws2-lambda`, producer) over the Lambda REST API.
//!
//! `aws2-lambda://functionNameOrArn?operation=invokeFunction|getFunction|listFunctions|deleteFunction`
//!
//! `invokeFunction` (default) posts the body as payload and sets the response payload as the
//! body; a function error is reported in the `CamelAwsLambdaFunctionError` header (the payload
//! then holds the error document). Invocation type `RequestResponse` by default, or
//! `CamelAwsLambdaInvocationType` (`Event`, `DryRun`). `listFunctions` returns an array of
//! function configurations, `getFunction` the function document.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::Request;
use crate::util::{header_str, kinds, parse_json, uri_encode, Opts};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAwsLambdaOperation";
    pub const INVOCATION_TYPE: &str = "CamelAwsLambdaInvocationType";
    pub const FUNCTION_ERROR: &str = "CamelAwsLambdaFunctionError";
    pub const STATUS_CODE: &str = "CamelAwsLambdaStatusCode";
}

/// The Lambda endpoint configuration and client.
pub struct LambdaService {
    pub aws: AwsConfig,
    pub function: String,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Function name must be specified: aws2-lambda://functionName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["pojoRequest"]);
    Ok(Arc::new(LambdaService { function: remaining.to_string(), operation: o.get("operation"), aws }))
}

impl LambdaService {
    fn url(&self, suffix: &str) -> Result<crate::http::Url> {
        let mut u = self.aws.endpoint("lambda")?;
        u.path = format!("{}/2015-03-31/functions{}", u.path.trim_end_matches('/'), suffix);
        Ok(u)
    }

    /// Invoke the function synchronously; returns (status, function error, payload).
    pub fn invoke(&self, payload: Vec<u8>, invocation_type: &str) -> Result<(u16, Option<String>, Vec<u8>)> {
        let req = Request::new("POST", self.url(&format!("/{}/invocations", uri_encode(&self.function, false)))?)
            .header("X-Amz-Invocation-Type", invocation_type)
            .header("Content-Type", "application/json")
            .body(payload);
        let resp = self.aws.send(req, "lambda", kinds::LAMBDA)?;
        Ok((resp.status, resp.header("X-Amz-Function-Error").map(|s| s.to_string()), resp.body))
    }
}

impl CloudService for LambdaService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "invokeFunction");
        match op.as_str() {
            "invokeFunction" => {
                let it = header_str(ex, headers::INVOCATION_TYPE).unwrap_or_else(|| "RequestResponse".into());
                let (status, err, payload) = self.invoke(ex.body().to_bytes(), &it)?;
                ex.set_header(headers::STATUS_CODE, status as i64);
                if let Some(e) = err {
                    ex.set_header(headers::FUNCTION_ERROR, e);
                }
                ex.set_body(String::from_utf8_lossy(&payload).into_owned());
            }
            "listFunctions" => {
                let resp = self.aws.send(Request::new("GET", self.url("/")?), "lambda", kinds::LAMBDA)?;
                let v = parse_json(&resp.body)?;
                ex.set_body(v.get("Functions").cloned().unwrap_or(Value::Array(vec![])));
            }
            "getFunction" => {
                let resp = self.aws.send(Request::new("GET", self.url(&format!("/{}", uri_encode(&self.function, false)))?), "lambda", kinds::LAMBDA)?;
                ex.set_body(parse_json(&resp.body)?);
            }
            "deleteFunction" => {
                self.aws.send(Request::new("DELETE", self.url(&format!("/{}", uri_encode(&self.function, false)))?), "lambda", kinds::LAMBDA)?;
            }
            other => return Err(unsupported("aws2-lambda", other)),
        }
        Ok(())
    }
}
