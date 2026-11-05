//! Azure AD (Microsoft Entra ID) token credentials for the Azure components
//! (`credentialType=AZURE_IDENTITY` / `TOKEN_CREDENTIAL`): the tokens are put on the
//! `$cbs` node with type `jwt`.
//!
//! * [`ClientSecretCredential`]: OAuth 2.0 client credentials grant against
//!   `{authorityHost}/{tenantId}/oauth2/v2.0/token`.
//! * [`ManagedIdentityCredential`]: the Azure Instance Metadata Service
//!   (`http://169.254.169.254/metadata/identity/oauth2/token`, header `Metadata: true`)
//!   or the App Service / Functions endpoint (`IDENTITY_ENDPOINT` + `IDENTITY_HEADER`).
//! * [`DefaultAzureCredential`]: environment client secret (`AZURE_TENANT_ID`,
//!   `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_AUTHORITY_HOST`), then managed
//!   identity (`AZURE_CLIENT_ID` selects a user-assigned identity).
//!
//! Any other source plugs in through the [`TokenCredential`] trait
//! (`tokenCredential=#bean` binds an `Arc<dyn TokenCredential>`). Tokens are cached per
//! scope until five minutes before they expire. The HTTP client is a minimal HTTP/1.1
//! client over `cameleer_tls::net::NetStream` (TLS for `https`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::net::NetStream;
use cameleer_tls::tls::config::TlsClientConfig;

use crate::util::now_secs;

/// Scope of Azure Service Bus tokens.
pub const SERVICE_BUS_SCOPE: &str = "https://servicebus.azure.net/.default";
/// Scope of Azure Event Hubs tokens.
pub const EVENT_HUBS_SCOPE: &str = "https://eventhubs.azure.net/.default";
/// Default authority host.
pub const DEFAULT_AUTHORITY_HOST: &str = "https://login.microsoftonline.com";
/// Default managed identity endpoint (IMDS).
pub const IMDS_ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";

/// An access token and its expiry (epoch seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessToken {
    pub token: String,
    pub expires_on: u64,
}

/// A source of Azure AD access tokens (Azure SDK `TokenCredential`).
pub trait TokenCredential: Send + Sync {
    fn get_token(&self, scope: &str) -> Result<AccessToken>;
}

/// A shared credential (`Debug` for configuration structs).
#[derive(Clone)]
pub struct Credential(pub Arc<dyn TokenCredential>);

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(..)")
    }
}

fn auth_error(msg: impl Into<String>) -> CamelError {
    crate::util::register_kinds();
    CamelError::new(crate::util::kinds::AUTHENTICATION_FAILURE, msg)
}

/// Per-scope token cache.
#[derive(Default)]
struct Cache(Mutex<HashMap<String, AccessToken>>);

impl Cache {
    fn get_or(&self, scope: &str, fetch: impl FnOnce() -> Result<AccessToken>) -> Result<AccessToken> {
        if let Some(t) = self.0.lock().unwrap().get(scope) {
            if t.expires_on > now_secs() + 300 {
                return Ok(t.clone());
            }
        }
        let t = fetch()?;
        self.0.lock().unwrap().insert(scope.to_string(), t.clone());
        Ok(t)
    }
}

/// OAuth 2.0 client credentials (application id + secret).
pub struct ClientSecretCredential {
    pub tenant_id: String,
    pub client_id: String,
    client_secret: String,
    pub authority_host: String,
    tls: Option<Arc<TlsClientConfig>>,
    cache: Cache,
}

impl ClientSecretCredential {
    pub fn new(tenant_id: &str, client_id: &str, client_secret: &str) -> Self {
        ClientSecretCredential {
            tenant_id: tenant_id.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            authority_host: DEFAULT_AUTHORITY_HOST.to_string(),
            tls: None,
            cache: Cache::default(),
        }
    }
    /// Another authority (sovereign clouds, or a test server).
    pub fn authority_host(mut self, host: &str) -> Self {
        self.authority_host = host.trim_end_matches('/').to_string();
        self
    }
    /// TLS client settings for `https` (default: the system trust store).
    pub fn tls(mut self, cfg: Arc<TlsClientConfig>) -> Self {
        self.tls = Some(cfg);
        self
    }
}

