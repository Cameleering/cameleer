//! Remote credential providers of the AWS default chain: web identity tokens (STS
//! `AssumeRoleWithWebIdentity`), the ECS/EKS container credentials endpoint and the EC2
//! instance metadata service (IMDSv2, falling back to IMDSv1).
//!
//! * Container: `AWS_CONTAINER_CREDENTIALS_FULL_URI`, or `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`
//!   relative to `http://169.254.170.2`; the `Authorization` header comes from
//!   `AWS_CONTAINER_AUTHORIZATION_TOKEN` or the file named by
//!   `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE`.
//! * Instance metadata: `AWS_EC2_METADATA_SERVICE_ENDPOINT` (default `http://169.254.169.254`),
//!   disabled with `AWS_EC2_METADATA_DISABLED=true`. A session token is obtained with
//!   `PUT /latest/api/token`, then the role name and the role credentials are read from
//!   `/latest/meta-data/iam/security-credentials/`.
//!
//! Credentials are cached process-wide per source until five minutes before they expire; an
//! unreachable metadata endpoint is remembered for a minute so that the chain stays fast off EC2.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use cameleer_core::value::Value;

use super::Credentials;
use crate::http::{HttpClient, Proxy, Request, Url};
use crate::util::{now_secs, parse_iso8601, parse_json};

/// Default container credentials host.
pub const CONTAINER_HOST: &str = "http://169.254.170.2";
/// Default instance metadata endpoint.
pub const IMDS_ENDPOINT: &str = "http://169.254.169.254";

enum Cached {
    Creds(Credentials, i64),
    Unavailable(i64),
}

fn cache() -> &'static Mutex<HashMap<String, Cached>> {
    static C: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Forget all cached remote credentials (tests).
pub fn clear_cache() {
    cache().lock().unwrap().clear();
}

fn client() -> HttpClient {
    HttpClient::new().with_proxy(Proxy::None).with_timeout(Duration::from_secs(2))
}

fn cached(key: &str, fetch: impl FnOnce() -> Result<CredsWithExpiry, String>) -> Option<Credentials> {
    let now = now_secs();
    if let Some(c) = cache().lock().unwrap().get(key) {
        match c {
            Cached::Creds(c, exp) if *exp > now + 300 => return Some(c.clone()),
            Cached::Unavailable(until) if *until > now => return None,
            _ => {}
        }
    }
    match fetch() {
        Ok((c, exp)) => {
            cache().lock().unwrap().insert(key.to_string(), Cached::Creds(c.clone(), exp));
            Some(c)
        }
        Err(e) => {
            cameleer_core::camel_debug!("cameleer-cloud.aws", "Credentials from {} unavailable: {}", key, e);
            cache().lock().unwrap().insert(key.to_string(), Cached::Unavailable(now + 60));
            None
        }
    }
}

type CredsWithExpiry = (Credentials, i64);

/// Parse a credentials document (`AccessKeyId`, `SecretAccessKey`, `Token`, `Expiration`).
pub fn parse_credentials_json(v: &Value) -> Result<CredsWithExpiry, String> {
    let s = |k: &str| v.get(k).map(|x| x.to_string()).filter(|x| !x.is_empty());
    if let Some(code) = s("Code") {
        if code != "Success" {
            return Err(format!("credentials endpoint returned Code={}", code));
        }
    }
    let ak = s("AccessKeyId").ok_or("missing AccessKeyId")?;
    let sk = s("SecretAccessKey").ok_or("missing SecretAccessKey")?;
    let exp = s("Expiration").and_then(|e| parse_iso8601(&e)).unwrap_or(now_secs() + 3600);
    Ok((Credentials { access_key: ak, secret_key: sk, session_token: s("Token") }, exp))
}

/// Container credentials, when the environment points at a container credentials endpoint.
pub fn container_credentials(env: &dyn Fn(&str) -> Option<String>) -> Option<Credentials> {
    let url = match (env("AWS_CONTAINER_CREDENTIALS_FULL_URI"), env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")) {
        (Some(full), _) => full,
        (None, Some(rel)) => format!("{}{}", CONTAINER_HOST, rel),
        _ => return None,
    };
    let token = env("AWS_CONTAINER_AUTHORIZATION_TOKEN").or_else(|| env("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE").and_then(|f| std::fs::read_to_string(f).ok()).map(|t| t.trim().to_string()));
    cached(&format!("container:{}", url), || {
        let mut req = Request::new("GET", Url::parse(&url).map_err(|e| e.to_string())?).header("Accept", "application/json");
        if let Some(t) = &token {
            req.set_header("Authorization", t.clone());
        }
        let resp = client().send(&req).map_err(|e| e.to_string())?;
        if !resp.is_success() {
            return Err(format!("HTTP {}", resp.status));
        }
        parse_credentials_json(&parse_json(&resp.body).map_err(|e| e.to_string())?)
    })
}

