//! AWS components (`aws2-s3`, `aws2-sqs`, `aws2-sns`, `aws2-ddb`, `aws2-ddbstream`, `aws2-lambda`,
//! `aws2-kinesis`, `aws2-kinesis-firehose`, `aws2-eventbridge`, `aws-secrets-manager`,
//! `aws2-ses`, `aws2-cw`, `aws2-sts`, `aws2-step-functions`, `aws2-iam`, `aws2-ec2`,
//! `aws2-msk`, `aws2-athena`, `aws2-textract`, `aws2-translate`, `aws2-comprehend`,
//! `aws2-polly`, `aws2-rekognition`, `aws2-transcribe`, `aws-bedrock`,
//! `aws-bedrock-agent-runtime`, `aws2-mq`, `aws2-ecs`, `aws2-eks`, `aws2-kms`,
//! `aws2-timestream`, `aws-cloudtrail`, `aws-config`, `aws2-redshift-data`), the `AWS_MSK_IAM`
//! Kafka SASL mechanism ([`msk`]) and the shared client configuration.
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
//! `profileCredentialsName` / `AWS_PROFILE` / `default`), then web identity federation
//! (`AWS_WEB_IDENTITY_TOKEN_FILE` + `AWS_ROLE_ARN`, STS `AssumeRoleWithWebIdentity`), the
//! container credentials endpoint and the EC2 instance metadata service ([`credentials`]).

