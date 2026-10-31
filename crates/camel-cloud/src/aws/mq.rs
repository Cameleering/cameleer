//! The `aws2-mq` component (Camel `camel-aws2-mq`, producer) over the Amazon MQ REST API
//! (signing name `mq`).
//!
//! `aws2-mq://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsMQOperation`):
//! * `listBrokers` (`CamelAwsMQMaxResults`): body = list of broker summaries (all pages).
//! * `createBroker`: `CamelAwsMQBrokerName`, `...BrokerEngine` (default `ACTIVEMQ`),
//!   `...BrokerEngineVersion`, `...BrokerInstanceType`, `...BrokerDeploymentMode` (default
//!   `SINGLE_INSTANCE`), `...BrokerPubliclyAccessible`, `...BrokerUsers` (list of
//!   `{username, password, consoleAccess}`); body = `{brokerArn, brokerId}`, sets
//!   `CamelAwsMQBrokerId`.
//! * `describeBroker`, `deleteBroker`, `rebootBroker` (`CamelAwsMQBrokerId`).

use std::sync::Arc;

use camel_core::error::Result;
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{header_json, need, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, header_str, kinds, uri_encode, Opts};

/// Header names (Camel `MQ2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsMQOperation";
    pub const MAX_RESULTS: &str = "CamelAwsMQMaxResults";
    pub const BROKER_NAME: &str = "CamelAwsMQBrokerName";
    pub const BROKER_ID: &str = "CamelAwsMQBrokerId";
    pub const BROKER_DEPLOYMENT_MODE: &str = "CamelAwsMQBrokerDeploymentMode";
    pub const BROKER_INSTANCE_TYPE: &str = "CamelAwsMQBrokerInstanceType";
    pub const BROKER_ENGINE: &str = "CamelAwsMQBrokerEngine";
    pub const BROKER_ENGINE_VERSION: &str = "CamelAwsMQBrokerEngineVersion";
    pub const BROKER_USERS: &str = "CamelAwsMQBrokerUsers";
    pub const BROKER_PUBLICLY_ACCESSIBLE: &str = "CamelAwsMQBrokerPubliclyAccessible";
}

/// The Amazon MQ endpoint configuration and client.
pub struct MqService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["amazonMqClient", "pojoRequest"]);
    Ok(Arc::new(MqService { operation: o.get("operation"), aws }))
}

impl MqService {
    fn rest(&self, method: &str, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        self.aws.rest_json(method, "mq", path, query, body, "mq", kinds::MQ)
    }
}

impl CloudService for MqService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let broker_path = |ex: &Exchange| -> Result<String> { Ok(format!("/v1/brokers/{}", uri_encode(&need(ex, headers::BROKER_ID)?, false))) };
        match op.as_str() {
            "listBrokers" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut q = Vec::new();
                    if let Some(m) = header_i64(ex, headers::MAX_RESULTS) {
                        q.push(("maxResults".to_string(), m.to_string()));
                    }
                    if let Some(t) = &token {
                        q.push(("nextToken".to_string(), t.clone()));
                    }
                    let r = self.rest("GET", "/v1/brokers", &q, None)?;
                    all.extend(r.get("brokerSummaries").and_then(|b| b.as_array()).cloned().unwrap_or_default());
                    token = r.get("nextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "createBroker" => {
                let mut m = ObjectMap::new();
                m.insert("brokerName".into(), Value::from(need(ex, headers::BROKER_NAME)?));
                m.insert("engineType".into(), Value::from(header_str(ex, headers::BROKER_ENGINE).unwrap_or_else(|| "ACTIVEMQ".into())));
                m.insert("engineVersion".into(), Value::from(header_str(ex, headers::BROKER_ENGINE_VERSION).unwrap_or_else(|| "5.18".into())));
                m.insert("hostInstanceType".into(), Value::from(header_str(ex, headers::BROKER_INSTANCE_TYPE).unwrap_or_else(|| "mq.t3.micro".into())));
                m.insert("deploymentMode".into(), Value::from(header_str(ex, headers::BROKER_DEPLOYMENT_MODE).unwrap_or_else(|| "SINGLE_INSTANCE".into())));
                m.insert("publiclyAccessible".into(), Value::Bool(ex.header(headers::BROKER_PUBLICLY_ACCESSIBLE).is_some_and(|v| v.is_truthy())));
                m.insert("autoMinorVersionUpgrade".into(), Value::Bool(false));
                m.insert("users".into(), header_json(ex, headers::BROKER_USERS).unwrap_or(Value::Array(vec![])));
                let r = self.rest("POST", "/v1/brokers", &[], Some(&Value::Object(m)))?;
                if let Some(id) = r.get("brokerId") {
                    ex.set_header(headers::BROKER_ID, id.clone());
                }
                ex.set_body(r);
            }
            "describeBroker" => {
                let r = self.rest("GET", &broker_path(ex)?, &[], None)?;
                ex.set_body(r);
            }
            "deleteBroker" => {
                let r = self.rest("DELETE", &broker_path(ex)?, &[], None)?;
                ex.set_body(r);
            }
            "rebootBroker" => {
                let p = format!("{}/reboot", broker_path(ex)?);
                self.rest("POST", &p, &[], None)?;
            }
            other => return Err(unsupported("MQ", other)),
        }
        Ok(())
    }
}