/// Instance profile credentials from the instance metadata service.
pub fn imds_credentials(env: &dyn Fn(&str) -> Option<String>) -> Option<Credentials> {
    if env("AWS_EC2_METADATA_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return None;
    }
    let endpoint = env("AWS_EC2_METADATA_SERVICE_ENDPOINT").unwrap_or_else(|| IMDS_ENDPOINT.to_string());
    let base = endpoint.trim_end_matches('/').to_string();
    cached(&format!("imds:{}", base), || {
        let http = client();
        let url = |p: &str| Url::parse(&format!("{}{}", base, p)).map_err(|e| e.to_string());
        // IMDSv2 session token (IMDSv1 when the endpoint does not support it)
        let tok_resp = http.send(&Request::new("PUT", url("/latest/api/token")?).header("X-aws-ec2-metadata-token-ttl-seconds", "21600")).map_err(|e| e.to_string())?;
        let token = match tok_resp.status {
            200 => Some(tok_resp.text().trim().to_string()),
            403 | 404 | 405 => None,
            s => return Err(format!("token request failed with HTTP {}", s)),
        };
        let get = |p: &str| -> Result<String, String> {
            let mut req = Request::new("GET", url(p)?);
            if let Some(t) = &token {
                req.set_header("X-aws-ec2-metadata-token", t.clone());
            }
            let resp = http.send(&req).map_err(|e| e.to_string())?;
            if !resp.is_success() {
                return Err(format!("GET {} failed with HTTP {}", p, resp.status));
            }
            Ok(resp.text())
        };
        let roles = get("/latest/meta-data/iam/security-credentials/")?;
        let role = roles.lines().map(|l| l.trim()).find(|l| !l.is_empty()).ok_or("no instance profile role")?.to_string();
        let doc = get(&format!("/latest/meta-data/iam/security-credentials/{}", role))?;
        parse_credentials_json(&parse_json(doc.as_bytes()).map_err(|e| e.to_string())?)
    })
}

/// Web identity credentials (EKS IRSA / workload identity federation): the OIDC token of
/// `AWS_WEB_IDENTITY_TOKEN_FILE` is exchanged for temporary credentials of `AWS_ROLE_ARN`
/// (session `AWS_ROLE_SESSION_NAME`) with the unsigned STS `AssumeRoleWithWebIdentity`
/// call. The STS endpoint is `AWS_ENDPOINT_URL_STS` / `AWS_ENDPOINT_URL`, else `sts_override`,
/// else `https://sts.{region}.amazonaws.com`. Cached until five minutes before expiry (the
/// token file is re-read on every refresh, as Kubernetes rotates it).
pub fn web_identity_credentials(env: &dyn Fn(&str) -> Option<String>, region: &str, sts_override: Option<&str>) -> Option<Credentials> {
    let token_file = env("AWS_WEB_IDENTITY_TOKEN_FILE")?;
    let role = env("AWS_ROLE_ARN")?;
    let session = env("AWS_ROLE_SESSION_NAME").unwrap_or_else(|| format!("camel-{}", now_secs()));
    let endpoint = env("AWS_ENDPOINT_URL_STS").or_else(|| env("AWS_ENDPOINT_URL")).or_else(|| sts_override.map(|s| s.to_string())).unwrap_or_else(|| format!("https://sts.{}.amazonaws.com", region));
    cached(&format!("web-identity:{}:{}:{}", endpoint, role, token_file), || {
        let token = std::fs::read_to_string(&token_file).map_err(|e| format!("cannot read {}: {}", token_file, e))?;
        let form = crate::util::encode_form(&[
            ("Action".to_string(), "AssumeRoleWithWebIdentity".to_string()),
            ("Version".to_string(), "2011-06-15".to_string()),
            ("RoleArn".to_string(), role.clone()),
            ("RoleSessionName".to_string(), session.clone()),
            ("WebIdentityToken".to_string(), token.trim().to_string()),
        ]);
        let req = Request::new("POST", Url::parse(&endpoint).map_err(|e| e.to_string())?).header("Content-Type", "application/x-www-form-urlencoded; charset=utf-8").body(form.into_bytes());
        let resp = HttpClient::new().with_timeout(Duration::from_secs(10)).send(&req).map_err(|e| e.to_string())?;
        let x = crate::util::Xml::parse(&resp.text()).map_err(|e| e.to_string())?;
        if !resp.is_success() {
            return Err(format!("AssumeRoleWithWebIdentity failed with HTTP {}: {} {}", resp.status, x.find_text("Code").unwrap_or_default(), x.find_text("Message").unwrap_or_default()));
        }
        let t = |n: &str| x.find_text(n).filter(|v| !v.is_empty()).ok_or_else(|| format!("missing {}", n));
        let exp = t("Expiration").ok().and_then(|e| parse_iso8601(&e)).unwrap_or(now_secs() + 3600);
        Ok((Credentials { access_key: t("AccessKeyId")?, secret_key: t("SecretAccessKey")?, session_token: Some(t("SessionToken")?) }, exp))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_document() {
        let v = parse_json(br#"{"Code":"Success","AccessKeyId":"ASIA1","SecretAccessKey":"s","Token":"t","Expiration":"2030-01-01T00:00:00Z"}"#).unwrap();
        let (c, exp) = parse_credentials_json(&v).unwrap();
        assert_eq!((c.access_key.as_str(), c.session_token.as_deref()), ("ASIA1", Some("t")));
        assert_eq!(exp, parse_iso8601("2030-01-01T00:00:00Z").unwrap());
        let bad = parse_json(br#"{"Code":"AssumeRoleUnauthorizedAccess"}"#).unwrap();
        assert!(parse_credentials_json(&bad).is_err());
        let disabled = |k: &str| (k == "AWS_EC2_METADATA_DISABLED").then(|| "true".to_string());
        assert!(imds_credentials(&disabled).is_none());
        assert!(container_credentials(&|_| None).is_none());
    }
}
