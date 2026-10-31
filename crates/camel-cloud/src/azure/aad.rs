//! Microsoft Entra ID (Azure AD) OAuth2 client-credentials authentication
//! (`ClientSecretCredential`), used by Key Vault and available to other Azure components.
//!
//! The token is requested from `{authorityHost}/{tenantId}/oauth2/v2.0/token` with
//! `grant_type=client_credentials`, `client_id`, `client_secret` and `scope`, and cached
//! process-wide until a minute before it expires. `authorityHost` defaults to
//! `AZURE_AUTHORITY_HOST` or `https://login.microsoftonline.com`. With
//! `credentialType=AZURE_IDENTITY` the client id / secret / tenant come from the
//! `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` and `AZURE_TENANT_ID` environment variables
//! (the environment part of `DefaultAzureCredential`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use camel_core::error::{CamelError, Result};

use crate::http::{HttpClient, Request, Url};
use crate::util::{cloud_error, encode_form, kinds, now_secs, parse_json};

/// Default authority host.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";

/// A client secret credential.
#[derive(Clone)]
pub struct ClientSecretCredential {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub authority_host: String,
}

impl std::fmt::Debug for ClientSecretCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientSecretCredential").field("tenant_id", &self.tenant_id).field("client_id", &self.client_id).field("authority_host", &self.authority_host).finish()
    }
}

fn token_cache() -> &'static Mutex<HashMap<(String, String, String, String), (String, i64)>> {
    static C: OnceLock<Mutex<HashMap<(String, String, String, String), (String, i64)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

impl ClientSecretCredential {
    /// Build from explicit values, or (`identity` = true) from the `AZURE_*` environment
    /// variables when values are missing.
    pub fn new(tenant_id: Option<String>, client_id: Option<String>, client_secret: Option<String>, authority_host: Option<String>, identity: bool) -> Result<ClientSecretCredential> {
        let env = |k: &str| if identity { std::env::var(k).ok().filter(|v| !v.is_empty()) } else { None };
        let need = |v: Option<String>, env_name: &str, opt: &str| {
            v.or_else(|| env(env_name)).ok_or_else(|| CamelError::illegal_argument(format!("Azure AD authentication requires the {} option (or {} with credentialType=AZURE_IDENTITY)", opt, env_name)))
        };
        Ok(ClientSecretCredential {
            tenant_id: need(tenant_id, "AZURE_TENANT_ID", "tenantId")?,
            client_id: need(client_id, "AZURE_CLIENT_ID", "clientId")?,
            client_secret: need(client_secret, "AZURE_CLIENT_SECRET", "clientSecret")?,
            authority_host: authority_host
                .or_else(|| std::env::var("AZURE_AUTHORITY_HOST").ok().filter(|v| !v.is_empty()))
                .unwrap_or_else(|| DEFAULT_AUTHORITY.to_string())
                .trim_end_matches('/')
                .to_string(),
        })
    }

    /// The token endpoint URL.
    pub fn token_url(&self) -> String {
        format!("{}/{}/oauth2/v2.0/token", self.authority_host, self.tenant_id)
    }

    /// An access token for `scope` (e.g. `https://vault.azure.net/.default`), cached.
    pub fn token(&self, scope: &str, http: &HttpClient) -> Result<String> {
        let key = (self.authority_host.clone(), self.tenant_id.clone(), format!("{}#{}", self.client_id, &crate::util::sha256_hex(self.client_secret.as_bytes())[..16]), scope.to_string());
        let now = now_secs();
        if let Some((t, exp)) = token_cache().lock().unwrap().get(&key) {
            if *exp > now + 60 {
                return Ok(t.clone());
            }
        }
        let body = encode_form(&[
            ("grant_type".into(), "client_credentials".into()),
            ("client_id".into(), self.client_id.clone()),
            ("client_secret".into(), self.client_secret.clone()),
            ("scope".into(), scope.to_string()),
        ]);
        let req = Request::new("POST", Url::parse(&self.token_url())?).header("Content-Type", "application/x-www-form-urlencoded").body(body.into_bytes());
        let resp = http.send(&req)?;
        let v = parse_json(&resp.body).unwrap_or_default();
        if !resp.is_success() {
            let msg = v.get("error_description").or_else(|| v.get("error")).map(|e| e.to_string()).unwrap_or_else(|| resp.text());
            return Err(cloud_error(kinds::AZURE_AUTH, format!("ClientSecretCredential authentication failed: {} (status {})", msg.lines().next().unwrap_or(""), resp.status)));
        }
        let tok = v.get("access_token").map(|t| t.to_string()).filter(|t| !t.is_empty()).ok_or_else(|| cloud_error(kinds::AZURE_AUTH, "Token response without access_token"))?;
        let exp = now + v.get("expires_in").and_then(|e| e.as_i64()).unwrap_or(3600);
        token_cache().lock().unwrap().insert(key, (tok.clone(), exp));
        Ok(tok)
    }

    /// Forget the cached token for `scope` (after a 401).
    pub fn invalidate(&self, scope: &str) {
        token_cache().lock().unwrap().remove(&(self.authority_host.clone(), self.tenant_id.clone(), format!("{}#{}", self.client_id, &crate::util::sha256_hex(self.client_secret.as_bytes())[..16]), scope.to_string()));
    }
}
