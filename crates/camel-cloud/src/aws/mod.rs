//! AWS components (`aws2-s3`, `aws2-sqs`, `aws2-sns`, `aws2-ddb`, `aws2-lambda`) and the
//! shared client configuration.
//!
//! Common endpoint options (also settable as `camel.component.<scheme>.<option>`):
//! `accessKey`, `secretKey`, `sessionToken`, `region` (default `AWS_REGION` /
//! `AWS_DEFAULT_REGION` / `us-east-1`), `useDefaultCredentialsProvider`,
//! `useProfileCredentialsProvider`, `profileCredentialsName`, `useSessionCredentials`,
//! `uriEndpointOverride` + `overrideEndpoint=true` (e.g. a LocalStack or mock URL),
//! `proxyHost`, `proxyPort`, `proxyProtocol`, `trustAllCertificates`.
//!
//! Credentials: explicit `accessKey`/`secretKey` when given, otherwise the default chain:
//! `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`, then the shared
//! credentials file (`AWS_SHARED_CREDENTIALS_FILE` or `~/.aws/credentials`, profile
//! `profileCredentialsName` / `AWS_PROFILE` / `default`). Container and EC2 instance metadata
//! credentials are not supported.

pub mod ddb;
pub mod lambda;
pub mod s3;
pub mod sigv4;
pub mod sns;
pub mod sqs;

use camel_core::error::{CamelError, Result};
use camel_core::value::Value;

use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{cloud_error, kinds, now_secs, Opts, Xml};
pub use sigv4::Credentials;

/// Client configuration shared by all AWS endpoints.
#[derive(Clone, Debug, Default)]
pub struct AwsConfig {
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub session_token: Option<String>,
    pub region: String,
    pub use_default_credentials_provider: bool,
    pub use_profile_credentials_provider: bool,
    pub profile_credentials_name: Option<String>,
    pub uri_endpoint_override: Option<String>,
    pub override_endpoint: bool,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
}

impl AwsConfig {
    /// Read the common options.
    pub fn from_opts(o: &mut Opts) -> Result<AwsConfig> {
        let region = o
            .get("region")
            .or_else(|| std::env::var("AWS_REGION").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "us-east-1".to_string())
            .to_ascii_lowercase()
            .replace('_', "-");
        let proxy = match o.get("proxyHost") {
            Some(h) => {
                let proto = o.string("proxyProtocol", "HTTP").to_ascii_lowercase();
                let port = o.get("proxyPort").unwrap_or_else(|| "3128".into());
                Proxy::Explicit(format!("{}://{}:{}", proto, h, port))
            }
            None => {
                o.ignore(&["proxyPort", "proxyProtocol"]);
                Proxy::Environment
            }
        };
        let cfg = AwsConfig {
            access_key: o.get("accessKey"),
            secret_key: o.get("secretKey"),
            session_token: o.get("sessionToken"),
            region,
            use_default_credentials_provider: o.bool("useDefaultCredentialsProvider", false)?,
            use_profile_credentials_provider: o.bool("useProfileCredentialsProvider", false)?,
            profile_credentials_name: o.get("profileCredentialsName"),
            uri_endpoint_override: o.get("uriEndpointOverride"),
            override_endpoint: o.bool("overrideEndpoint", false)?,
            proxy,
            trust_all_certificates: o.bool("trustAllCertificates", false)?,
        };
        o.ignore(&["useSessionCredentials", "autoDiscoverClient", "useIAMCredentials"]);
        if cfg.override_endpoint && cfg.uri_endpoint_override.is_none() {
            return Err(CamelError::illegal_argument("overrideEndpoint=true requires the uriEndpointOverride option"));
        }
        Ok(cfg)
    }

    /// Resolve credentials (explicit options, then the default chain).
    pub fn credentials(&self) -> Result<Credentials> {
        resolve_credentials(self, &|k| std::env::var(k).ok())
    }