pub mod athena;
pub mod bedrock;
pub mod cloudtrail;
pub mod comprehend;
pub mod config;
pub mod credentials;
pub mod cw;
pub mod ddb;
pub mod ddbstream;
pub mod ec2;
pub mod ecs;
pub mod eventbridge;
pub mod eventstream;
pub mod firehose;
pub mod iam;
pub mod kinesis;
pub mod kinesis_kcl;
pub mod kms;
pub mod lambda;
pub mod mq;
pub mod msk;
pub mod polly;
pub mod redshiftdata;
pub mod rekognition;
pub mod s3;
pub mod s3_multipart;
pub mod s3_ops;
pub mod secretsmanager;
pub mod ses;
pub mod sigv4;
pub mod sns;
pub mod sqs;
pub mod stepfunctions;
pub mod sts;
pub mod textract;
pub mod timestream;
pub mod transcribe;
pub mod translate;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

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

    /// Endpoint of a global service (`https://{host}` unless overridden, e.g. `iam.amazonaws.com`).
    pub fn global_endpoint(&self, host: &str) -> Result<Url> {
        match (&self.uri_endpoint_override, self.override_endpoint) {
            (Some(u), true) => Url::parse(u),
            _ => Url::parse(&format!("https://{}", host)),
        }
    }

    /// Sign (SigV4 for `service`) and send a request; non-2xx responses become errors of `kind`.
    pub fn send(&self, req: Request, service: &str, kind: &str) -> Result<Response> {
        self.send_in_region(req, service, &self.region.clone(), kind)
    }

    /// [`AwsConfig::send`] signing for an explicit region (global services sign for `us-east-1`).
    pub fn send_in_region(&self, mut req: Request, service: &str, region: &str, kind: &str) -> Result<Response> {
        let creds = self.credentials()?;
        sigv4::sign_request(&mut req, &creds, region, service, now_secs());
        let resp = self.http().send(&req)?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(service_error(kind, service, &resp))
        }
    }

    /// Send a request whose body is streamed from `body` (`len` bytes), signed with
    /// `UNSIGNED-PAYLOAD`.
    pub fn send_streaming(&self, mut req: Request, service: &str, kind: &str, body: &mut dyn std::io::Read, len: u64) -> Result<Response> {
        let creds = self.credentials()?;
        req.set_header("x-amz-content-sha256", sigv4::UNSIGNED_PAYLOAD);
        sigv4::sign_request(&mut req, &creds, &self.region, service, now_secs());
        let resp = self.http().send_streaming(&req, body, len)?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(service_error(kind, service, &resp))
        }
    }

    /// Send a request and stream a successful response body into `sink`; returns the
    /// response (with an empty body) and the number of bytes written.
    pub fn send_to(&self, mut req: Request, service: &str, kind: &str, sink: &mut dyn std::io::Write) -> Result<(Response, u64)> {
        let creds = self.credentials()?;
        sigv4::sign_request(&mut req, &creds, &self.region, service, now_secs());
        let (resp, n) = self.http().send_to(&req, sink)?;
        if resp.is_success() {
            Ok((resp, n))
        } else {
            Err(service_error(kind, service, &resp))
        }
    }

    /// REST call whose successful response is an AWS event stream: every decoded message
    /// is passed to `on_message` as soon as it arrives; exception messages become errors.
    #[allow(clippy::too_many_arguments)]
    pub fn rest_event_stream(
        &self,
        method: &str,
        service_host: &str,
        path: &str,
        body: Option<&Value>,
        service: &str,
        kind: &str,
        on_message: &mut dyn FnMut(eventstream::Message) -> Result<()>,
    ) -> Result<Response> {
        let mut url = self.endpoint(service_host)?;
        url.path = format!("{}{}", url.path.trim_end_matches('/'), path);
        let mut req = Request::new(method, url).header("Accept", "application/vnd.amazon.eventstream");
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let k = kind.to_string();
        let mut decoder = eventstream::Decoder::new(|m: eventstream::Message| match m.error(&k) {
            Some(e) => Err(e),
            None => on_message(m),
        });
        let res = self.send_to(req, service, kind, &mut decoder);
        if let Some(e) = decoder.error.take() {
            return Err(e);
        }
        let (resp, _) = res?;
        decoder.finish()?;
        Ok(resp)
    }

    /// AWS JSON call over **HTTP/2** whose response is an event stream (operations AWS only
    /// serves over HTTP/2, e.g. Kinesis `SubscribeToShard`): TLS endpoints negotiate ALPN `h2`,
    /// `http://` overrides use h2c with prior knowledge. Messages are decoded as they arrive
    /// and handed to `on_message`; returning `Ok(false)` from it (or `stop()` becoming true)
    /// cancels the stream. Exception messages become errors.
    #[allow(clippy::too_many_arguments)]
    pub fn h2_event_stream(
        &self,
        service_host: &str,
        service: &str,
        target: &str,
        body: &Value,
        json_version: &str,
        kind: &str,
        stop: &dyn Fn() -> bool,
        on_message: &mut dyn FnMut(eventstream::Message) -> Result<bool>,
    ) -> Result<()> {
        use simoom_client as sc;
        let url = self.endpoint(service_host)?;
        let mut req = Request::new("POST", url.clone())
            .header("Content-Type", format!("application/x-amz-json-{}", json_version))
            .header("X-Amz-Target", target)
            .body(body.to_json().into_bytes());
        let creds = self.credentials()?;
        sigv4::sign_request(&mut req, &creds, &self.region, service, now_secs());
        let h2err = |e: sc::Error| cloud_error(kind, format!("HTTP/2 error talking to {}: {}", url.origin(), e));
        // HTTP/2 only: TLS endpoints negotiate ALPN h2 and verify the certificate, `http://` overrides use prior knowledge.
        // The system trust store is loaded only when the endpoint is `https`.
        let mut cfg = sc::Config::default();
        cfg.version = sc::HttpVersion::Http2;
        // the loop below polls with its own deadlines
        cfg.read_timeout = None;
        let tls = if url.is_https() {
            sc::tls::TlsOptions::default()
        } else {
            sc::tls::TlsOptions { roots: sc::tls::RootStore::empty(), client_cert: None, server_name: None, resumption: false }
        };
        let client = sc::ClientBuilder::default().config(cfg).tls(tls).build();
        let mut headers = sc::Headers::new();
        for (k, v) in &req.headers {
            // `host` becomes the :authority
            if !matches!(k.to_ascii_lowercase().as_str(), "connection" | "content-length") {
                headers.append(k, v);
            }
        }
        let full = format!("{}://{}{}", if url.is_https() { "https" } else { "http" }, url.authority(), url.target());
        let result = (|| -> Result<()> {
            let mut call = client.call("POST", &full, &headers).map_err(h2err)?;
            call.send(&req.body).map_err(h2err)?;
            call.finish().map_err(h2err)?;
            let (status, head) = {
                let (s, h) = call.response_until(Some(std::time::Instant::now() + std::time::Duration::from_secs(30))).map_err(h2err)?;
                (s, h.to_strings())
            };
            let mut buf = vec![0u8; 16 * 1024];
            if status != 200 {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let mut body = Vec::new();
                while let Ok(n) = call.read_until(&mut buf, Some(until)) {
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                let resp = Response { status, headers: head, body };
                return Err(service_error(kind, service, &resp));
            }
            let k = kind.to_string();
            let cancelled = std::cell::Cell::new(false);
            let mut decoder = eventstream::Decoder::new(|m: eventstream::Message| {
                if let Some(e) = m.error(&k) {
                    return Err(e);
                }
                if !on_message(m)? {
                    cancelled.set(true);
                }
                Ok(())
            });
            loop {
                if stop() {
                    call.cancel();
                    return Ok(());
                }
                match call.read_until(&mut buf, Some(std::time::Instant::now() + std::time::Duration::from_millis(100))) {
                    Ok(0) => break,
                    Ok(n) => decoder.push(&buf[..n])?,
                    Err(e) if e.is_timeout() => continue,
                    Err(e) => return Err(h2err(e)),
                }
                if cancelled.get() {
                    break;
                }
            }
            drop(decoder);
            if cancelled.get() {
                call.cancel();
            }
            Ok(())
        })();
        client.close_idle();
        result
    }

    /// AWS Query protocol call (`Action`/`Version` form POST); returns the XML response.
    pub fn query_call(&self, url: Url, service: &str, region: &str, version: &str, action: &str, params: Vec<(String, String)>, kind: &str) -> Result<Xml> {
        let mut all = vec![("Action".to_string(), action.to_string()), ("Version".to_string(), version.to_string())];
        all.extend(params);
        let req = Request::new("POST", url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=utf-8")
            .body(crate::util::encode_form(&all).into_bytes());
        let resp = self.send_in_region(req, service, region, kind)?;
        Xml::parse(&resp.text())
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

    /// Call an AWS JSON protocol API described by `api`.
    pub fn api(&self, api: &JsonApi, operation: &str, body: &Value) -> Result<Value> {
        self.json_call(api.host, api.service, api.target, operation, body, api.version, api.kind)
    }

    /// Call a paginated AWS JSON API: follows `token_field` (`NextToken` / `nextToken`) and
    /// concatenates the `list_field` arrays.
    pub fn api_paged(&self, api: &JsonApi, operation: &str, req: ObjectMap, list_field: &str, token_field: &str) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut r = req.clone();
            if let Some(t) = &token {
                r.insert(token_field.to_string(), Value::from(t.clone()));
            }
            let v = self.api(api, operation, &Value::Object(r))?;
            all.extend(v.get(list_field).and_then(|a| a.as_array()).cloned().unwrap_or_default());
            token = v.get(token_field).map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() {
                return Ok(all);
            }
        }
    }

    /// AWS REST-JSON call: `method` on `{endpoint(service_host)}{path}?{query}`, JSON body,
    /// JSON response.
    #[allow(clippy::too_many_arguments)]
    pub fn rest_json(&self, method: &str, service_host: &str, path: &str, query: &[(String, String)], body: Option<&Value>, service: &str, kind: &str) -> Result<Value> {
        let resp = self.rest_call(method, service_host, path, query, body.map(|b| ("application/json", b.to_json().into_bytes())), &[], service, kind)?;
        crate::util::parse_json(&resp.body)
    }

    /// AWS REST call with a raw body `(content type, bytes)` and extra headers.
    #[allow(clippy::too_many_arguments)]
    pub fn rest_call(
        &self,
        method: &str,
        service_host: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<(&str, Vec<u8>)>,
        headers: &[(&str, String)],
        service: &str,
        kind: &str,
    ) -> Result<Response> {
        let mut url = self.endpoint(service_host)?;
        url.path = format!("{}{}", url.path.trim_end_matches('/'), path);
        url.query = if query.is_empty() { None } else { Some(crate::util::encode_form(query)) };
        let mut req = Request::new(method, url);
        for (k, v) in headers {
            req.set_header(k, v.clone());
        }
        if let Some((ct, b)) = body {
            req = req.header("Content-Type", ct).body(b);
        }
        self.send(req, service, kind)
    }
}

