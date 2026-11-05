//! Microsoft Entra ID (Azure AD) token credentials of the Azure components.
//!
//! * Client secret (`ClientSecretCredential`): `grant_type=client_credentials` with
//!   `client_id` / `client_secret` / `scope` against `{authorityHost}/{tenantId}/oauth2/v2.0/token`.
//! * Workload identity federation (`WorkloadIdentityCredential`, AKS workload identity / any
//!   OIDC federated credential): the same token endpoint with `client_assertion_type =
//!   urn:ietf:params:oauth:client-assertion-type:jwt-bearer` and the federated token read from
//!   `federatedTokenFile` / `AZURE_FEDERATED_TOKEN_FILE` as `client_assertion` (re-read on
//!   every refresh, as Kubernetes rotates it).
//! * Managed identity (`ManagedIdentityCredential`): the Azure Instance Metadata Service
//!   (`GET http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=...`,
//!   header `Metadata: true`), or the App Service / Functions / Container Apps endpoint
//!   (`IDENTITY_ENDPOINT` + `IDENTITY_HEADER`, `api-version=2019-08-01`, header
//!   `X-IDENTITY-HEADER`); `managedIdentityClientId` (or `clientId` / `AZURE_CLIENT_ID`) selects a
//!   user-assigned identity, the Cameleer `managedIdentityEndpoint` option overrides the
//!   endpoint (e.g. an emulator).
//!
//! `credentialType=AZURE_IDENTITY` resolves like `DefaultAzureCredential`: environment client
//! secret (`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`), then workload identity
//! (`AZURE_FEDERATED_TOKEN_FILE`), then managed identity. Explicit options win over the
//! environment. Tokens are cached process-wide until a minute before they expire;
//! `authorityHost` defaults to `AZURE_AUTHORITY_HOST` or `https://login.microsoftonline.com`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use cameleer_core::error::{CamelError, Result};

use crate::http::{HttpClient, Request, Url};
use crate::util::{cloud_error, encode_form, kinds, now_secs, parse_json, uri_encode};

/// Default authority host.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";
/// Default managed identity endpoint (IMDS).
pub const IMDS_ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
/// `client_assertion_type` of federated credentials.
pub const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// How tokens are obtained.
#[derive(Clone, PartialEq, Eq)]
pub enum AadMethod {
    /// Client credentials with a secret.
    ClientSecret(String),
    /// Workload identity federation: the federated token file.
    WorkloadIdentity(String),
    /// Managed identity: endpoint and the App Service identity header (None = IMDS).
    ManagedIdentity { endpoint: String, identity_header: Option<String> },
}

/// An Entra ID credential (client secret, workload identity or managed identity).
#[derive(Clone, PartialEq, Eq)]
pub struct AadCredential {
    pub tenant_id: String,
    /// Client id (for a managed identity: the user-assigned identity, empty = system assigned).
    pub client_id: String,
    pub authority_host: String,
    pub method: AadMethod,
}

/// The historical name of [`AadCredential`].
pub type ClientSecretCredential = AadCredential;

impl std::fmt::Debug for AadCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = match &self.method {
            AadMethod::ClientSecret(_) => "ClientSecret".to_string(),
            AadMethod::WorkloadIdentity(file) => format!("WorkloadIdentity({})", file),
            AadMethod::ManagedIdentity { endpoint, .. } => format!("ManagedIdentity({})", endpoint),
        };
        f.debug_struct("AadCredential").field("tenant_id", &self.tenant_id).field("client_id", &self.client_id).field("authority_host", &self.authority_host).field("method", &method).finish()
    }
}

fn token_cache() -> &'static Mutex<HashMap<String, (String, i64)>> {
    static C: OnceLock<Mutex<HashMap<String, (String, i64)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

impl AadCredential {
    /// Build from explicit values, or (`identity` = true) resolve like `DefaultAzureCredential`
    /// from the `AZURE_*` environment variables when values are missing.
    pub fn new(tenant_id: Option<String>, client_id: Option<String>, client_secret: Option<String>, authority_host: Option<String>, identity: bool) -> Result<AadCredential> {
        Self::resolve(&|k: &str| match k {
            "tenantId" => tenant_id.clone(),
            "clientId" => client_id.clone(),
            "clientSecret" => client_secret.clone(),
            "authorityHost" => authority_host.clone(),
            _ => None,
        }, identity)
    }