    /// The HTTP client for this configuration.
    pub fn http(&self) -> HttpClient {
        HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates)
    }

    /// Whether a custom endpoint is in use.
    pub fn is_overridden(&self) -> bool {
        self.override_endpoint && self.uri_endpoint_override.is_some()
    }

    /// Base URL of a service (`https://{service}.{region}.amazonaws.com` or the override).
    pub fn endpoint(&self, service_host: &str) -> Result<Url> {
        match (&self.uri_endpoint_override, self.override_endpoint) {
            (Some(u), true) => Url::parse(u),
            _ => {
                let suffix = if self.region.starts_with("cn-") { "amazonaws.com.cn" } else { "amazonaws.com" };
                Url::parse(&format!("https://{}.{}.{}", service_host, self.region, suffix))
            }
        }
    }

    /// Sign (SigV4 for `service`) and send a request; non-2xx responses become errors of `kind`.
    pub fn send(&self, mut req: Request, service: &str, kind: &str) -> Result<Response> {
        let creds = self.credentials()?;
        sigv4::sign_request(&mut req, &creds, &self.region, service, now_secs());
        let resp = self.http().send(&req)?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(service_error(kind, service, &resp))
        }
    }

    /// AWS JSON 1.0/1.1 protocol call (`X-Amz-Target: {target_prefix}.{operation}`).
    pub fn json_call(&self, service_host: &str, service: &str, target_prefix: &str, operation: &str, body: &Value, json_version: &str, kind: &str) -> Result<Value> {
        let req = Request::new("POST", self.endpoint(service_host)?)
            .header("Content-Type", format!("application/x-amz-json-{}", json_version))
            .header("X-Amz-Target", format!("{}.{}", target_prefix, operation))
            .body(body.to_json().into_bytes());
        let resp = self.send(req, service, kind)?;
        crate::util::parse_json(&resp.body)
    }
}

/// Resolve credentials with an injectable environment (for tests).
pub fn resolve_credentials(cfg: &AwsConfig, env: &dyn Fn(&str) -> Option<String>) -> Result<Credentials> {
    if !cfg.use_default_credentials_provider && !cfg.use_profile_credentials_provider {
        if let (Some(a), Some(s)) = (&cfg.access_key, &cfg.secret_key) {
            return Ok(Credentials { access_key: a.clone(), secret_key: s.clone(), session_token: cfg.session_token.clone() });
        }
    }
    let envv = |k: &str| env(k).filter(|v| !v.is_empty());
    if !cfg.use_profile_credentials_provider {
        if let (Some(a), Some(s)) = (envv("AWS_ACCESS_KEY_ID"), envv("AWS_SECRET_ACCESS_KEY").or_else(|| envv("AWS_SECRET_KEY"))) {
            return Ok(Credentials { access_key: a, secret_key: s, session_token: envv("AWS_SESSION_TOKEN") });
        }
    }
    let file = envv("AWS_SHARED_CREDENTIALS_FILE").or_else(|| envv("HOME").map(|h| format!("{}/.aws/credentials", h)));
    let profile = cfg.profile_credentials_name.clone().or_else(|| envv("AWS_PROFILE")).unwrap_or_else(|| "default".into());
    if let Some(text) = file.and_then(|f| std::fs::read_to_string(f).ok()) {
        if let Some(c) = parse_credentials_file(&text, &profile) {
            return Ok(c);
        }
    }
    Err(cloud_error(
        kinds::SDK_CLIENT,
        "Unable to load AWS credentials: set accessKey/secretKey, AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or a shared credentials profile",
    ))
}

/// Parse an AWS shared credentials (INI) file for `profile`.
pub fn parse_credentials_file(text: &str, profile: &str) -> Option<Credentials> {
    let mut current = String::new();
    let (mut ak, mut sk, mut tok) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().trim_start_matches("profile ").to_string();
            continue;
        }
        if current != profile {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().to_string();
            match k.trim() {
                "aws_access_key_id" => ak = Some(v),
                "aws_secret_access_key" => sk = Some(v),
                "aws_session_token" => tok = Some(v),
                _ => {}
            }
        }
    }
    Some(Credentials { access_key: ak?, secret_key: sk?, session_token: tok })
}

