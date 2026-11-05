//! The `aws2-msk` component (Camel `camel-aws2-msk`, producer) over the Amazon MSK REST API
//! (signing name `kafka`), and the `AWS_MSK_IAM` SASL mechanism for the `kafka` component.
//!
//! `aws2-msk://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsMSKOperation`):
//! * `listClusters` (`CamelAwsMSKClusterFilter` name prefix, all pages): body = list of
//!   cluster infos.
//! * `createCluster`: `CamelAwsMSKClusterName`, `CamelAwsMSKClusterKafkaVersion`,
//!   `CamelAwsMSKBrokerNodesNumber`, `CamelAwsMSKBrokerNodesGroupInfo` (map with
//!   `clientSubnets`, `instanceType`...); body = `{clusterArn, clusterName, state}`, sets
//!   `CamelAwsMSKClusterArn` / `CamelAwsMSKClusterState`.
//! * `describeCluster`, `deleteCluster` (`CamelAwsMSKClusterArn`; body = cluster info /
//!   `{clusterArn, state}`).
//! * Cameleer extension `getBootstrapBrokers` (`CamelAwsMSKClusterArn`): body = the
//!   bootstrap broker strings map (`bootstrapBrokerStringSaslIam`...).
//!
//! ## AWS_MSK_IAM
//!
//! [`register_kafka_iam`] (called by `cameleer_cloud::register`) teaches the `kafka` component
//! the `AWS_MSK_IAM` mechanism of the `aws-msk-iam-auth` library:
//! `kafka:topic?brokers=...&securityProtocol=SASL_SSL&saslMechanism=AWS_MSK_IAM&saslJaasConfig=software.amazon.msk.auth.iam.IAMLoginModule required;`
//! The single SASL message is the JSON payload of a SigV4 presigned `kafka-cluster:Connect`
//! request for the broker host (service `kafka-cluster`, 900 s validity). Credentials come from
//! the default AWS chain, the JAAS `awsProfileName="..."`, or (Cameleer extension, handy for
//! tests) `awsAccessKeyId="..." awsSecretAccessKey="..." [awsSessionToken="..."]`; the region
//! from the JAAS `awsRegion="..."` (extension), the broker host name
//! (`b-1.x.kafka.<region>.amazonaws.com`) or `AWS_REGION`.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::sigv4::{presign_url, Credentials};
use super::{header_json, need, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::Url;
use crate::util::{amz_date, header_i64, header_str, kinds, now_secs, obj, parse_form, uri_encode, Opts};

/// Header names (Camel `MSK2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsMSKOperation";
    pub const CLUSTER_FILTER: &str = "CamelAwsMSKClusterFilter";
    pub const CLUSTER_NAME: &str = "CamelAwsMSKClusterName";
    pub const CLUSTER_ARN: &str = "CamelAwsMSKClusterArn";
    pub const CLUSTER_KAFKA_VERSION: &str = "CamelAwsMSKClusterKafkaVersion";
    pub const BROKER_NODES_NUMBER: &str = "CamelAwsMSKBrokerNodesNumber";
    pub const BROKER_NODES_GROUP_INFO: &str = "CamelAwsMSKBrokerNodesGroupInfo";
    pub const CLUSTER_STATE: &str = "CamelAwsMSKClusterState";
}

/// The MSK endpoint configuration and client.
pub struct MskService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["mskClient", "pojoRequest"]);
    Ok(Arc::new(MskService { operation: o.get("operation"), aws }))
}

impl MskService {
    fn rest(&self, method: &str, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        self.aws.rest_json(method, "kafka", path, query, body, "kafka", kinds::MSK)
    }
}

