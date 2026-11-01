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
//! Keys and certificates (rust-camel operations; Camel's component only manages secrets), over
//! the `/keys` and `/certificates` APIs:
//! * `createKey` (`CamelAzureKeyVaultKeyName`, `CamelAzureKeyVaultKeyType` `EC` (default) /
//!   `RSA` / `EC-HSM` / `RSA-HSM`, `CamelAzureKeyVaultKeyCurve` (`P-256` default for EC),
//!   `CamelAzureKeyVaultKeySize`, `CamelAzureKeyVaultKeyOperations`), `getKey`
//!   (`CamelAzureKeyVaultKeyVersion`), `listKeys` (names), `deleteKey` (recovery id): body = the
//!   key bundle (`key` JWK with `kid`, `attributes`), header `CamelAzureKeyVaultKeyVersion`.
//! * `signData` / `verifyData`: the body is hashed for `CamelAzureKeyVaultSignatureAlgorithm`
//!   (`ES256` default, `ES384`, `ES512`, `RS256`, `RS384`, `RS512`, `PS256`...) and the digest is
//!   signed by the vault (body = signature bytes) or verified against
//!   `CamelAzureKeyVaultSignature` (bytes or base64; body = boolean).
//! * `createCertificate` (`CamelAzureKeyVaultCertificateName`, `CamelAzureKeyVaultCertificateSubject`
//!   default `CN=<name>`, `CamelAzureKeyVaultCertificateValidityMonths` default 12; self-issued,
//!   the pending operation is polled until completed), `getCertificate`
//!   (`CamelAzureKeyVaultCertificateVersion`), `importCertificate` (body = PEM or base64
//!   PKCS#12, optional `CamelAzureKeyVaultCertificatePassword`), `listCertificates` (names),
//!   `deleteCertificate`: body = the certificate bundle (`cer` = base64 DER, `x5t`, `policy`).
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
    /// rust-camel extensions (keys and certificates).
    pub const KEY_NAME: &str = "CamelAzureKeyVaultKeyName";
    pub const KEY_VERSION: &str = "CamelAzureKeyVaultKeyVersion";
    pub const KEY_TYPE: &str = "CamelAzureKeyVaultKeyType";
    pub const KEY_CURVE: &str = "CamelAzureKeyVaultKeyCurve";
    pub const KEY_SIZE: &str = "CamelAzureKeyVaultKeySize";
    pub const KEY_OPERATIONS: &str = "CamelAzureKeyVaultKeyOperations";
    pub const SIGNATURE_ALGORITHM: &str = "CamelAzureKeyVaultSignatureAlgorithm";
    pub const SIGNATURE: &str = "CamelAzureKeyVaultSignature";
    pub const CERTIFICATE_NAME: &str = "CamelAzureKeyVaultCertificateName";
    pub const CERTIFICATE_VERSION: &str = "CamelAzureKeyVaultCertificateVersion";
    pub const CERTIFICATE_SUBJECT: &str = "CamelAzureKeyVaultCertificateSubject";
    pub const CERTIFICATE_VALIDITY_MONTHS: &str = "CamelAzureKeyVaultCertificateValidityMonths";
    pub const CERTIFICATE_PASSWORD: &str = "CamelAzureKeyVaultCertificatePassword";
}