impl TokenCredential for ClientSecretCredential {
    fn get_token(&self, scope: &str) -> Result<AccessToken> {
        self.cache.get_or(scope, || {
            let url = format!("{}/{}/oauth2/v2.0/token", self.authority_host, self.tenant_id);
            let enc = cameleer_core::util::url_encode;
            let body = format!("grant_type=client_credentials&client_id={}&client_secret={}&scope={}", enc(&self.client_id), enc(&self.client_secret), enc(scope));
            let (status, text) = http_request("POST", &url, &[("Content-Type", "application/x-www-form-urlencoded")], body.as_bytes(), self.tls.as_deref())?;
            parse_token_response(status, &text, "Azure AD client credentials")
        })
    }
}

/// Managed identity (IMDS or the App Service identity endpoint).
pub struct ManagedIdentityCredential {
    pub endpoint: String,
    /// User-assigned identity (client id).
    pub client_id: Option<String>,
    /// App Service `IDENTITY_HEADER` (sent as `X-IDENTITY-HEADER`, api-version 2019-08-01).
    pub identity_header: Option<String>,
    tls: Option<Arc<TlsClientConfig>>,
    cache: Cache,
}

impl ManagedIdentityCredential {
    /// IMDS, or the App Service endpoint when `IDENTITY_ENDPOINT` / `IDENTITY_HEADER` are set.
    pub fn new(client_id: Option<&str>) -> Self {
        let env_endpoint = std::env::var("IDENTITY_ENDPOINT").ok().filter(|v| !v.is_empty());
        let header = std::env::var("IDENTITY_HEADER").ok().filter(|v| !v.is_empty());
        ManagedIdentityCredential {
            endpoint: env_endpoint.unwrap_or_else(|| IMDS_ENDPOINT.to_string()),
            client_id: client_id.map(String::from),
            identity_header: header,
            tls: None,
            cache: Cache::default(),
        }
    }
    pub fn endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.to_string();
        self
    }
    pub fn tls(mut self, cfg: Arc<TlsClientConfig>) -> Self {
        self.tls = Some(cfg);
        self
    }
}

impl TokenCredential for ManagedIdentityCredential {
    fn get_token(&self, scope: &str) -> Result<AccessToken> {
        self.cache.get_or(scope, || {
            let resource = scope.trim_end_matches("/.default");
            let enc = cameleer_core::util::url_encode;
            let api = if self.identity_header.is_some() { "2019-08-01" } else { "2018-02-01" };
            let mut url = format!("{}{}api-version={}&resource={}", self.endpoint, if self.endpoint.contains('?') { "&" } else { "?" }, api, enc(resource));
            if let Some(c) = &self.client_id {
                url.push_str(&format!("&client_id={}", enc(c)));
            }
            let mut headers = vec![("Metadata", "true")];
            if let Some(h) = &self.identity_header {
                headers.push(("X-IDENTITY-HEADER", h.as_str()));
            }
            let (status, text) = http_request("GET", &url, &headers, &[], self.tls.as_deref())?;
            parse_token_response(status, &text, "managed identity")
        })
    }
}

/// The Azure SDK's default chain: environment client secret, then managed identity.
pub struct DefaultAzureCredential {
    chain: Vec<Box<dyn TokenCredential>>,
}

impl DefaultAzureCredential {
    pub fn new(tls: Option<Arc<TlsClientConfig>>) -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut chain: Vec<Box<dyn TokenCredential>> = Vec::new();
        if let (Some(t), Some(c), Some(s)) = (env("AZURE_TENANT_ID"), env("AZURE_CLIENT_ID"), env("AZURE_CLIENT_SECRET")) {
            let mut cred = ClientSecretCredential::new(&t, &c, &s);
            if let Some(h) = env("AZURE_AUTHORITY_HOST") {
                cred = cred.authority_host(&h);
            }
            if let Some(cfg) = &tls {
                cred = cred.tls(cfg.clone());
            }
            chain.push(Box::new(cred));
        }
        let mut mi = ManagedIdentityCredential::new(env("AZURE_CLIENT_ID").as_deref());
        if let Some(cfg) = &tls {
            mi = mi.tls(cfg.clone());
        }
        chain.push(Box::new(mi));
        DefaultAzureCredential { chain }
    }
}

