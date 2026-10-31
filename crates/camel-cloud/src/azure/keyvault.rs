//! The `azure-key-vault` component (Camel `camel-azure-key-vault`, producer) over the Key Vault
//! secrets REST API (`api-version=7.4`), authenticated with Azure AD client credentials, and
//! the `{{azure:...}}` properties function.
//!
//! `azure-key-vault://vaultName?clientId=...&clientSecret=RAW(...)&tenantId=...&operation=...`
//!
//! Operations (`operation` / `CamelAzureKeyVaultOperation`, default `getSecret`), secret name
//! from `CamelAzureKeyVaultSecretName`:
//! * `createSecret`: value = body; body = the secret id (URL), `CamelAzureKeyVaultSecretVersion`.
//! * `getSecret`: body = the value (optionally `CamelAzureKeyVaultSecretVersion`).
//! * `deleteSecret`: soft delete; body = the deleted secret's recovery id.
//! * `purgeDeletedSecret`, `recoverDeletedSecret`, `listSecrets` (rust-camel; body = names).
//!
//! Options: `credentialType` (`CLIENT_SECRET` default, or `AZURE_IDENTITY` = `AZURE_CLIENT_ID` /
//! `AZURE_CLIENT_SECRET` / `AZURE_TENANT_ID`), `authorityHost` (Azure AD host), and the
//! rust-camel extension `uriEndpointOverride` + `overrideEndpoint` for the vault URL.
//!
//! Properties function: `{{azure:name}}`, `{{azure:name/field}}` (JSON field),
//! `{{azure:name@version}}`, `{{azure:name:default}}`, configured with `camel.vault.azure.vaultName`,
//! `clientId`, `clientSecret`, `tenantId`, `azureIdentityEnabled`, and (rust-camel)
//! `authorityHost`, `uriEndpointOverride`.

use std::sync::Arc;

use camel_core::context::{CamelContext, PropertiesFunction};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::aad::ClientSecretCredential;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{cloud_error, header_str, kinds, parse_json, uri_encode, Opts};

/// Key Vault API version.
pub const API_VERSION: &str = "7.4";
/// OAuth scope of Key Vault.
pub const SCOPE: &str = "https://vault.azure.net/.default";

/// Header names (Camel `KeyVaultConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAzureKeyVaultOperation";
    pub const SECRET_NAME: &str = "CamelAzureKeyVaultSecretName";
    /// rust-camel extension.
    pub const SECRET_VERSION: &str = "CamelAzureKeyVaultSecretVersion";
}

/// A Key Vault secrets client.
pub struct KeyVaultService {
    pub vault: String,
    pub credential: ClientSecretCredential,
    pub operation: Option<String>,
    pub endpoint: Option<String>,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Vault name must be specified: azure-key-vault://vaultName"));
    }
    let ct = o.string("credentialType", "CLIENT_SECRET").to_ascii_uppercase();
    if ct != "CLIENT_SECRET" && ct != "AZURE_IDENTITY" {
        return Err(CamelError::illegal_argument(format!("Unsupported credentialType {} (CLIENT_SECRET or AZURE_IDENTITY)", ct)));
    }
    let credential = ClientSecretCredential::new(o.get("tenantId"), o.get("clientId"), o.get("clientSecret"), o.get("authorityHost"), ct == "AZURE_IDENTITY")?;
    let over = o.get("uriEndpointOverride");
    let endpoint = if o.bool("overrideEndpoint", over.is_some())? { over } else { None };
    let proxy = match o.get("proxyHost") {
        Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
        None => {
            o.ignore(&["proxyPort"]);
            Proxy::Environment
        }
    };
    o.ignore(&["secretClient"]);
    Ok(Arc::new(KeyVaultService { vault: remaining.to_string(), credential, operation: o.get("operation"), endpoint, proxy, trust_all_certificates: o.bool("trustAllCertificates", false)? }))
}

impl KeyVaultService {
    fn http(&self) -> HttpClient {
        HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates)
    }

    fn url(&self, path: &str) -> Result<Url> {
        let base = match &self.endpoint {
            Some(e) => e.trim_end_matches('/').to_string(),
            None => format!("https://{}.vault.azure.net", self.vault),
        };
        Url::parse(&format!("{}{}?api-version={}", base, path, API_VERSION))
    }

    /// Send with a bearer token (one retry after a 401); errors become `KeyVaultErrorException`.
    pub fn send(&self, req: Request) -> Result<Response> {
        let http = self.http();
        for attempt in 0..2 {
            let mut r = req.clone();
            r.set_header("Authorization", format!("Bearer {}", self.credential.token(SCOPE, &http)?));
            let resp = http.send(&r)?;
            if resp.status == 401 && attempt == 0 {
                self.credential.invalidate(SCOPE);
                continue;
            }
            if resp.is_success() {
                return Ok(resp);
            }
            return Err(vault_error(&resp));
        }
        unreachable!("loop returns")
    }

    /// Set a secret; returns the secret bundle.
    pub fn set_secret(&self, name: &str, value: &str) -> Result<Value> {
        let body = crate::util::obj([("value", Value::from(value))]).to_json();
        let resp = self.send(Request::new("PUT", self.url(&format!("/secrets/{}", uri_encode(name, false)))?).header("Content-Type", "application/json").body(body.into_bytes()))?;
        parse_json(&resp.body)
    }

    /// Get a secret bundle (latest version unless `version`).
    pub fn get_secret(&self, name: &str, version: Option<&str>) -> Result<Value> {
        let path = match version {
            Some(v) => format!("/secrets/{}/{}", uri_encode(name, false), uri_encode(v, false)),
            None => format!("/secrets/{}", uri_encode(name, false)),
        };
        let resp = self.send(Request::new("GET", self.url(&path)?))?;
        parse_json(&resp.body)
    }
}

