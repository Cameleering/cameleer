//! Shared client of the Azure JSON REST components (Functions, Event Grid, Schema Registry,
//! OpenAI): base URL / endpoint override, key header or Microsoft Entra ID bearer
//! authentication (retried once after a `401`), proxy and error mapping.

use camel_core::error::{CamelError, Result};

use super::aad::ClientSecretCredential;
use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{cloud_error, parse_json, Opts};

/// How a request is authorized.
#[derive(Clone, Debug)]
pub enum AzureAuth {
    /// No credentials (anonymous functions, local emulators).
    None,
    /// A key sent in a header (`x-functions-key`, `aeg-sas-key`, `api-key`...).
    Header(String, String),
    /// A bearer token for `scope` from the Entra ID client credentials.
    Bearer(ClientSecretCredential, String),
}

/// A REST client for one Azure service endpoint.
#[derive(Clone, Debug)]
pub struct AzureRest {
    /// Base URL without trailing slash.
    pub base: String,
    pub auth: AzureAuth,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
    /// Exception kind of error responses.
    pub kind: &'static str,
}

/// The Entra ID client credentials of the endpoint options (`tenantId`, `clientId`,
/// `clientSecret`, `authorityHost`; `credentialType=AZURE_IDENTITY` reads `AZURE_*` env vars),
/// None when no client id is configured and the identity is not requested.
pub fn aad_credential(o: &mut Opts, identity: bool) -> Result<Option<ClientSecretCredential>> {
    let opts: std::collections::HashMap<&str, Option<String>> = ["tenantId", "clientId", "clientSecret", "authorityHost", "federatedTokenFile", "managedIdentityClientId", "managedIdentityEndpoint", "managedIdentityHeader"].into_iter().map(|k| (k, o.get(k))).collect();
    if opts["clientId"].is_none() && opts["tenantId"].is_none() && !identity {
        return Ok(None);
    }
    ClientSecretCredential::resolve(&|k: &str| opts.get(k).cloned().flatten(), identity).map(Some)
}

impl AzureRest {
    /// Read the common options: `uriEndpointOverride` (+ `overrideEndpoint`, default true when
    /// an override is given), `proxyHost` / `proxyPort`, `trustAllCertificates`.
    pub fn from_opts(o: &mut Opts, default_base: &str, auth: AzureAuth, kind: &'static str) -> Result<AzureRest> {
        let over = o.get("uriEndpointOverride");
        let base = if o.bool("overrideEndpoint", over.is_some())? { over.unwrap_or_else(|| default_base.to_string()) } else { default_base.to_string() };
        let proxy = match o.get("proxyHost") {
            Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
            None => {
                o.ignore(&["proxyPort"]);
                Proxy::Environment
            }
        };
        Ok(AzureRest { base: base.trim_end_matches('/').to_string(), auth, proxy, trust_all_certificates: o.bool("trustAllCertificates", false)?, kind })
    }

    pub fn http(&self) -> HttpClient {
        HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates)
    }

    /// `{base}{path}` with an already encoded query.
    pub fn url(&self, path: &str, query: &str) -> Result<Url> {
        let q = if query.is_empty() { String::new() } else { format!("?{}", query) };
        Url::parse(&format!("{}{}{}", self.base, path, q))
    }

    /// Send with the configured authorization (bearer tokens refreshed once after a 401);
    /// non-2xx responses become errors of the client's kind.
    pub fn send(&self, req: Request) -> Result<Response> {
        let http = self.http();
        for attempt in 0..2 {
            let mut r = req.clone();
            match &self.auth {
                AzureAuth::None => {}
                AzureAuth::Header(k, v) => r.set_header(k, v.clone()),
                AzureAuth::Bearer(c, scope) => r.set_header("Authorization", format!("Bearer {}", c.token(scope, &http)?)),
            }
            let resp = http.send(&r)?;
            if resp.status == 401 && attempt == 0 {
                if let AzureAuth::Bearer(c, scope) = &self.auth {
                    c.invalidate(scope);
                    continue;
                }
            }
            if resp.is_success() {
                return Ok(resp);
            }
            return Err(rest_error(self.kind, &resp));
        }
        unreachable!("loop returns")
    }

    /// Send a JSON body and parse the JSON response.
    pub fn json(&self, method: &str, path: &str, query: &str, body: Option<&camel_core::value::Value>) -> Result<camel_core::value::Value> {
        let mut req = Request::new(method, self.url(path, query)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        parse_json(&self.send(req)?.body)
    }
}

/// Build the exception for an Azure JSON error response (`{"error": {"code", "message"}}`).
pub fn rest_error(kind: &str, resp: &Response) -> CamelError {
    let v = parse_json(&resp.body).unwrap_or_default();
    let e = v.get("error").cloned().unwrap_or_default();
    let code = e.get("code").map(|c| c.to_string()).filter(|c| !c.is_empty()).or_else(|| resp.header("x-ms-error-code").map(|s| s.to_string())).unwrap_or_else(|| format!("Http{}", resp.status));
    let msg = e.get("message").map(|m| m.to_string()).or_else(|| v.get("message").map(|m| m.to_string())).unwrap_or_else(|| resp.text());
    cloud_error(kind, format!("Status code {}, \"{}\" (ErrorCode: {})", resp.status, msg.lines().next().unwrap_or(""), code))
}

/// The HTTP status of an Azure exception message.
pub fn error_status(e: &CamelError) -> Option<u16> {
    e.message.strip_prefix("Status code ").and_then(|r| r.split(',').next()).and_then(|s| s.trim().parse().ok())
}

/// Validate a `credentialType` option against the accepted values.
pub fn credential_type(o: &mut Opts, default: &str, accepted: &[&str]) -> Result<String> {
    let ct = o.string("credentialType", default).to_ascii_uppercase();
    if !accepted.contains(&ct.as_str()) {
        return Err(CamelError::illegal_argument(format!("Unsupported credentialType {} ({})", ct, accepted.join(", "))));
    }
    Ok(ct)
}
