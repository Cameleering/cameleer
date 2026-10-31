//! Azure components: Storage (`azure-storage-blob`, `azure-storage-queue`,
//! `azure-storage-datalake`) with the shared client (Shared Key request signing and SAS
//! tokens), `azure-key-vault` (Azure AD client credentials, [`aad`]) and `azure-cosmosdb`
//! (master-key signing).
//!
//! Common options: `accessKey` (base64 account key), `credentialType`
//! (`SHARED_ACCOUNT_KEY` - default when `accessKey` is set, `AZURE_SAS` - with `sasToken`;
//! `AZURE_IDENTITY` / `SHARED_KEY_CREDENTIAL` beans are not supported), `sasToken`,
//! `uriEndpointOverride` + `overrideEndpoint=true` (Azurite-style path addressing
//! `http://host:port/{account}/...`), `proxyHost`/`proxyPort`, `trustAllCertificates`.
//!
//! `azure-servicebus` / `azure-eventhubs` speak AMQP 1.0 and are not part of this crate.

pub mod aad;
pub mod blob;
pub mod cosmosdb;
pub mod datalake;
pub mod keyvault;
pub mod queue;

use camel_core::error::{CamelError, Result};

use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{base64_decode, base64_encode, cloud_error, hmac_sha256, http_date, now_secs, percent_decode, Opts, Xml};

/// Storage REST API version sent in `x-ms-version`.
pub const API_VERSION: &str = "2021-08-06";

/// How requests are authorized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    /// Shared Key signing with the account key.
    SharedKey(String),
    /// A SAS token appended to every request.
    Sas(String),
}

/// Client configuration shared by the Azure Storage endpoints.
#[derive(Clone, Debug)]
pub struct AzureConfig {
    pub account: String,
    pub credential: Credential,
    pub uri_endpoint_override: Option<String>,
    pub override_endpoint: bool,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
}

impl AzureConfig {
    /// Read the common options.
    pub fn from_opts(account: &str, o: &mut Opts) -> Result<AzureConfig> {
        if account.is_empty() {
            return Err(CamelError::illegal_argument("Azure storage account name must be specified"));
        }
        let ct = o.get("credentialType").map(|c| c.to_ascii_uppercase());
        let key = o.get("accessKey").or_else(|| o.get("accountKey"));
        let sas = o.get("sasToken");
        let credential = match (ct.as_deref(), key, sas) {
            (Some("AZURE_SAS"), _, Some(s)) | (None, None, Some(s)) => Credential::Sas(s.trim_start_matches('?').to_string()),
            (Some("AZURE_SAS"), _, None) => return Err(CamelError::illegal_argument("credentialType=AZURE_SAS requires the sasToken option")),
            (Some("SHARED_ACCOUNT_KEY") | Some("SHARED_KEY_CREDENTIAL") | None, Some(k), _) => {
                if base64_decode(&k).is_none() {
                    return Err(CamelError::illegal_argument("accessKey must be the base64 encoded storage account key"));
                }
                Credential::SharedKey(k)
            }
            (Some("SHARED_ACCOUNT_KEY") | Some("SHARED_KEY_CREDENTIAL"), None, _) | (None, None, None) => {
                return Err(CamelError::illegal_argument("An accessKey (credentialType=SHARED_ACCOUNT_KEY) or a sasToken (credentialType=AZURE_SAS) is required"))
            }
            (Some(other), _, _) => {
                return Err(CamelError::new(
                    camel_core::error::kinds::UNSUPPORTED_OPERATION,
                    format!("credentialType {} is not supported by rust-camel (use SHARED_ACCOUNT_KEY or AZURE_SAS)", other),
                ))
            }
        };
        let proxy = match o.get("proxyHost") {
            Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
            None => {
                o.ignore(&["proxyPort"]);
                Proxy::Environment
            }
        };
        o.ignore(&["serviceClient", "credentials", "timeout", "sourceBlobAccessKey"]);
        Ok(AzureConfig {
            account: account.to_string(),
            credential,
            uri_endpoint_override: o.get("uriEndpointOverride"),
            override_endpoint: o.bool("overrideEndpoint", false)?,
            proxy,
            trust_all_certificates: o.bool("trustAllCertificates", false)?,
        })
    }

    /// Base URL for a service (`blob` / `queue`), without trailing slash.
    pub fn endpoint(&self, service: &str) -> Result<Url> {
        match (&self.uri_endpoint_override, self.override_endpoint) {
            (Some(u), true) => {
                let mut url = Url::parse(u)?;
                url.path = format!("{}/{}", url.path.trim_end_matches('/'), self.account);
                Ok(url)
            }
            _ => Url::parse(&format!("https://{}.{}.core.windows.net", self.account, service)),
        }
    }

    /// URL of `{endpoint}/{path}` with an encoded query.
    pub fn url(&self, service: &str, path: &str, query: &str) -> Result<Url> {
        let mut u = self.endpoint(service)?;
        let base = u.path.trim_end_matches('/').to_string();
        u.path = if path.is_empty() { format!("{}/", base) } else { format!("{}/{}", base, path) };
        u.query = if query.is_empty() { None } else { Some(query.to_string()) };
        Ok(u)
    }