    /// Build from named options (`tenantId`, `clientId`, `clientSecret`, `authorityHost`,
    /// `federatedTokenFile`, `managedIdentityClientId`, `managedIdentityEndpoint`).
    pub fn resolve(get: &dyn Fn(&str) -> Option<String>, identity: bool) -> Result<AadCredential> {
        let e = |k: &str| if identity { env(k) } else { None };
        let authority_host = get("authorityHost").or_else(|| env("AZURE_AUTHORITY_HOST")).unwrap_or_else(|| DEFAULT_AUTHORITY.to_string()).trim_end_matches('/').to_string();
        let tenant = get("tenantId").or_else(|| e("AZURE_TENANT_ID"));
        let client = get("clientId").or_else(|| e("AZURE_CLIENT_ID"));
        let need = |v: Option<String>, env_name: &str, opt: &str| v.ok_or_else(|| CamelError::illegal_argument(format!("Azure AD authentication requires the {} option (or {} with credentialType=AZURE_IDENTITY)", opt, env_name)));
        if let Some(secret) = get("clientSecret").or_else(|| e("AZURE_CLIENT_SECRET")) {
            return Ok(AadCredential { tenant_id: need(tenant, "AZURE_TENANT_ID", "tenantId")?, client_id: need(client, "AZURE_CLIENT_ID", "clientId")?, authority_host, method: AadMethod::ClientSecret(secret) });
        }
        if !identity {
            need(tenant, "AZURE_TENANT_ID", "tenantId")?;
            need(client, "AZURE_CLIENT_ID", "clientId")?;
            return Err(CamelError::illegal_argument("Azure AD authentication requires the clientSecret option (or credentialType=AZURE_IDENTITY for workload / managed identity)"));
        }
        if let Some(file) = get("federatedTokenFile").or_else(|| env("AZURE_FEDERATED_TOKEN_FILE")) {
            return Ok(AadCredential { tenant_id: need(tenant, "AZURE_TENANT_ID", "tenantId")?, client_id: need(client, "AZURE_CLIENT_ID", "clientId")?, authority_host, method: AadMethod::WorkloadIdentity(file) });
        }
        let (endpoint, identity_header) = match (get("managedIdentityEndpoint"), env("IDENTITY_ENDPOINT"), env("IDENTITY_HEADER")) {
            (Some(ep), _, _) => {
                let h = get("managedIdentityHeader");
                (ep, h)
            }
            (None, Some(ep), Some(h)) => (ep, Some(h)),
            _ => (IMDS_ENDPOINT.to_string(), None),
        };
        Ok(AadCredential {
            tenant_id: tenant.unwrap_or_default(),
            client_id: get("managedIdentityClientId").or(client).unwrap_or_default(),
            authority_host,
            method: AadMethod::ManagedIdentity { endpoint, identity_header },
        })
    }

    /// The token endpoint URL (client secret / workload identity).
    pub fn token_url(&self) -> String {
        match &self.method {
            AadMethod::ManagedIdentity { endpoint, .. } => endpoint.clone(),
            _ => format!("{}/{}/oauth2/v2.0/token", self.authority_host, self.tenant_id),
        }
    }

    fn cache_key(&self, scope: &str) -> String {
        let m = match &self.method {
            AadMethod::ClientSecret(s) => format!("secret#{}", &crate::util::sha256_hex(s.as_bytes())[..16]),
            AadMethod::WorkloadIdentity(f) => format!("wi#{}", f),
            AadMethod::ManagedIdentity { endpoint, .. } => format!("mi#{}", endpoint),
        };
        format!("{}|{}|{}|{}|{}", self.authority_host, self.tenant_id, self.client_id, m, scope)
    }