/// Error code and message from an AWS error response (XML or JSON).
pub fn error_details(resp: &Response) -> (String, String) {
    let text = resp.text();
    if let Some(t) = resp.header("x-amzn-ErrorType") {
        let code = t.split(':').next().unwrap_or(t).to_string();
        let msg = crate::util::parse_json(&resp.body).ok().and_then(|v| v.get("message").or_else(|| v.get("Message")).map(|m| m.to_string())).unwrap_or_default();
        return (code, msg);
    }
    if text.trim_start().starts_with('{') {
        if let Ok(v) = crate::util::parse_json(&resp.body) {
            let code = v.get("__type").or_else(|| v.get("code")).map(|t| t.to_string()).unwrap_or_default();
            let code = code.rsplit('#').next().unwrap_or("").to_string();
            let msg = v.get("message").or_else(|| v.get("Message")).map(|m| m.to_string()).unwrap_or_default();
            return (code, msg);
        }
    }
    if let Ok(x) = Xml::parse(&text) {
        return (x.find_text("Code").unwrap_or_default(), x.find_text("Message").unwrap_or_default());
    }
    (String::new(), text)
}

/// Build the exception for an error response.
pub fn service_error(kind: &str, service: &str, resp: &Response) -> CamelError {
    let (code, msg) = error_details(resp);
    let code = if code.is_empty() { format!("Http{}", resp.status) } else { code };
    let req_id = resp.header("x-amz-request-id").or_else(|| resp.header("x-amzn-RequestId")).unwrap_or("");
    cloud_error(
        kind,
        format!("{} (Service: {}, Status Code: {}, Error Code: {}, Request ID: {})", if msg.is_empty() { &code } else { &msg }, service, resp.status, code, req_id),
    )
}

/// Error code from a component exception message (`Error Code: X`).
pub fn error_code(e: &CamelError) -> Option<String> {
    e.message.split("Error Code: ").nth(1).map(|r| r.split(',').next().unwrap_or("").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_chain() {
        let mut cfg = AwsConfig { access_key: Some("A".into()), secret_key: Some("S".into()), region: "us-east-1".into(), ..Default::default() };
        let env = |k: &str| match k {
            "AWS_ACCESS_KEY_ID" => Some("ENVA".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("ENVS".to_string()),
            "AWS_SESSION_TOKEN" => Some("TOK".to_string()),
            _ => None,
        };
        assert_eq!(resolve_credentials(&cfg, &env).unwrap().access_key, "A");
        cfg.use_default_credentials_provider = true;
        let c = resolve_credentials(&cfg, &env).unwrap();
        assert_eq!((c.access_key.as_str(), c.session_token.as_deref()), ("ENVA", Some("TOK")));
        let none = |_: &str| None;
        assert!(resolve_credentials(&cfg, &none).unwrap_err().is_a(kinds::SDK));
    }

    #[test]
    fn credentials_file() {
        let text = "[default]\naws_access_key_id = D\naws_secret_access_key = DS\n\n[profile dev]\naws_access_key_id=DEV\naws_secret_access_key=DEVS\naws_session_token=T\n";
        assert_eq!(parse_credentials_file(text, "default").unwrap().access_key, "D");
        let dev = parse_credentials_file(text, "dev").unwrap();
        assert_eq!((dev.secret_key.as_str(), dev.session_token.as_deref()), ("DEVS", Some("T")));
        assert!(parse_credentials_file(text, "prod").is_none());
    }

    #[test]
    fn endpoints_and_errors() {
        let cfg = AwsConfig { region: "eu-central-1".into(), ..Default::default() };
        assert_eq!(cfg.endpoint("sqs").unwrap().to_string(), "https://sqs.eu-central-1.amazonaws.com/");
        let cn = AwsConfig { region: "cn-north-1".into(), ..Default::default() };
        assert_eq!(cn.endpoint("s3").unwrap().host, "s3.cn-north-1.amazonaws.com.cn");
        let resp = Response::xml(403, "<Error><Code>SignatureDoesNotMatch</Code><Message>bad sig</Message></Error>");
        let e = service_error(kinds::S3, "s3", &resp);
        assert!(e.is_a(kinds::AWS_SERVICE));
        assert_eq!(error_code(&e).as_deref(), Some("SignatureDoesNotMatch"));
        let resp = Response::json(400, r#"{"__type":"com.amazonaws.sqs#QueueDoesNotExist","message":"nope"}"#);
        assert_eq!(error_details(&resp), ("QueueDoesNotExist".into(), "nope".into()));
    }
}