    /// Authorize (Shared Key or SAS) and send; non-2xx responses become errors of `kind`.
    pub fn send(&self, mut req: Request, kind: &str) -> Result<Response> {
        req.set_header("x-ms-date", http_date(now_secs()));
        req.set_header("x-ms-version", API_VERSION);
        match &self.credential {
            Credential::SharedKey(key) => sign_shared_key(&mut req, &self.account, key)?,
            Credential::Sas(token) => {
                req.url.query = Some(match req.url.query.take() {
                    Some(q) if !q.is_empty() => format!("{}&{}", q, token),
                    _ => token.clone(),
                });
            }
        }
        let client = HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates);
        let resp = client.send(&req)?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(storage_error(kind, &resp))
        }
    }
}

/// Build the exception for an Azure error response.
pub fn storage_error(kind: &str, resp: &Response) -> CamelError {
    let (code, msg) = match Xml::parse(&resp.text()) {
        Ok(x) => (x.find_text("Code").unwrap_or_default(), x.find_text("Message").unwrap_or_default()),
        Err(_) => (String::new(), String::new()),
    };
    let code = if code.is_empty() { resp.header("x-ms-error-code").map(|s| s.to_string()).unwrap_or_else(|| format!("Http{}", resp.status)) } else { code };
    cloud_error(kind, format!("Status code {}, \"{}\" (ErrorCode: {})", resp.status, msg.lines().next().unwrap_or(""), code))
}

/// Error code from an Azure exception message.
pub fn error_code(e: &CamelError) -> Option<String> {
    e.message.split("ErrorCode: ").nth(1).map(|r| r.trim_end_matches(')').to_string())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// The Shared Key string to sign (storage services, version 2009-09-19 and later).
pub fn shared_key_string_to_sign(method: &str, headers: &[(String, String)], account: &str, wire_path: &str, raw_query: &str) -> String {
    let len = header(headers, "Content-Length");
    let len = if len == "0" { "" } else { len };
    let mut s = String::new();
    s.push_str(method);
    s.push('\n');
    for h in ["Content-Encoding", "Content-Language"] {
        s.push_str(header(headers, h));
        s.push('\n');
    }
    s.push_str(len);
    s.push('\n');
    for h in ["Content-MD5", "Content-Type", "Date", "If-Modified-Since", "If-Match", "If-None-Match", "If-Unmodified-Since", "Range"] {
        s.push_str(header(headers, h));
        s.push('\n');
    }
    let mut ms: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.to_ascii_lowercase().starts_with("x-ms-"))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    ms.sort();
    for (k, v) in ms {
        s.push_str(&format!("{}:{}\n", k, v));
    }
    s.push_str(&format!("/{}{}", account, wire_path));
    let mut params: Vec<(String, Vec<String>)> = Vec::new();
    for p in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        let k = percent_decode(k, true).to_ascii_lowercase();
        let v = percent_decode(v, true);
        match params.iter_mut().find(|(n, _)| *n == k) {
            Some((_, vs)) => vs.push(v),
            None => params.push((k, vec![v])),
        }
    }
    params.sort();
    for (k, mut vs) in params {
        vs.sort();
        s.push_str(&format!("\n{}:{}", k, vs.join(",")));
    }
    s
}

/// Shared Key signature: base64(HMAC-SHA256(key, string to sign)).
pub fn shared_key_signature(account_key_b64: &str, string_to_sign: &str) -> Result<String> {
    let key = base64_decode(account_key_b64).ok_or_else(|| CamelError::illegal_argument("Invalid base64 account key"))?;
    Ok(base64_encode(&hmac_sha256(&key, string_to_sign.as_bytes())))
}

/// Add the `Authorization: SharedKey account:signature` header. The request must
/// already carry its `x-ms-*` headers; `Content-Length` is derived from the body.
pub fn sign_shared_key(req: &mut Request, account: &str, account_key_b64: &str) -> Result<()> {
    let mut headers = req.headers.clone();
    if !req.body.is_empty() || matches!(req.method.as_str(), "PUT" | "POST") {
        headers.push(("Content-Length".into(), req.body.len().to_string()));
    }
    let sts = shared_key_string_to_sign(&req.method, &headers, account, &req.url.path, req.url.query.as_deref().unwrap_or(""));
    let sig = shared_key_signature(account_key_b64, &sts)?;
    req.set_header("Authorization", format!("SharedKey {}:{}", account, sig));
    Ok(())
}

/// Parameters of an account SAS.
#[derive(Clone, Debug)]
pub struct AccountSas {
    /// Permissions (`rwdlacup...`).
    pub permissions: String,
    /// Services (`b` blob, `q` queue, `t` table, `f` file).
    pub services: String,
    /// Resource types (`s` service, `c` container, `o` object).
    pub resource_types: String,
    pub start: Option<String>,
    /// Expiry (ISO 8601 UTC, e.g. `2030-01-01T00:00:00Z`).
    pub expiry: String,
    pub ip: Option<String>,
    pub protocol: Option<String>,
    pub version: String,
}

