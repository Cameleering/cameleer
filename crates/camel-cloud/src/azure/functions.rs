//! The `azure-functions` component (rust-camel, producer): invokes HTTP-triggered Azure
//! Functions.
//!
//! `azure-functions://functionApp/functionName?functionKey=RAW(...)`
//!
//! The request goes to `https://{functionApp}.azurewebsites.net/api/{functionName}` (or
//! `{uriEndpointOverride}/api/{functionName}`) with method `httpMethod` /
//! `CamelAzureFunctionsHttpMethod` (default `POST`; `GET` / `DELETE` send no body). The body
//! is sent as is (maps and lists as JSON). Authorization (`authLevel` of the function):
//! `functionKey` (or `masterKey`) in the `x-functions-key` header (default) or as the `code`
//! query parameter with `keyInQuery=true`; Entra ID tokens with `tenantId` / `clientId` /
//! `clientSecret` and `scope` (App Service authentication); nothing for anonymous functions.
//! Extra query parameters from `CamelAzureFunctionsQuery` (`a=1&b=2`).
//!
//! Result: body = response text (`CamelAzureFunctionsStatusCode`,
//! `CamelAzureFunctionsContentType`); JSON responses are parsed into maps/lists when
//! `parseJson=true`. Non-2xx statuses raise `HttpResponseException` (status in the message).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::rest::{aad_credential, AzureAuth, AzureRest};
use crate::endpoint::CloudService;
use crate::http::Request;
use crate::util::{header_str, kinds, parse_json, uri_encode, Opts};

/// Header names.
pub mod headers {
    pub const HTTP_METHOD: &str = "CamelAzureFunctionsHttpMethod";
    pub const QUERY: &str = "CamelAzureFunctionsQuery";
    pub const STATUS_CODE: &str = "CamelAzureFunctionsStatusCode";
    pub const CONTENT_TYPE: &str = "CamelAzureFunctionsContentType";
}

/// The Functions endpoint configuration and client.
pub struct FunctionsService {
    pub rest: AzureRest,
    pub function: String,
    pub http_method: String,
    pub key_in_query: Option<String>,
    pub parse_json: bool,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (app, function) = remaining
        .split_once('/')
        .filter(|(a, f)| !a.is_empty() && !f.is_empty())
        .ok_or_else(|| CamelError::illegal_argument("Expected azure-functions://functionApp/functionName"))?;
    let key = o.get("functionKey").or_else(|| o.get("masterKey"));
    let key_in_query = o.bool("keyInQuery", false)?;
    let identity = o.string("credentialType", "").eq_ignore_ascii_case("AZURE_IDENTITY");
    let aad = aad_credential(o, identity)?;
    let scope = o.get("scope");
    let auth = match (&key, aad) {
        (Some(k), _) if !key_in_query => AzureAuth::Header("x-functions-key".into(), k.clone()),
        (Some(_), _) => AzureAuth::None,
        (None, Some(c)) => {
            let scope = scope.unwrap_or_else(|| format!("api://{}/.default", c.client_id));
            AzureAuth::Bearer(c, scope)
        }
        (None, None) => AzureAuth::None,
    };
    let rest = AzureRest::from_opts(o, &format!("https://{}.azurewebsites.net", app), auth, kinds::AZURE_HTTP)?;
    Ok(Arc::new(FunctionsService {
        rest,
        function: function.to_string(),
        http_method: o.string("httpMethod", "POST").to_ascii_uppercase(),
        key_in_query: if key_in_query { key } else { None },
        parse_json: o.bool("parseJson", false)?,
    }))
}

impl CloudService for FunctionsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let method = header_str(ex, headers::HTTP_METHOD).map(|m| m.to_ascii_uppercase()).unwrap_or_else(|| self.http_method.clone());
        let mut query: Vec<String> = Vec::new();
        if let Some(k) = &self.key_in_query {
            query.push(format!("code={}", uri_encode(k, false)));
        }
        if let Some(q) = header_str(ex, headers::QUERY) {
            query.push(q.trim_start_matches('?').to_string());
        }
        let mut req = Request::new(&method, self.rest.url(&format!("/api/{}", uri_encode(&self.function, true)), &query.join("&"))?);
        if method != "GET" && method != "DELETE" && method != "HEAD" {
            let (ct, body) = match ex.body() {
                Value::Object(_) | Value::Array(_) => ("application/json", ex.body().to_json().into_bytes()),
                Value::Bytes(b) => ("application/octet-stream", b.clone()),
                other => ("text/plain; charset=utf-8", other.to_bytes()),
            };
            req = req.header("Content-Type", ct).body(body);
        }
        let resp = self.rest.send(req)?;
        let ct = resp.header("Content-Type").unwrap_or("").to_string();
        ex.set_header(headers::STATUS_CODE, resp.status as i64);
        ex.set_header(headers::CONTENT_TYPE, ct.clone());
        if self.parse_json && ct.contains("json") {
            ex.set_body(parse_json(&resp.body)?);
        } else {
            ex.set_body(resp.text());
        }
        Ok(())
    }
}
