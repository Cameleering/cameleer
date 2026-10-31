//! The `aws-secrets-manager` component (Camel `camel-aws-secrets-manager`, producer) over the
//! Secrets Manager AWS JSON 1.1 protocol, and the `{{aws:...}}` properties function.
//!
//! `aws-secrets-manager://label?operation=...&binaryPayload=false`
//!
//! Operations (`operation` / `CamelAwsSecretsManagerOperation`, default `getSecret`):
//! * `createSecret`: name `CamelAwsSecretsManagerSecretName`, value = body (string, or bytes
//!   as `SecretBinary` with `binaryPayload=true`), `CamelAwsSecretsManagerSecretDescription`;
//!   body = secret ARN, `CamelAwsSecretsManagerSecretVersionId`.
//! * `getSecret`: id `CamelAwsSecretsManagerSecretId` (name or ARN), optional
//!   `CamelAwsSecretsManagerSecretVersionId`; body = secret string (bytes for binary secrets).
//! * `describeSecret`, `listSecrets` (body = list of secret entries), `deleteSecret` (body =
//!   ARN; `CamelAwsSecretsManagerForceDeletion` for `ForceDeleteWithoutRecovery`),
//!   `restoreSecret`, `updateSecret` (body = new value), `rotateSecret` (rotation lambda
//!   `CamelAwsSecretsManagerLambdaRotationFunctionArn`, `CamelAwsSecretsManagerRotateImmediately`),
//!   `replicateSecretToRegions` (`CamelAwsSecretsManagerSecretReplicationRegions`).
//!
//! Properties function (registered by [`crate::register`]): `{{aws:name}}`,
//! `{{aws:name/field}}` (field of a JSON secret), `{{aws:name@version}}` and
//! `{{aws:name/field:default}}`. Configuration from the context properties
//! `camel.vault.aws.accessKey`, `secretKey`, `region`, `defaultCredentialsProvider`,
//! `profileCredentialsProvider`, `profileName`, and (as in recent Camel versions)
//! `uriEndpointOverride` + `overrideEndpoint`.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::context::{CamelContext, PropertiesFunction};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::{error_code, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, header_list, header_str, kinds, obj, parse_json, Opts};

/// Header names (Camel `SecretsManagerConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsSecretsManagerOperation";
    pub const SECRET_NAME: &str = "CamelAwsSecretsManagerSecretName";
    pub const SECRET_DESCRIPTION: &str = "CamelAwsSecretsManagerSecretDescription";
    pub const SECRET_ID: &str = "CamelAwsSecretsManagerSecretId";
    pub const SECRET_VERSION_ID: &str = "CamelAwsSecretsManagerSecretVersionId";
    pub const LAMBDA_ROTATION_FUNCTION_ARN: &str = "CamelAwsSecretsManagerLambdaRotationFunctionArn";
    pub const SECRET_REPLICATION_REGIONS: &str = "CamelAwsSecretsManagerSecretReplicationRegions";
    /// rust-camel extensions.
    pub const FORCE_DELETION: &str = "CamelAwsSecretsManagerForceDeletion";
    pub const ROTATE_IMMEDIATELY: &str = "CamelAwsSecretsManagerRotateImmediately";
}

/// A Secrets Manager client.
pub struct SecretsManagerService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub binary_payload: bool,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["secretsManagerClient", "pojoRequest"]);
    Ok(Arc::new(SecretsManagerService { operation: o.get("operation"), binary_payload: o.bool("binaryPayload", false)?, aws }))
}

impl SecretsManagerService {
    /// Invoke a Secrets Manager API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("secretsmanager", "secretsmanager", "secretsmanager", action, &body, "1.1", kinds::SECRETS_MANAGER)
    }

    /// `GetSecretValue`: the secret string, or the decoded binary.
    pub fn get_secret_value(&self, id: &str, version: Option<&str>) -> Result<Value> {
        let mut m = BTreeMap::new();
        m.insert("SecretId".to_string(), Value::from(id));
        if let Some(v) = version {
            m.insert("VersionId".to_string(), Value::from(v));
        }
        let r = self.call("GetSecretValue", Value::Object(m))?;
        if let Some(s) = r.get("SecretString").filter(|s| !s.is_null()) {
            return Ok(Value::from(s.to_string()));
        }
        Ok(Value::Bytes(r.get("SecretBinary").and_then(|b| base64_decode(&b.to_string())).unwrap_or_default()))
    }

    fn secret_id(ex: &Exchange) -> Result<String> {
        header_str(ex, headers::SECRET_ID)
            .or_else(|| header_str(ex, headers::SECRET_NAME))
            .ok_or_else(|| CamelError::illegal_argument("Secret id must be specified (CamelAwsSecretsManagerSecretId)"))
    }

    fn payload(&self, ex: &Exchange, m: &mut BTreeMap<String, Value>) {
        if self.binary_payload {
            m.insert("SecretBinary".into(), Value::from(base64_encode(&ex.body().to_bytes())));
        } else {
            m.insert("SecretString".into(), Value::from(ex.body().to_string()));
        }
    }
}