impl AccountSas {
    /// A SAS for blob+queue, all resource types and permissions, valid for `secs` seconds.
    pub fn full_access(secs: i64) -> AccountSas {
        AccountSas {
            permissions: "rwdlacup".into(),
            services: "bq".into(),
            resource_types: "sco".into(),
            start: None,
            expiry: crate::util::iso8601(now_secs() + secs),
            ip: None,
            protocol: None,
            version: API_VERSION.into(),
        }
    }

    /// The string to sign (version 2020-12-06 and later layout).
    pub fn string_to_sign(&self, account: &str) -> String {
        [
            account,
            &self.permissions,
            &self.services,
            &self.resource_types,
            self.start.as_deref().unwrap_or(""),
            &self.expiry,
            self.ip.as_deref().unwrap_or(""),
            self.protocol.as_deref().unwrap_or(""),
            &self.version,
            "",
            "",
        ]
        .join("\n")
    }

    /// The SAS token query string (without `?`).
    pub fn token(&self, account: &str, account_key_b64: &str) -> Result<String> {
        let sig = shared_key_signature(account_key_b64, &self.string_to_sign(account))?;
        let enc = |s: &str| crate::util::uri_encode(s, false);
        let mut q = vec![format!("sv={}", enc(&self.version)), format!("ss={}", self.services), format!("srt={}", self.resource_types), format!("sp={}", self.permissions)];
        if let Some(st) = &self.start {
            q.push(format!("st={}", enc(st)));
        }
        q.push(format!("se={}", enc(&self.expiry)));
        if let Some(ip) = &self.ip {
            q.push(format!("sip={}", enc(ip)));
        }
        if let Some(p) = &self.protocol {
            q.push(format!("spr={}", enc(p)));
        }
        q.push(format!("sig={}", enc(&sig)));
        Ok(q.join("&"))
    }

    /// Parse the SAS fields of a query (inverse of [`AccountSas::token`]); returns it with the signature.
    pub fn parse(params: &[(String, String)]) -> Option<(AccountSas, String)> {
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        Some((
            AccountSas {
                permissions: get("sp")?,
                services: get("ss")?,
                resource_types: get("srt")?,
                start: get("st"),
                expiry: get("se")?,
                ip: get("sip"),
                protocol: get("spr"),
                version: get("sv")?,
            },
            get("sig")?,
        ))
    }
}

/// Parse `PT30S`-style ISO durations, plain seconds or Camel durations into seconds.
pub fn duration_secs(v: &str) -> Result<i64> {
    let t = v.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Ok(n);
    }
    if let Some(iso) = t.strip_prefix("PT").or_else(|| t.strip_prefix("pt")) {
        let mut total = 0i64;
        let mut num = String::new();
        for c in iso.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else {
                let n: i64 = num.parse().unwrap_or(0);
                num.clear();
                total += match c.to_ascii_uppercase() {
                    'H' => n * 3600,
                    'M' => n * 60,
                    _ => n,
                };
            }
        }
        return Ok(total);
    }
    Ok(camel_core::util::parse_duration(t)?.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_sign_layout() {
        let headers = vec![
            ("x-ms-date".to_string(), "Fri, 26 Jun 2015 23:39:12 GMT".to_string()),
            ("x-ms-version".to_string(), "2015-02-21".to_string()),
            ("Content-Length".to_string(), "0".to_string()),
        ];
        let s = shared_key_string_to_sign("GET", &headers, "myaccount", "/mycontainer", "restype=container&comp=metadata");
        assert_eq!(
            s,
            "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2015-02-21\n/myaccount/mycontainer\ncomp:metadata\nrestype:container"
        );
        // documented example: "?comp=list&include=metadata&include=snapshots" style multi-values
        let s = shared_key_string_to_sign("GET", &[], "acc", "/c", "include=snapshots&include=metadata&COMP=list");
        assert!(s.ends_with("/acc/c\ncomp:list\ninclude:metadata,snapshots"));
    }

    #[test]
    fn sas_token_roundtrip() {
        let key = base64_encode(b"0123456789abcdef0123456789abcdef");
        let sas = AccountSas { expiry: "2030-01-01T00:00:00Z".into(), ..AccountSas::full_access(10) };
        let token = sas.token("acc", &key).unwrap();
        assert!(token.starts_with("sv=2021-08-06&ss=bq&srt=sco&sp=rwdlacup&se=2030-01-01T00%3A00%3A00Z&sig="));
        let params = crate::util::parse_form(&token);
        let (parsed, sig) = AccountSas::parse(&params).unwrap();
        assert_eq!(shared_key_signature(&key, &parsed.string_to_sign("acc")).unwrap(), sig);
    }

    #[test]
    fn durations() {
        assert_eq!(duration_secs("30").unwrap(), 30);
        assert_eq!(duration_secs("PT1M30S").unwrap(), 90);
        assert_eq!(duration_secs("2s").unwrap(), 2);
    }
}
