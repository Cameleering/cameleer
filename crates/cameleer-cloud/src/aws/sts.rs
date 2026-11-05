//! The `aws2-sts` component (Camel `camel-aws2-sts`, producer) over the STS Query API.
//!
//! `aws2-sts://label?operation=assumeRole`
//!
//! Operations (`operation` / `CamelAwsStsOperation`, default `assumeRole`):
//! * `assumeRole`: `CamelAwsStsRoleArn`, `CamelAwsStsRoleSessionName`, optional
//!   `CamelAwsStsDurationSeconds`.
//! * `getSessionToken`: optional `CamelAwsStsDurationSeconds`.
//! * `getFederationToken`: `CamelAwsStsFederatedName`, optional policy `CamelAwsStsPolicy`.
//! * `getCallerIdentity`.
//!
//! The body becomes the credentials map `{accessKeyId, secretAccessKey, sessionToken,
//! expiration}` (plus `assumedRoleArn` / `federatedUserArn`; the identity for
//! `getCallerIdentity`), ready to be used as `accessKey`/`secretKey`/`sessionToken` options.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, header_str, kinds, Opts, Xml};

/// Header names (Camel `STS2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsStsOperation";
    pub const ROLE_ARN: &str = "CamelAwsStsRoleArn";
    pub const ROLE_SESSION_NAME: &str = "CamelAwsStsRoleSessionName";
    pub const FEDERATED_NAME: &str = "CamelAwsStsFederatedName";
    /// Cameleer extensions.
    pub const DURATION_SECONDS: &str = "CamelAwsStsDurationSeconds";
    pub const POLICY: &str = "CamelAwsStsPolicy";
}

/// The STS endpoint configuration and client.
pub struct StsService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["stsClient", "pojoRequest"]);
    Ok(Arc::new(StsService { operation: o.get("operation"), aws }))
}

fn credentials_value(x: &Xml) -> Value {
    let mut m = ObjectMap::new();
    if let Some(c) = x.descendants("Credentials").first().copied() {
        let t = |n: &str| Value::from(x.child_text(c, n).unwrap_or_default());
        m.insert("accessKeyId".to_string(), t("AccessKeyId"));
        m.insert("secretAccessKey".to_string(), t("SecretAccessKey"));
        m.insert("sessionToken".to_string(), t("SessionToken"));
        m.insert("expiration".to_string(), t("Expiration"));
    }
    if let Some(arn) = x.descendants("AssumedRoleUser").first().and_then(|u| x.child_text(*u, "Arn")) {
        m.insert("assumedRoleArn".into(), Value::from(arn));
    }
    if let Some(arn) = x.descendants("FederatedUser").first().and_then(|u| x.child_text(*u, "Arn")) {
        m.insert("federatedUserArn".into(), Value::from(arn));
    }
    Value::Object(m)
}

impl StsService {
    /// Invoke an STS action.
    pub fn call(&self, action: &str, params: Vec<(String, String)>) -> Result<Xml> {
        self.aws.query_call(self.aws.endpoint("sts")?, "sts", &self.aws.region.clone(), "2011-06-15", action, params, kinds::STS)
    }
}

impl CloudService for StsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "assumeRole");
        let mut p = Vec::new();
        if let Some(d) = header_i64(ex, headers::DURATION_SECONDS) {
            p.push(("DurationSeconds".to_string(), d.to_string()));
        }
        let x = match op.as_str() {
            "assumeRole" => {
                let arn = header_str(ex, headers::ROLE_ARN).ok_or_else(|| CamelError::illegal_argument("assumeRole requires the CamelAwsStsRoleArn header"))?;
                let name = header_str(ex, headers::ROLE_SESSION_NAME).ok_or_else(|| CamelError::illegal_argument("assumeRole requires the CamelAwsStsRoleSessionName header"))?;
                p.push(("RoleArn".into(), arn));
                p.push(("RoleSessionName".into(), name));
                self.call("AssumeRole", p)?
            }
            "getSessionToken" => self.call("GetSessionToken", p)?,
            "getFederationToken" => {
                let name = header_str(ex, headers::FEDERATED_NAME).ok_or_else(|| CamelError::illegal_argument("getFederationToken requires the CamelAwsStsFederatedName header"))?;
                p.push(("Name".into(), name));
                if let Some(pol) = header_str(ex, headers::POLICY) {
                    p.push(("Policy".into(), pol));
                }
                self.call("GetFederationToken", p)?
            }
            "getCallerIdentity" => {
                let x = self.call("GetCallerIdentity", Vec::new())?;
                let t = |n: &str| Value::from(x.find_text(n).unwrap_or_default());
                ex.set_body(crate::util::obj([("account", t("Account")), ("arn", t("Arn")), ("userId", t("UserId"))]));
                return Ok(());
            }
            other => return Err(unsupported("aws2-sts", other)),
        };
        ex.set_body(credentials_value(&x));
        Ok(())
    }
}