/// Description of an AWS JSON 1.0/1.1 API: endpoint host prefix, signing name,
/// `X-Amz-Target` prefix, JSON version and exception kind.
#[derive(Clone, Copy, Debug)]
pub struct JsonApi {
    pub host: &'static str,
    pub service: &'static str,
    pub target: &'static str,
    pub version: &'static str,
    pub kind: &'static str,
}

/// A required header.
pub fn need(ex: &cameleer_core::message::Exchange, h: &str) -> Result<String> {
    crate::util::header_str(ex, h).ok_or_else(|| CamelError::illegal_argument(format!("The {} header is required", h)))
}

/// A header, falling back to an endpoint option.
pub fn hdr(ex: &cameleer_core::message::Exchange, h: &str, option: &Option<String>) -> Option<String> {
    crate::util::header_str(ex, h).or_else(|| option.clone())
}

/// A required header or endpoint option.
pub fn need_or(ex: &cameleer_core::message::Exchange, h: &str, option: &Option<String>, option_name: &str) -> Result<String> {
    hdr(ex, h, option).ok_or_else(|| CamelError::illegal_argument(format!("The {} header (or the {} option) is required", h, option_name)))
}

/// Insert `key` into `m` when `v` is present.
pub fn put(m: &mut ObjectMap, key: &str, v: Option<impl Into<Value>>) {
    if let Some(v) = v {
        m.insert(key.to_string(), v.into());
    }
}