impl CloudService for MskService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let cluster = |ex: &Exchange| -> Result<String> { Ok(format!("/v1/clusters/{}", uri_encode(&need(ex, headers::CLUSTER_ARN)?, false))) };
        match op.as_str() {
            "listClusters" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut q = Vec::new();
                    if let Some(f) = header_str(ex, headers::CLUSTER_FILTER) {
                        q.push(("clusterNameFilter".to_string(), f));
                    }
                    if let Some(t) = &token {
                        q.push(("nextToken".to_string(), t.clone()));
                    }
                    let r = self.rest("GET", "/v1/clusters", &q, None)?;
                    all.extend(r.get("clusterInfoList").and_then(|c| c.as_array()).cloned().unwrap_or_default());
                    token = r.get("nextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "createCluster" => {
                let mut m = ObjectMap::new();
                m.insert("clusterName".into(), Value::from(need(ex, headers::CLUSTER_NAME)?));
                m.insert("kafkaVersion".into(), Value::from(need(ex, headers::CLUSTER_KAFKA_VERSION)?));
                m.insert("numberOfBrokerNodes".into(), Value::Int(header_i64(ex, headers::BROKER_NODES_NUMBER).ok_or_else(|| CamelError::illegal_argument("The CamelAwsMSKBrokerNodesNumber header is required"))?));
                m.insert("brokerNodeGroupInfo".into(), header_json(ex, headers::BROKER_NODES_GROUP_INFO).unwrap_or_else(|| obj([("clientSubnets", Value::Array(vec![])), ("instanceType", Value::from("kafka.m5.large"))])));
                let r = self.rest("POST", "/v1/clusters", &[], Some(&Value::Object(m)))?;
                if let Some(a) = r.get("clusterArn") {
                    ex.set_header(headers::CLUSTER_ARN, a.clone());
                }
                if let Some(s) = r.get("state") {
                    ex.set_header(headers::CLUSTER_STATE, s.clone());
                }
                ex.set_body(r);
            }
            "describeCluster" => {
                let r = self.rest("GET", &cluster(ex)?, &[], None)?;
                let info = r.get("clusterInfo").cloned().unwrap_or_default();
                if let Some(s) = info.get("state") {
                    ex.set_header(headers::CLUSTER_STATE, s.clone());
                }
                ex.set_body(info);
            }
            "deleteCluster" => {
                let r = self.rest("DELETE", &cluster(ex)?, &[], None)?;
                if let Some(s) = r.get("state") {
                    ex.set_header(headers::CLUSTER_STATE, s.clone());
                }
                ex.set_body(r);
            }
            "getBootstrapBrokers" => {
                let p = format!("{}/bootstrap-brokers", cluster(ex)?);
                let r = self.rest("GET", &p, &[], None)?;
                ex.set_body(r);
            }
            other => return Err(unsupported("MSK", other)),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AWS_MSK_IAM
// ---------------------------------------------------------------------------

/// Version of the MSK IAM authentication payload.
pub const IAM_PAYLOAD_VERSION: &str = "2020_10_22";
/// The action authorized by the payload.
pub const IAM_ACTION: &str = "kafka-cluster:Connect";
const IAM_SERVICE: &str = "kafka-cluster";
const IAM_EXPIRY_SECS: u64 = 900;

/// The `AWS_MSK_IAM` client payload (JSON) for broker `host` at time `secs`.
pub fn iam_auth_payload(creds: &Credentials, region: &str, host: &str, secs: i64) -> Vec<u8> {
    let url = Url { scheme: "https".into(), host: host.to_ascii_lowercase(), port: 443, path: "/".into(), query: Some(format!("Action={}", uri_encode(IAM_ACTION, false))) };
    let signed = presign_url("GET", &url, creds, region, IAM_SERVICE, secs, IAM_EXPIRY_SECS);
    let params = parse_form(signed.query.as_deref().unwrap_or(""));
    let mut m = ObjectMap::new();
    m.insert("version".into(), Value::from(IAM_PAYLOAD_VERSION));
    m.insert("host".into(), Value::from(url.host.clone()));
    m.insert("user-agent".into(), Value::from(concat!("cameleer/", env!("CARGO_PKG_VERSION"))));
    m.insert("action".into(), Value::from(IAM_ACTION));
    for (k, v) in params {
        if k != "Action" {
            m.insert(k.to_ascii_lowercase(), Value::from(v));
        }
    }
    Value::Object(m).to_json().into_bytes()
}

/// Server-side verification of an `AWS_MSK_IAM` payload (used by the test broker): checks
/// the version, action, host, expiry and the SigV4 signature with `secret_for(access key)`.
/// Returns the access key (the principal).
pub fn verify_iam_payload(payload: &[u8], expected_host: Option<&str>, secret_for: &dyn Fn(&str) -> Option<String>, now: i64) -> std::result::Result<String, String> {
    let v = crate::util::parse_json(payload).map_err(|e| format!("invalid payload: {}", e))?;
    let g = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
    if g("version") != IAM_PAYLOAD_VERSION {
        return Err(format!("unsupported version {}", g("version")));
    }
    if g("action") != IAM_ACTION {
        return Err(format!("unexpected action {}", g("action")));
    }
    let host = g("host");
    if let Some(h) = expected_host {
        if !h.eq_ignore_ascii_case(&host) {
            return Err(format!("host mismatch: {}", host));
        }
    }
    let cred = g("x-amz-credential");
    let parts: Vec<&str> = cred.split('/').collect();
    if parts.len() != 5 || parts[3] != IAM_SERVICE || parts[4] != "aws4_request" {
        return Err("malformed credential".into());
    }
    let secret = secret_for(parts[0]).ok_or_else(|| "unknown access key".to_string())?;
    let date = g("x-amz-date");
    let t = crate::util::parse_iso8601(&date).ok_or("bad date")?;
    let expires: i64 = g("x-amz-expires").parse().map_err(|_| "bad expiry")?;
    if now > t + expires || t > now + 300 {
        return Err("signature expired".into());
    }
    let creds = Credentials { access_key: parts[0].into(), secret_key: secret, session_token: v.get("x-amz-security-token").map(|s| s.to_string()) };
    let expected = iam_auth_payload(&creds, parts[2], &host, t);
    let ev = crate::util::parse_json(&expected).map_err(|e| e.to_string())?;
    let sig = ev.get("x-amz-signature").map(|s| s.to_string()).unwrap_or_default();
    if amz_date(t) != date || !crate::util::ct_eq(sig.as_bytes(), g("x-amz-signature").as_bytes()) {
        return Err("signature mismatch".into());
    }
    Ok(parts[0].to_string())
}

/// JAAS value `key="value"` (or unquoted) from a login module configuration line.
fn jaas_value(jaas: &str, key: &str) -> Option<String> {
    let i = jaas.find(&format!("{}=", key))? + key.len() + 1;
    let rest = &jaas[i..];
    match rest.strip_prefix('"') {
        Some(r) => r.find('"').map(|e| r[..e].to_string()),
        None => Some(rest.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string()),
    }
    .filter(|s| !s.is_empty())
}

/// Region of an MSK broker host name (`...kafka.<region>.amazonaws.com`).
pub fn region_of_host(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    let i = labels.iter().position(|l| *l == "kafka" || *l == "kafka-serverless")?;
    labels.get(i + 1).map(|r| r.to_string()).filter(|r| r.contains('-'))
}

/// Build the token provider for a JAAS configuration.
pub fn iam_token_provider(jaas: &str) -> Result<cameleer_kafka_mqtt::kafka::client::SaslTokenProvider> {
    let cfg = AwsConfig {
        access_key: jaas_value(jaas, "awsAccessKeyId"),
        secret_key: jaas_value(jaas, "awsSecretAccessKey"),
        session_token: jaas_value(jaas, "awsSessionToken"),
        profile_credentials_name: jaas_value(jaas, "awsProfileName"),
        use_profile_credentials_provider: jaas_value(jaas, "awsProfileName").is_some(),
        region: String::new(),
        ..Default::default()
    };
    let region = jaas_value(jaas, "awsRegion");
    if jaas.contains("awsRoleArn") {
        return Err(CamelError::new(cameleer_core::error::kinds::UNSUPPORTED_OPERATION, "AWS_MSK_IAM: awsRoleArn (assume role) is not supported by Cameleer; use an STS-issued session token"));
    }
    Ok(Arc::new(move |host: &str| {
        let creds = cfg.credentials()?;
        let region = region
            .clone()
            .or_else(|| region_of_host(host))
            .or_else(|| std::env::var("AWS_REGION").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "us-east-1".into());
        Ok(iam_auth_payload(&creds, &region, host, now_secs()))
    }))
}

/// Register the `AWS_MSK_IAM` SASL mechanism with the kafka component.
pub fn register_kafka_iam() {
    cameleer_kafka_mqtt::kafka::client::register_sasl_mechanism("AWS_MSK_IAM", Arc::new(iam_token_provider));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iam_payload_roundtrip() {
        let creds = Credentials::new("AKID", "SECRET");
        let t = 1_700_000_000;
        let p = iam_auth_payload(&creds, "eu-west-1", "b-1.demo.abc.c2.kafka.eu-west-1.amazonaws.com", t);
        let v = crate::util::parse_json(&p).unwrap();
        assert_eq!(v.get("action").unwrap(), &Value::from("kafka-cluster:Connect"));
        assert_eq!(v.get("x-amz-signedheaders").unwrap(), &Value::from("host"));
        assert!(v.get("x-amz-credential").unwrap().to_string().ends_with("/eu-west-1/kafka-cluster/aws4_request"));
        let secret = |k: &str| (k == "AKID").then(|| "SECRET".to_string());
        assert_eq!(verify_iam_payload(&p, Some("b-1.demo.abc.c2.kafka.eu-west-1.amazonaws.com"), &secret, t + 10).unwrap(), "AKID");
        assert!(verify_iam_payload(&p, None, &|_| Some("WRONG".into()), t + 10).is_err());
        assert!(verify_iam_payload(&p, None, &secret, t + 1000).unwrap_err().contains("expired"));
        assert!(verify_iam_payload(&p, Some("other"), &secret, t).is_err());
        assert_eq!(region_of_host("b-1.demo.abc.c2.kafka.eu-west-1.amazonaws.com").as_deref(), Some("eu-west-1"));
        assert_eq!(region_of_host("127.0.0.1"), None);
        assert_eq!(jaas_value("x required awsRegion=\"us-west-2\" awsProfileName=dev;", "awsProfileName").as_deref(), Some("dev"));
    }
}