impl TokenCredential for DefaultAzureCredential {
    fn get_token(&self, scope: &str) -> Result<AccessToken> {
        let mut errors = Vec::new();
        for c in &self.chain {
            match c.get_token(scope) {
                Ok(t) => return Ok(t),
                Err(e) => errors.push(e.message),
            }
        }
        Err(auth_error(format!("DefaultAzureCredential failed to retrieve a token: {}", errors.join("; "))))
    }
}

fn parse_token_response(status: u16, text: &str, what: &str) -> Result<AccessToken> {
    let v = cameleer_core::json::parse(text).map_err(|_| auth_error(format!("{}: HTTP {} with an unreadable body", what, status)))?;
    if status != 200 {
        let desc = v.get("error_description").or_else(|| v.get("error")).map(|e| e.to_string()).unwrap_or_default();
        return Err(auth_error(format!("{} token request failed: HTTP {} {}", what, status, desc)));
    }
    let token = v.get("access_token").map(|t| t.to_string()).filter(|t| !t.is_empty()).ok_or_else(|| auth_error(format!("{}: no access_token in the response", what)))?;
    let num = |k: &str| v.get(k).and_then(|x| x.as_i64().or_else(|| x.to_string().trim().parse().ok()));
    let expires_on = match (num("expires_on"), num("expires_in")) {
        (Some(on), _) => on.max(0) as u64,
        (None, Some(n)) => now_secs() + n.max(0) as u64,
        _ => now_secs() + 3600,
    };
    Ok(AccessToken { token, expires_on })
}

/// A minimal HTTP/1.1 request (`Connection: close`); returns the status and body text.
pub fn http_request(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8], tls: Option<&TlsClientConfig>) -> Result<(u16, String)> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(CamelError::illegal_argument(format!("Unsupported url: {}", url)));
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid url: {}", url)))?),
        _ => (hostport.trim_matches(['[', ']']).to_string(), if https { 443 } else { 80 }),
    };
    let timeout = Duration::from_secs(30);
    let io = |e: std::io::Error| cameleer_tls::camel_support::io_to_camel(e, &format!("{}:{}", host, port));
    let mut stream = if https {
        let default;
        let cfg = match tls {
            Some(c) => c,
            None => {
                default = TlsClientConfig::builder().build();
                &default
            }
        };
        NetStream::connect(&host, port, Some(cfg), timeout).map_err(io)?
    } else {
        NetStream::connect(&host, port, None, timeout).map_err(io)?
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let mut req = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\nContent-Length: {}\r\n", method, path, hostport, body.len());
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(io)?;
    stream.write_all(body).map_err(io)?;
    stream.flush().map_err(io)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(io)?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| CamelError::io(format!("Malformed HTTP response from {}", url)))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut payload = raw[split + 4..].to_vec();
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).ok_or_else(|| CamelError::io(format!("Malformed HTTP status from {}", url)))?;
    if head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked")) {
        payload = dechunk(&payload);
    }
    Ok((status, String::from_utf8_lossy(&payload).to_string()))
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..i]).split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        b = &b[i + 2..];
        if size == 0 || b.len() < size {
            break;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[(size + 2).min(b.len())..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_responses_and_chunks() {
        let t = parse_token_response(200, r#"{"access_token":"abc","expires_in":"3600","token_type":"Bearer"}"#, "x").unwrap();
        assert_eq!(t.token, "abc");
        assert!(t.expires_on >= now_secs() + 3500);
        let t = parse_token_response(200, r#"{"access_token":"m","expires_on":"1900000000"}"#, "x").unwrap();
        assert_eq!(t.expires_on, 1_900_000_000);
        assert!(parse_token_response(401, r#"{"error":"invalid_client","error_description":"bad secret"}"#, "x").unwrap_err().message.contains("bad secret"));
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), b"abcde");
    }
}