/// Digest of `data` for a Key Vault signature algorithm.
pub fn signature_digest(alg: &str, data: &[u8]) -> Result<Vec<u8>> {
    use camel_tls::crypto::hash::{Digest, Sha256, Sha384, Sha512};
    Ok(match alg.get(2..).unwrap_or("") {
        "256" | "256K" => Sha256::digest(data),
        "384" => Sha384::digest(data),
        "512" => Sha512::digest(data),
        _ => return Err(CamelError::illegal_argument(format!("Unsupported signature algorithm: {}", alg))),
    })
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
    let opts: std::collections::HashMap<&str, Option<String>> = ["tenantId", "clientId", "clientSecret", "authorityHost", "federatedTokenFile", "managedIdentityClientId", "managedIdentityEndpoint", "managedIdentityHeader"].into_iter().map(|k| (k, o.get(k))).collect();
    let credential = ClientSecretCredential::resolve(&|k: &str| opts.get(k).cloned().flatten(), ct == "AZURE_IDENTITY")?;
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
            "createKey" | "getKey" | "listKeys" | "deleteKey" | "signData" | "verifyData" => self.key_op(ex, &op)?,
            "createCertificate" | "getCertificate" | "importCertificate" | "listCertificates" | "deleteCertificate" => self.certificate_op(ex, &op)?,
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

impl KeyVaultService {
    fn json(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = Request::new(method, self.url(path)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let resp = self.send(req)?;
        if resp.body.is_empty() {
            return Ok(Value::Null);
        }
        parse_json(&resp.body)
    }

    fn names(&self, path: &str) -> Result<Value> {
        let mut out = Vec::new();
        let mut next = Some(self.url(path)?.to_string());
        while let Some(u) = next.take() {
            let resp = self.send(Request::new("GET", Url::parse(&u)?))?;
            let v = parse_json(&resp.body)?;
            out.extend(v.get("value").and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().map(|s| Value::from(version_of(&s.get("id").or_else(|| s.get("kid")).map(|i| i.to_string()).unwrap_or_default()))));
            next = v.get("nextLink").map(|n| n.to_string()).filter(|n| !n.is_empty());
        }
        Ok(Value::Array(out))
    }

    fn key_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        if op == "listKeys" {
            let names = self.names("/keys")?;
            ex.set_body(names);
            return Ok(());
        }
        let name = header_str(ex, headers::KEY_NAME).ok_or_else(|| CamelError::illegal_argument("Key name must be specified (CamelAzureKeyVaultKeyName)"))?;
        let n = uri_encode(&name, false);
        let version = header_str(ex, headers::KEY_VERSION);
        let vpath = match &version {
            Some(v) => format!("/keys/{}/{}", n, uri_encode(v, false)),
            None => format!("/keys/{}", n),
        };
        match op {
            "createKey" => {
                let kty = header_str(ex, headers::KEY_TYPE).unwrap_or_else(|| "EC".into()).to_ascii_uppercase();
                let mut m = camel_core::value::ObjectMap::new();
                m.insert("kty".into(), Value::from(kty.clone()));
                if kty.starts_with("EC") {
                    m.insert("crv".into(), Value::from(header_str(ex, headers::KEY_CURVE).unwrap_or_else(|| "P-256".into())));
                }
                if let Some(sz) = crate::util::header_i64(ex, headers::KEY_SIZE) {
                    m.insert("key_size".into(), Value::Int(sz));
                }
                let ops = crate::util::header_list(ex, headers::KEY_OPERATIONS);
                if !ops.is_empty() {
                    m.insert("key_ops".into(), Value::from(ops));
                }
                let v = self.json("POST", &format!("/keys/{}/create", n), Some(Value::Object(m)))?;
                Self::key_headers(ex, &v);
                ex.set_body(v);
            }
            "getKey" => {
                let v = self.json("GET", &vpath, None)?;
                Self::key_headers(ex, &v);
                ex.set_body(v);
            }
            "deleteKey" => {
                let v = self.json("DELETE", &format!("/keys/{}", n), None)?;
                ex.set_body(v.get("recoveryId").cloned().unwrap_or_default());
            }
            _ => {
                let alg = header_str(ex, headers::SIGNATURE_ALGORITHM).unwrap_or_else(|| "ES256".into()).to_ascii_uppercase();
                let digest = signature_digest(&alg, &ex.body().to_bytes())?;
                let vp = if version.is_some() { vpath.clone() } else { format!("{}/", vpath) };
                if op == "signData" {
                    let v = self.json("POST", &format!("{}/sign", vp.trim_end_matches('/')), Some(crate::util::obj([("alg", Value::from(alg)), ("value", Value::from(crate::util::base64url_encode(&digest)))])))?;
                    let sig = v.get("value").map(|s| b64url_decode(&s.to_string())).transpose()?.unwrap_or_default();
                    ex.set_body(Value::Bytes(sig));
                } else {
                    let sig = match ex.header(headers::SIGNATURE) {
                        Some(Value::Bytes(b)) => b.clone(),
                        Some(v) => b64url_decode(&v.to_string())?,
                        None => return Err(CamelError::illegal_argument("verifyData requires the CamelAzureKeyVaultSignature header")),
                    };
                    let v = self.json(
                        "POST",
                        &format!("{}/verify", vp.trim_end_matches('/')),
                        Some(crate::util::obj([("alg", Value::from(alg)), ("digest", Value::from(crate::util::base64url_encode(&digest))), ("value", Value::from(crate::util::base64url_encode(&sig)))])),
                    )?;
                    ex.set_body(Value::Bool(v.get("value").and_then(|b| b.as_bool()).unwrap_or(false)));
                }
            }
        }
        Ok(())
    }

    fn key_headers(ex: &mut Exchange, v: &Value) {
        if let Some(kid) = v.get("key").and_then(|k| k.get("kid")) {
            ex.set_header(headers::KEY_VERSION, version_of(&kid.to_string()));
        }
    }

    fn certificate_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        if op == "listCertificates" {
            let names = self.names("/certificates")?;
            ex.set_body(names);
            return Ok(());
        }
        let name = header_str(ex, headers::CERTIFICATE_NAME).ok_or_else(|| CamelError::illegal_argument("Certificate name must be specified (CamelAzureKeyVaultCertificateName)"))?;
        let n = uri_encode(&name, false);
        let bundle = match op {
            "createCertificate" => {
                let subject = header_str(ex, headers::CERTIFICATE_SUBJECT).unwrap_or_else(|| format!("CN={}", name));
                let months = crate::util::header_i64(ex, headers::CERTIFICATE_VALIDITY_MONTHS).unwrap_or(12);
                let policy = crate::util::obj([
                    ("key_props", crate::util::obj([("kty", Value::from("EC")), ("crv", Value::from("P-256")), ("exportable", Value::Bool(true)), ("reuse_key", Value::Bool(false))])),
                    ("x509_props", crate::util::obj([("subject", Value::from(subject)), ("validity_months", Value::Int(months))])),
                    ("issuer", crate::util::obj([("name", Value::from("Self"))])),
                ]);
                let mut op_state = self.json("POST", &format!("/certificates/{}/create", n), Some(crate::util::obj([("policy", policy)])))?;
                for _ in 0..50 {
                    match op_state.get("status").map(|s| s.to_string()).as_deref() {
                        Some("completed") => break,
                        Some("inProgress") | None => {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            op_state = self.json("GET", &format!("/certificates/{}/pending", n), None)?;
                        }
                        Some(other) => {
                            return Err(cloud_error(kinds::KEY_VAULT, format!("Certificate creation {}: {}", other, op_state.get("status_details").map(|d| d.to_string()).unwrap_or_default())));
                        }
                    }
                }
                self.json("GET", &format!("/certificates/{}", n), None)?
            }
            "getCertificate" => {
                let path = match header_str(ex, headers::CERTIFICATE_VERSION) {
                    Some(v) => format!("/certificates/{}/{}", n, uri_encode(&v, false)),
                    None => format!("/certificates/{}", n),
                };
                self.json("GET", &path, None)?
            }
            "importCertificate" => {
                let body = ex.body().to_bytes();
                let text = String::from_utf8_lossy(&body).to_string();
                let (value, ct) = if text.contains("-----BEGIN") { (text, "application/x-pem-file") } else { (crate::util::base64_encode(&body), "application/x-pkcs12") };
                let mut m = camel_core::value::ObjectMap::new();
                m.insert("value".into(), Value::from(value));
                m.insert("policy".into(), crate::util::obj([("secret_props", crate::util::obj([("contentType", Value::from(ct))]))]));
                if let Some(p) = header_str(ex, headers::CERTIFICATE_PASSWORD) {
                    m.insert("pwd".into(), Value::from(p));
                }
                self.json("POST", &format!("/certificates/{}/import", n), Some(Value::Object(m)))?
            }
            _ => self.json("DELETE", &format!("/certificates/{}", n), None)?,
        };
        if let Some(id) = bundle.get("id") {
            ex.set_header(headers::CERTIFICATE_VERSION, version_of(&id.to_string()));
        }
        ex.set_body(bundle);
        Ok(())
    }
}

fn b64url_decode(s: &str) -> Result<Vec<u8>> {
    let mut t: String = s.trim().replace('-', "+").replace('_', "/");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    crate::util::base64_decode(&t).ok_or_else(|| CamelError::illegal_argument("Invalid base64url value"))
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
        let credential = ClientSecretCredential::resolve(&get, identity)?;
        let svc = KeyVaultService { vault, credential, operation: None, endpoint: get("uriEndpointOverride"), proxy: Proxy::Environment, trust_all_certificates: false };
        let (name, field, version) = crate::aws::secretsmanager::parse_secret_ref(r);
        match svc.get_secret(&name, version.as_deref()) {
            Ok(v) => Ok(crate::aws::secretsmanager::secret_field(&v.get("value").map(|x| x.to_string()).unwrap_or_default(), field.as_deref())),
            Err(e) if super::error_code(&e).as_deref() == Some("SecretNotFound") => Ok(None),
            Err(e) => Err(e),
        }
    })
}