impl CloudService for SecretsManagerService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "getSecret");
        match op.as_str() {
            "createSecret" => {
                let name = header_str(ex, headers::SECRET_NAME).ok_or_else(|| CamelError::illegal_argument("createSecret requires the CamelAwsSecretsManagerSecretName header"))?;
                let mut m = BTreeMap::new();
                m.insert("Name".to_string(), Value::from(name));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                if let Some(d) = header_str(ex, headers::SECRET_DESCRIPTION) {
                    m.insert("Description".into(), Value::from(d));
                }
                self.payload(ex, &mut m);
                let r = self.call("CreateSecret", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "getSecret" => {
                let id = Self::secret_id(ex)?;
                let version = header_str(ex, headers::SECRET_VERSION_ID);
                let v = self.get_secret_value(&id, version.as_deref())?;
                ex.set_body(v);
            }
            "describeSecret" => {
                let r = self.call("DescribeSecret", obj([("SecretId", Value::from(Self::secret_id(ex)?))]))?;
                ex.set_body(r);
            }
            "listSecrets" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let req = match &token {
                        Some(t) => obj([("NextToken", Value::from(t.clone()))]),
                        None => obj([]),
                    };
                    let r = self.call("ListSecrets", req)?;
                    all.extend(r.get("SecretList").and_then(|l| l.as_array()).cloned().unwrap_or_default());
                    token = r.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "deleteSecret" => {
                let mut m = BTreeMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                if ex.header(headers::FORCE_DELETION).is_some_and(|v| v.is_truthy()) {
                    m.insert("ForceDeleteWithoutRecovery".into(), Value::Bool(true));
                }
                let r = self.call("DeleteSecret", Value::Object(m))?;
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "restoreSecret" => {
                let r = self.call("RestoreSecret", obj([("SecretId", Value::from(Self::secret_id(ex)?))]))?;
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "updateSecret" => {
                let mut m = BTreeMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                self.payload(ex, &mut m);
                let r = self.call("PutSecretValue", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "rotateSecret" => {
                let mut m = BTreeMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                if let Some(arn) = header_str(ex, headers::LAMBDA_ROTATION_FUNCTION_ARN) {
                    m.insert("RotationLambdaARN".into(), Value::from(arn));
                }
                if let Some(v) = ex.header(headers::ROTATE_IMMEDIATELY) {
                    m.insert("RotateImmediately".into(), Value::Bool(v.is_truthy()));
                }
                let r = self.call("RotateSecret", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "replicateSecretToRegions" => {
                let regions: Vec<Value> = header_list(ex, headers::SECRET_REPLICATION_REGIONS).into_iter().map(|r| obj([("Region", Value::from(r))])).collect();
                let r = self.call("ReplicateSecretToRegions", obj([("SecretId", Value::from(Self::secret_id(ex)?)), ("AddReplicaRegions", Value::Array(regions))]))?;
                ex.set_body(r);
            }
            other => return Err(unsupported("aws-secrets-manager", other)),
        }
        Ok(())
    }
}

/// Split a vault reference `name[/field][@version]` into its parts.
pub fn parse_secret_ref(r: &str) -> (String, Option<String>, Option<String>) {
    let (rest, version) = match r.split_once('@') {
        Some((a, v)) => (a, Some(v.to_string()).filter(|v| !v.is_empty())),
        None => (r, None),
    };
    match rest.split_once('/') {
        Some((n, f)) => (n.to_string(), Some(f.to_string()).filter(|f| !f.is_empty()), version),
        None => (rest.to_string(), None, version),
    }
}

/// Select `field` of a JSON secret (the whole secret when `field` is None).
pub fn secret_field(secret: &str, field: Option<&str>) -> Option<String> {
    match field {
        None => Some(secret.to_string()),
        Some(f) => {
            let v = parse_json(secret.as_bytes()).ok()?;
            let x = v.get(f)?;
            Some(if matches!(x, Value::Object(_) | Value::Array(_)) { x.to_json() } else { x.to_string() })
        }
    }
}

fn vault_config(ctx: &CamelContext) -> Result<AwsConfig> {
    let p = ctx.properties();
    let get = |k: &str| p.get(&format!("camel.vault.aws.{}", k)).filter(|v| !v.is_empty());
    let flag = |k: &str| get(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let mut cfg = AwsConfig {
        access_key: get("accessKey"),
        secret_key: get("secretKey"),
        session_token: get("sessionToken"),
        region: get("region").or_else(|| std::env::var("AWS_REGION").ok().filter(|r| !r.is_empty())).unwrap_or_else(|| "us-east-1".into()),
        use_default_credentials_provider: flag("defaultCredentialsProvider"),
        use_profile_credentials_provider: flag("profileCredentialsProvider"),
        profile_credentials_name: get("profileName"),
        uri_endpoint_override: get("uriEndpointOverride"),
        override_endpoint: flag("overrideEndpoint"),
        ..Default::default()
    };
    if cfg.uri_endpoint_override.is_some() && get("overrideEndpoint").is_none() {
        cfg.override_endpoint = true;
    }
    Ok(cfg)
}

/// The `aws:` properties function for a context.
pub fn properties_function(ctx: &CamelContext) -> PropertiesFunction {
    let weak = ctx.downgrade();
    Arc::new(move |r: &str| {
        let ctx = weak.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let (name, field, version) = parse_secret_ref(r);
        let svc = SecretsManagerService { aws: vault_config(&ctx)?, operation: None, binary_payload: false };
        match svc.get_secret_value(&name, version.as_deref()) {
            Ok(v) => Ok(secret_field(&v.to_string(), field.as_deref())),
            Err(e) if error_code(&e).as_deref() == Some("ResourceNotFoundException") => Ok(None),
            Err(e) => Err(e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_references() {
        assert_eq!(parse_secret_ref("db"), ("db".into(), None, None));
        assert_eq!(parse_secret_ref("db/user@v2"), ("db".into(), Some("user".into()), Some("v2".into())));
        assert_eq!(secret_field(r#"{"user":"scott","port":5432}"#, Some("port")).as_deref(), Some("5432"));
        assert_eq!(secret_field("plain", None).as_deref(), Some("plain"));
        assert_eq!(secret_field(r#"{"user":"scott"}"#, Some("nope")), None);
    }
}