/// Build the exception for a Key Vault error (`{"error": {"code", "message"}}`).
pub fn vault_error(resp: &Response) -> CamelError {
    let v = parse_json(&resp.body).unwrap_or_default();
    let e = v.get("error").cloned().unwrap_or_default();
    let code = e.get("code").map(|c| c.to_string()).unwrap_or_else(|| format!("Http{}", resp.status));
    let msg = e.get("message").map(|m| m.to_string()).unwrap_or_else(|| resp.text());
    cloud_error(kinds::KEY_VAULT, format!("Status code {}, \"{}\" (ErrorCode: {})", resp.status, msg, code))
}

fn version_of(id: &str) -> String {
    id.rsplit('/').next().unwrap_or("").to_string()
}

impl CloudService for KeyVaultService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "getSecret");
        let name = || header_str(ex, headers::SECRET_NAME).ok_or_else(|| CamelError::illegal_argument("Secret name must be specified (CamelAzureKeyVaultSecretName)"));
        match op.as_str() {
            "createSecret" => {
                let n = name()?;
                let v = self.set_secret(&n, &ex.body_as::<String>()?)?;
                let id = v.get("id").map(|i| i.to_string()).unwrap_or_default();
                ex.set_header(headers::SECRET_VERSION, version_of(&id));
                ex.set_body(id);
            }
            "getSecret" => {
                let n = name()?;
                let version = header_str(ex, headers::SECRET_VERSION);
                let v = self.get_secret(&n, version.as_deref())?;
                ex.set_body(v.get("value").map(|x| x.to_string()).unwrap_or_default());
            }
            "deleteSecret" => {
                let resp = self.send(Request::new("DELETE", self.url(&format!("/secrets/{}", uri_encode(&name()?, false)))?))?;
                let v = parse_json(&resp.body)?;
                ex.set_body(v.get("recoveryId").cloned().unwrap_or_default());
            }
            "purgeDeletedSecret" => {
                self.send(Request::new("DELETE", self.url(&format!("/deletedsecrets/{}", uri_encode(&name()?, false)))?))?;
            }
            "recoverDeletedSecret" => {
                let resp = self.send(Request::new("POST", self.url(&format!("/deletedsecrets/{}/recover", uri_encode(&name()?, false)))?))?;
                ex.set_body(parse_json(&resp.body)?.get("id").cloned().unwrap_or_default());
            }
            "listSecrets" => {
                let resp = self.send(Request::new("GET", self.url("/secrets")?))?;
                let v = parse_json(&resp.body)?;
                let names: Vec<Value> = v.get("value").and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().map(|s| Value::from(version_of(&s.get("id").map(|i| i.to_string()).unwrap_or_default()))).collect();
                ex.set_body(Value::Array(names));
            }
            other => return Err(unsupported("azure-key-vault", other)),
        }
        Ok(())
    }
}

/// The `azure:` properties function for a context.
pub fn properties_function(ctx: &CamelContext) -> PropertiesFunction {
    let weak = ctx.downgrade();
    Arc::new(move |r: &str| {
        let ctx = weak.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let p = ctx.properties();
        let get = |k: &str| p.get(&format!("camel.vault.azure.{}", k)).filter(|v| !v.is_empty());
        let vault = get("vaultName").ok_or_else(|| CamelError::illegal_argument("camel.vault.azure.vaultName must be set to use {{azure:...}} placeholders"))?;
        let identity = get("azureIdentityEnabled").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let credential = ClientSecretCredential::new(get("tenantId"), get("clientId"), get("clientSecret"), get("authorityHost"), identity)?;
        let svc = KeyVaultService { vault, credential, operation: None, endpoint: get("uriEndpointOverride"), proxy: Proxy::Environment, trust_all_certificates: false };
        let (name, field, version) = crate::aws::secretsmanager::parse_secret_ref(r);
        match svc.get_secret(&name, version.as_deref()) {
            Ok(v) => Ok(crate::aws::secretsmanager::secret_field(&v.get("value").map(|x| x.to_string()).unwrap_or_default(), field.as_deref())),
            Err(e) if super::error_code(&e).as_deref() == Some("SecretNotFound") => Ok(None),
            Err(e) => Err(e),
        }
    })
}
