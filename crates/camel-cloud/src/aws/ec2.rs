//! The `aws2-ec2` component (Camel `camel-aws2-ec2`, producer subset) over the EC2 Query API
//! (XML responses).
//!
//! `aws2-ec2://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsEC2Operation`, required): `describeInstances`
//! (`CamelAwsEC2InstancesIds` list, optional), `describeInstancesStatus`,
//! `createAndRunInstances` (`CamelAwsEC2ImageId`, `CamelAwsEC2InstanceType`,
//! `CamelAwsEC2InstanceMinCount` / `MaxCount` (default 1), `CamelAwsEC2InstanceKeyPair`),
//! `startInstances`, `stopInstances`, `terminateInstances`, `rebootInstances`
//! (`CamelAwsEC2InstancesIds`). Bodies: lists of instances `{instanceId, imageId,
//! instanceType, state, stateCode, launchTime, privateIpAddress, keyName}` or of state
//! changes `{instanceId, previousState, currentState}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_i64, header_list, header_str, kinds, obj, Opts, Xml};

/// Header names (Camel `AWS2EC2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsEC2Operation";
    pub const IMAGE_ID: &str = "CamelAwsEC2ImageId";
    pub const INSTANCE_TYPE: &str = "CamelAwsEC2InstanceType";
    pub const INSTANCE_MIN_COUNT: &str = "CamelAwsEC2InstanceMinCount";
    pub const INSTANCE_MAX_COUNT: &str = "CamelAwsEC2InstanceMaxCount";
    pub const INSTANCES_IDS: &str = "CamelAwsEC2InstancesIds";
    pub const INSTANCES_KEY_PAIR: &str = "CamelAwsEC2InstanceKeyPair";
}

/// API version.
pub const VERSION: &str = "2016-11-15";

/// The EC2 endpoint configuration and client.
pub struct Ec2Service {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["amazonEc2Client", "pojoRequest"]);
    Ok(Arc::new(Ec2Service { operation: o.get("operation"), aws }))
}

/// Parse the instances of a `DescribeInstances` / `RunInstances` response.
pub fn parse_instances(x: &Xml) -> Vec<Value> {
    let mut out = Vec::new();
    for set in x.descendants("instancesSet") {
        for item in x.children(set, "item") {
            let t = |n: &str| Value::from(x.child_text(item, n).unwrap_or_default());
            let state = x.child(item, "instanceState");
            let st = |n: &str| state.and_then(|s| x.child_text(s, n)).unwrap_or_default();
            let mut m = BTreeMap::new();
            m.insert("instanceId".to_string(), t("instanceId"));
            m.insert("imageId".to_string(), t("imageId"));
            m.insert("instanceType".to_string(), t("instanceType"));
            m.insert("launchTime".to_string(), t("launchTime"));
            m.insert("privateIpAddress".to_string(), t("privateIpAddress"));
            m.insert("keyName".to_string(), t("keyName"));
            m.insert("state".to_string(), Value::from(st("name")));
            m.insert("stateCode".to_string(), Value::Int(st("code").parse().unwrap_or(0)));
            out.push(Value::Object(m));
        }
    }
    out
}

fn state_changes(x: &Xml) -> Value {
    let mut out = Vec::new();
    for set in x.descendants("instancesSet") {
        for item in x.children(set, "item") {
            let st = |n: &str| x.child(item, n).and_then(|s| x.child_text(s, "name")).unwrap_or_default();
            out.push(obj([
                ("instanceId", Value::from(x.child_text(item, "instanceId").unwrap_or_default())),
                ("previousState", Value::from(st("previousState"))),
                ("currentState", Value::from(st("currentState"))),
            ]));
        }
    }
    Value::Array(out)
}

impl Ec2Service {
    /// Invoke an EC2 action.
    pub fn call(&self, action: &str, params: Vec<(String, String)>) -> Result<Xml> {
        self.aws.query_call(self.aws.endpoint("ec2")?, "ec2", &self.aws.region.clone(), VERSION, action, params, kinds::EC2)
    }
}

fn ids(ex: &Exchange) -> Vec<(String, String)> {
    header_list(ex, headers::INSTANCES_IDS).into_iter().enumerate().map(|(i, id)| (format!("InstanceId.{}", i + 1), id)).collect()
}

impl CloudService for Ec2Service {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        match op.as_str() {
            "describeInstances" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut p = ids(ex);
                    if let Some(t) = &token {
                        p.push(("NextToken".into(), t.clone()));
                    }
                    let x = self.call("DescribeInstances", p)?;
                    all.extend(parse_instances(&x));
                    token = x.find_text("nextToken").filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "describeInstancesStatus" => {
                let x = self.call("DescribeInstanceStatus", ids(ex))?;
                let mut out = Vec::new();
                for set in x.descendants("instanceStatusSet") {
                    for item in x.children(set, "item") {
                        let state = x.child(item, "instanceState").and_then(|s| x.child_text(s, "name")).unwrap_or_default();
                        out.push(obj([("instanceId", Value::from(x.child_text(item, "instanceId").unwrap_or_default())), ("state", Value::from(state))]));
                    }
                }
                ex.set_body(Value::Array(out));
            }
            "createAndRunInstances" => {
                let image = header_str(ex, headers::IMAGE_ID).ok_or_else(|| CamelError::illegal_argument("createAndRunInstances requires the CamelAwsEC2ImageId header"))?;
                let itype = header_str(ex, headers::INSTANCE_TYPE).ok_or_else(|| CamelError::illegal_argument("createAndRunInstances requires the CamelAwsEC2InstanceType header"))?;
                let mut p = vec![
                    ("ImageId".to_string(), image),
                    ("InstanceType".to_string(), itype),
                    ("MinCount".to_string(), header_i64(ex, headers::INSTANCE_MIN_COUNT).unwrap_or(1).to_string()),
                    ("MaxCount".to_string(), header_i64(ex, headers::INSTANCE_MAX_COUNT).unwrap_or(1).to_string()),
                ];
                if let Some(k) = header_str(ex, headers::INSTANCES_KEY_PAIR) {
                    p.push(("KeyName".into(), k));
                }
                let x = self.call("RunInstances", p)?;
                ex.set_body(Value::Array(parse_instances(&x)));
            }
            "startInstances" | "stopInstances" | "terminateInstances" | "rebootInstances" => {
                let p = ids(ex);
                if p.is_empty() {
                    return Err(CamelError::illegal_argument(format!("{} requires the CamelAwsEC2InstancesIds header", op)));
                }
                let action = match op.as_str() {
                    "startInstances" => "StartInstances",
                    "stopInstances" => "StopInstances",
                    "terminateInstances" => "TerminateInstances",
                    _ => "RebootInstances",
                };
                let x = self.call(action, p)?;
                if op != "rebootInstances" {
                    ex.set_body(state_changes(&x));
                }
            }
            "" => return Err(CamelError::illegal_argument("aws2-ec2 requires an operation (operation option or CamelAwsEC2Operation header)")),
            other => return Err(unsupported("aws2-ec2", other)),
        }
        Ok(())
    }
}
