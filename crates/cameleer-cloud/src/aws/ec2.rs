//! The `aws2-ec2` component (Camel `camel-aws2-ec2`, producer subset) over the EC2 Query API
//! (XML responses).
//!
//! `aws2-ec2://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsEC2Operation`, required; Camel's full list):
//! `describeInstances` (`CamelAwsEC2InstancesIds` list, optional), `describeInstancesStatus`,
//! `createAndRunInstances` (`CamelAwsEC2ImageId`, `CamelAwsEC2InstanceType`,
//! `CamelAwsEC2InstanceMinCount` / `MaxCount` (default 1), `CamelAwsEC2InstanceKeyPair`,
//! `CamelAwsEC2InstanceMonitoring`, `CamelAwsEC2InstanceKernelId`,
//! `CamelAwsEC2InstanceEbsOptimized`, `CamelAwsEC2InstanceSecurityGroups` (list of names or
//! `sg-` ids), `CamelAwsEC2InstancesClientToken` (idempotency token),
//! `CamelAwsEC2InstancesPlacement` (availability zone), `CamelAwsEC2SubnetId`,
//! `CamelAwsEC2InstancesTags` (map, tags of the new instances)), `startInstances`,
//! `stopInstances`, `terminateInstances`, `rebootInstances`, `monitorInstances`,
//! `unmonitorInstances` (`CamelAwsEC2InstancesIds`), `createTags` / `deleteTags`
//! (`CamelAwsEC2InstancesIds` + `CamelAwsEC2InstancesTags`; for `deleteTags` a list of keys or
//! a map whose values must match). Bodies: lists of instances `{instanceId, imageId,
//! instanceType, state, stateCode, launchTime, privateIpAddress, keyName, monitoring,
//! securityGroups, tags, subnetId, availabilityZone, ebsOptimized, kernelId}`, of state
//! changes `{instanceId, previousState, currentState}` or of monitoring states
//! `{instanceId, monitoring}`.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

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
    pub const INSTANCE_MONITORING: &str = "CamelAwsEC2InstanceMonitoring";
    pub const INSTANCE_KERNEL_ID: &str = "CamelAwsEC2InstanceKernelId";
    pub const INSTANCE_EBS_OPTIMIZED: &str = "CamelAwsEC2InstanceEbsOptimized";
    pub const INSTANCE_SECURITY_GROUPS: &str = "CamelAwsEC2InstanceSecurityGroups";
    pub const INSTANCES_CLIENT_TOKEN: &str = "CamelAwsEC2InstancesClientToken";
    pub const INSTANCES_PLACEMENT: &str = "CamelAwsEC2InstancesPlacement";
    pub const INSTANCES_TAGS: &str = "CamelAwsEC2InstancesTags";
    pub const SUBNET_ID: &str = "CamelAwsEC2SubnetId";
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
            let mut m = ObjectMap::new();
            m.insert("instanceId".to_string(), t("instanceId"));
            m.insert("imageId".to_string(), t("imageId"));
            m.insert("instanceType".to_string(), t("instanceType"));
            m.insert("launchTime".to_string(), t("launchTime"));
            m.insert("privateIpAddress".to_string(), t("privateIpAddress"));
            m.insert("keyName".to_string(), t("keyName"));
            m.insert("state".to_string(), Value::from(st("name")));
            m.insert("stateCode".to_string(), Value::Int(st("code").parse().unwrap_or(0)));
            m.insert("monitoring".to_string(), Value::from(x.child(item, "monitoring").and_then(|mo| x.child_text(mo, "state")).unwrap_or_default()));
            m.insert("availabilityZone".to_string(), Value::from(x.child(item, "placement").and_then(|pl| x.child_text(pl, "availabilityZone")).unwrap_or_default()));
            m.insert("subnetId".to_string(), t("subnetId"));
            m.insert("ebsOptimized".to_string(), Value::Bool(x.child_text(item, "ebsOptimized").as_deref() == Some("true")));
            m.insert("kernelId".to_string(), t("kernelId"));
            let groups: Vec<Value> = x.child(item, "groupSet").map(|g| x.children(g, "item").into_iter().map(|i| Value::from(x.child_text(i, "groupName").or_else(|| x.child_text(i, "groupId")).unwrap_or_default())).collect()).unwrap_or_default();
            m.insert("securityGroups".to_string(), Value::Array(groups));
            let tags: ObjectMap = x.child(item, "tagSet").map(|g| x.children(g, "item").into_iter().map(|i| (x.child_text(i, "key").unwrap_or_default(), Value::from(x.child_text(i, "value").unwrap_or_default()))).collect()).unwrap_or_default();
            m.insert("tags".to_string(), Value::Object(tags));
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
                if let Some(v) = header_str(ex, headers::INSTANCE_MONITORING) {
                    p.push(("Monitoring.Enabled".into(), v));
                }
                if let Some(v) = header_str(ex, headers::INSTANCE_KERNEL_ID) {
                    p.push(("KernelId".into(), v));
                }
                if let Some(v) = header_str(ex, headers::INSTANCE_EBS_OPTIMIZED) {
                    p.push(("EbsOptimized".into(), v));
                }
                if let Some(v) = header_str(ex, headers::INSTANCES_CLIENT_TOKEN) {
                    p.push(("ClientToken".into(), v));
                }
                if let Some(v) = header_str(ex, headers::INSTANCES_PLACEMENT) {
                    p.push(("Placement.AvailabilityZone".into(), v));
                }
                if let Some(v) = header_str(ex, headers::SUBNET_ID) {
                    p.push(("SubnetId".into(), v));
                }
                let (mut names, mut ids) = (0, 0);
                for g in header_list(ex, headers::INSTANCE_SECURITY_GROUPS) {
                    if g.starts_with("sg-") {
                        ids += 1;
                        p.push((format!("SecurityGroupId.{}", ids), g));
                    } else {
                        names += 1;
                        p.push((format!("SecurityGroup.{}", names), g));
                    }
                }
                if let Some(Value::Object(tags)) = ex.header(headers::INSTANCES_TAGS) {
                    p.push(("TagSpecification.1.ResourceType".into(), "instance".into()));
                    for (i, (k, v)) in tags.iter().enumerate() {
                        p.push((format!("TagSpecification.1.Tag.{}.Key", i + 1), k.clone()));
                        p.push((format!("TagSpecification.1.Tag.{}.Value", i + 1), v.to_string()));
                    }
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
            "monitorInstances" | "unmonitorInstances" => {
                let p = ids(ex);
                if p.is_empty() {
                    return Err(CamelError::illegal_argument(format!("{} requires the CamelAwsEC2InstancesIds header", op)));
                }
                let x = self.call(if op == "monitorInstances" { "MonitorInstances" } else { "UnmonitorInstances" }, p)?;
                let mut out = Vec::new();
                for set in x.descendants("instancesSet") {
                    for item in x.children(set, "item") {
                        let m = x.child(item, "monitoring").and_then(|mo| x.child_text(mo, "state")).unwrap_or_default();
                        out.push(obj([("instanceId", Value::from(x.child_text(item, "instanceId").unwrap_or_default())), ("monitoring", Value::from(m))]));
                    }
                }
                ex.set_body(Value::Array(out));
            }
            "createTags" | "deleteTags" => {
                let resources = header_list(ex, headers::INSTANCES_IDS);
                if resources.is_empty() {
                    return Err(CamelError::illegal_argument(format!("{} requires the CamelAwsEC2InstancesIds header", op)));
                }
                let mut p: Vec<(String, String)> = resources.into_iter().enumerate().map(|(i, id)| (format!("ResourceId.{}", i + 1), id)).collect();
                match ex.header(headers::INSTANCES_TAGS) {
                    Some(Value::Object(tags)) => {
                        for (i, (k, v)) in tags.iter().enumerate() {
                            p.push((format!("Tag.{}.Key", i + 1), k.clone()));
                            p.push((format!("Tag.{}.Value", i + 1), v.to_string()));
                        }
                    }
                    Some(Value::Array(keys)) if op == "deleteTags" => {
                        for (i, k) in keys.iter().enumerate() {
                            p.push((format!("Tag.{}.Key", i + 1), k.to_string()));
                        }
                    }
                    _ => return Err(CamelError::illegal_argument(format!("{} requires the CamelAwsEC2InstancesTags header", op))),
                }
                self.call(if op == "createTags" { "CreateTags" } else { "DeleteTags" }, p)?;
            }
            "" => return Err(CamelError::illegal_argument("aws2-ec2 requires an operation (operation option or CamelAwsEC2Operation header)")),
            other => return Err(unsupported("aws2-ec2", other)),
        }
        Ok(())
    }
}