/// A header as a JSON value: objects/arrays as is, JSON text parsed, other scalars as is.
pub fn header_json(ex: &cameleer_core::message::Exchange, h: &str) -> Option<Value> {
    match ex.header(h)? {
        Value::Null => None,
        Value::String(s) if s.trim_start().starts_with(['{', '[']) => cameleer_core::json::parse(s).ok(),
        v => Some(v.clone()),
    }
}

/// The body as a JSON value (maps/lists as is, JSON text parsed); None otherwise.
pub fn body_json(ex: &cameleer_core::message::Exchange) -> Option<Value> {
    match ex.body() {
        Value::Null => None,
        Value::Object(_) | Value::Array(_) => Some(ex.body().clone()),
        Value::Bytes(b) => cameleer_core::json::parse(&String::from_utf8_lossy(b)).ok().filter(|v| matches!(v, Value::Object(_) | Value::Array(_))),
        Value::String(s) => cameleer_core::json::parse(s).ok().filter(|v| matches!(v, Value::Object(_) | Value::Array(_))),
        _ => None,
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
    if !cfg.use_profile_credentials_provider {
        let sts_override = cfg.is_overridden().then(|| cfg.uri_endpoint_override.clone()).flatten();
        if let Some(c) = credentials::web_identity_credentials(&envv, &cfg.region, sts_override.as_deref()) {
            return Ok(c);
        }
        if let Some(c) = credentials::container_credentials(&envv).or_else(|| credentials::imds_credentials(&envv)) {
            return Ok(c);
        }
    }
    Err(cloud_error(
        kinds::SDK_CLIENT,
        "Unable to load AWS credentials: set accessKey/secretKey, AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, a shared credentials profile, or run with container / instance profile credentials",
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
        let none = |k: &str| (k == "AWS_EC2_METADATA_DISABLED").then(|| "true".to_string());
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