    /// An access token for `scope` (e.g. `https://vault.azure.net/.default`), cached.
    pub fn token(&self, scope: &str, http: &HttpClient) -> Result<String> {
        let key = self.cache_key(scope);
        let now = now_secs();
        if let Some((t, exp)) = token_cache().lock().unwrap().get(&key) {
            if *exp > now + 60 {
                return Ok(t.clone());
            }
        }
        let (tok, exp) = match &self.method {
            AadMethod::ClientSecret(secret) => self.oauth(http, scope, &[("client_secret".into(), secret.clone())], "ClientSecretCredential")?,
            AadMethod::WorkloadIdentity(file) => {
                let assertion = std::fs::read_to_string(file).map_err(|e| cloud_error(kinds::AZURE_AUTH, format!("WorkloadIdentityCredential cannot read the federated token file {}: {}", file, e)))?;
                self.oauth(http, scope, &[("client_assertion_type".into(), JWT_BEARER.into()), ("client_assertion".into(), assertion.trim().to_string())], "WorkloadIdentityCredential")?
            }
            AadMethod::ManagedIdentity { endpoint, identity_header } => self.managed_identity(http, scope, endpoint, identity_header.as_deref())?,
        };
        token_cache().lock().unwrap().insert(key, (tok.clone(), exp));
        Ok(tok)
    }

    fn oauth(&self, http: &HttpClient, scope: &str, extra: &[(String, String)], what: &str) -> Result<(String, i64)> {
        let mut form = vec![("grant_type".to_string(), "client_credentials".to_string()), ("client_id".to_string(), self.client_id.clone()), ("scope".to_string(), scope.to_string())];
        form.extend(extra.iter().cloned());
        let req = Request::new("POST", Url::parse(&self.token_url())?).header("Content-Type", "application/x-www-form-urlencoded").body(encode_form(&form).into_bytes());
        let resp = http.send(&req)?;
        let v = parse_json(&resp.body).unwrap_or_default();
        if !resp.is_success() {
            let msg = v.get("error_description").or_else(|| v.get("error")).map(|e| e.to_string()).unwrap_or_else(|| resp.text());
            return Err(cloud_error(kinds::AZURE_AUTH, format!("{} authentication failed: {} (status {})", what, msg.lines().next().unwrap_or(""), resp.status)));
        }
        let tok = v.get("access_token").map(|t| t.to_string()).filter(|t| !t.is_empty()).ok_or_else(|| cloud_error(kinds::AZURE_AUTH, "Token response without access_token"))?;
        let exp = now_secs() + v.get("expires_in").and_then(|e| e.as_i64().or_else(|| e.to_string().parse().ok())).unwrap_or(3600);
        Ok((tok, exp))
    }

    fn managed_identity(&self, http: &HttpClient, scope: &str, endpoint: &str, identity_header: Option<&str>) -> Result<(String, i64)> {
        let resource = scope.trim_end_matches("/.default");
        let api = if identity_header.is_some() { "2019-08-01" } else { "2018-02-01" };
        let mut url = format!("{}{}api-version={}&resource={}", endpoint, if endpoint.contains('?') { "&" } else { "?" }, api, uri_encode(resource, false));
        if !self.client_id.is_empty() {
            url.push_str(&format!("&client_id={}", uri_encode(&self.client_id, false)));
        }
        let mut req = Request::new("GET", Url::parse(&url)?);
        req = match identity_header {
            Some(h) => req.header("X-IDENTITY-HEADER", h),
            None => req.header("Metadata", "true"),
        };
        let resp = http.send(&req).map_err(|e| cloud_error(kinds::AZURE_AUTH, format!("ManagedIdentityCredential authentication unavailable: {}", e.message)))?;
        let v = parse_json(&resp.body).unwrap_or_default();
        if !resp.is_success() {
            let msg = v.get("error_description").or_else(|| v.get("message")).or_else(|| v.get("error")).map(|e| e.to_string()).unwrap_or_else(|| resp.text());
            return Err(cloud_error(kinds::AZURE_AUTH, format!("ManagedIdentityCredential authentication failed: {} (status {})", msg.lines().next().unwrap_or(""), resp.status)));
        }
        let tok = v.get("access_token").map(|t| t.to_string()).filter(|t| !t.is_empty()).ok_or_else(|| cloud_error(kinds::AZURE_AUTH, "Managed identity token response without access_token"))?;
        let num = |k: &str| v.get(k).and_then(|e| e.as_i64().or_else(|| e.to_string().parse().ok()));
        let exp = num("expires_on").or_else(|| num("expires_in").map(|i| now_secs() + i)).unwrap_or(now_secs() + 3600);
        Ok((tok, exp))
    }

    /// Forget the cached token for `scope` (after a 401).
    pub fn invalidate(&self, scope: &str) {
        token_cache().lock().unwrap().remove(&self.cache_key(scope));
    }
}
