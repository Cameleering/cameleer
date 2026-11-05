//! The `aws2-eventbridge` component (Camel `camel-aws2-eventbridge`, producer) over the
//! EventBridge AWS JSON 1.1 protocol.
//!
//! `aws2-eventbridge://eventbusNameOrArn?operation=...` (`default` = the default bus)
//!
//! Operations (`operation` / `CamelAwsEventbridgeOperation`, default `putRule`):
//! * `putRule`: rule `CamelAwsEventbridgeRuleName`, pattern from `CamelAwsEventbridgeEventPattern`
//!   or the `eventPatternFile` option (`file:` / plain path); body = rule ARN.
//! * `putTargets`: targets of `CamelAwsEventbridgeTargets` (list of `{id, arn, input}` maps).
//! * `removeTargets`: ids of `CamelAwsEventbridgeTargetsIds`.
//! * `deleteRule`, `enableRule`, `disableRule`, `describeRule` (body = rule document).
//! * `listRules` (`CamelAwsEventbridgeRuleNamePrefix`; body = list of rules),
//!   `listTargetsByRule` (body = list of targets), `listRuleNamesByTarget`
//!   (`CamelAwsEventbridgeTargetArn`; body = list of names).
//! * `putEvent`: body = event detail (JSON), `CamelAwsEventbridgeEventSource`,
//!   `CamelAwsEventbridgeEventDetailType`, `CamelAwsEventbridgeEventResources` (list or comma
//!   separated); body = list of entry ids, `CamelAwsEventbridgeFailedEntryCount`.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_list, header_str, kinds, obj, Opts};

/// Header names (Camel `EventbridgeConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsEventbridgeOperation";
    pub const RULE_NAME: &str = "CamelAwsEventbridgeRuleName";
    pub const RULE_NAME_PREFIX: &str = "CamelAwsEventbridgeRuleNamePrefix";
    pub const EVENT_PATTERN: &str = "CamelAwsEventbridgeEventPattern";
    pub const TARGETS: &str = "CamelAwsEventbridgeTargets";
    pub const TARGETS_IDS: &str = "CamelAwsEventbridgeTargetsIds";
    pub const TARGET_ARN: &str = "CamelAwsEventbridgeTargetArn";
    pub const EVENT_RESOURCES_ARN: &str = "CamelAwsEventbridgeEventResources";
    pub const EVENT_SOURCE: &str = "CamelAwsEventbridgeEventSource";
    pub const EVENT_DETAIL_TYPE: &str = "CamelAwsEventbridgeEventDetailType";
    /// Cameleer extension.
    pub const FAILED_ENTRY_COUNT: &str = "CamelAwsEventbridgeFailedEntryCount";
}

/// The EventBridge endpoint configuration and client.
pub struct EventbridgeService {
    pub aws: AwsConfig,
    pub event_bus: String,
    pub operation: Option<String>,
    pub event_pattern_file: Option<String>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Event bus name must be specified: aws2-eventbridge://eventbusName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["eventbridgeClient", "pojoRequest"]);
    Ok(Arc::new(EventbridgeService { event_bus: remaining.to_string(), operation: o.get("operation"), event_pattern_file: o.get("eventPatternFile"), aws }))
}

impl EventbridgeService {
    /// Invoke an EventBridge API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("events", "events", "AWSEvents", action, &body, "1.1", kinds::EVENTBRIDGE)
    }

    fn rule(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::RULE_NAME).ok_or_else(|| CamelError::illegal_argument("Rule name must be specified (CamelAwsEventbridgeRuleName)"))
    }

    fn with_bus(&self, mut m: ObjectMap) -> Value {
        m.insert("EventBusName".into(), Value::from(self.event_bus.clone()));
        Value::Object(m)
    }

    fn pattern(&self, ex: &Exchange) -> Result<Option<String>> {
        if let Some(p) = header_str(ex, headers::EVENT_PATTERN) {
            return Ok(Some(p));
        }
        match &self.event_pattern_file {
            Some(f) => {
                let path = f.strip_prefix("file:").or_else(|| f.strip_prefix("classpath:")).unwrap_or(f);
                std::fs::read_to_string(path).map(Some).map_err(|e| CamelError::io(format!("Cannot read eventPatternFile {}: {}", path, e)))
            }
            None => Ok(None),
        }
    }
}

fn target(v: &Value) -> Value {
    let g = |a: &str, b: &str| v.get(a).or_else(|| v.get(b)).cloned();
    let mut m = ObjectMap::new();
    m.insert("Id".to_string(), g("id", "Id").unwrap_or_default());
    m.insert("Arn".to_string(), g("arn", "Arn").unwrap_or_default());
    if let Some(i) = g("input", "Input") {
        m.insert("Input".into(), Value::from(if matches!(i, Value::Object(_) | Value::Array(_)) { i.to_json() } else { i.to_string() }));
    }
    if let Some(r) = g("roleArn", "RoleArn") {
        m.insert("RoleArn".into(), r);
    }
    Value::Object(m)
}

impl CloudService for EventbridgeService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "putRule");
        let one = |k: &str, v: Value| {
            let mut m = ObjectMap::new();
            m.insert(k.to_string(), v);
            m
        };
        match op.as_str() {
            "putRule" => {
                let mut m = one("Name", Value::from(self.rule(ex)?));
                if let Some(p) = self.pattern(ex)? {
                    m.insert("EventPattern".into(), Value::from(p));
                }
                let r = self.call("PutRule", self.with_bus(m))?;
                ex.set_body(r.get("RuleArn").cloned().unwrap_or_default());
            }
            "putTargets" => {
                let targets: Vec<Value> = match ex.header(headers::TARGETS) {
                    Some(Value::Array(a)) => a.iter().map(target).collect(),
                    Some(v @ Value::Object(_)) => vec![target(v)],
                    _ => return Err(CamelError::illegal_argument("putTargets requires the CamelAwsEventbridgeTargets header")),
                };
                let mut m = one("Rule", Value::from(self.rule(ex)?));
                m.insert("Targets".into(), Value::Array(targets));
                let r = self.call("PutTargets", self.with_bus(m))?;
                ex.set_body(r);
            }
            "removeTargets" => {
                let ids = header_list(ex, headers::TARGETS_IDS);
                let mut m = one("Rule", Value::from(self.rule(ex)?));
                m.insert("Ids".into(), Value::from(ids));
                let r = self.call("RemoveTargets", self.with_bus(m))?;
                ex.set_body(r);
            }
            "deleteRule" | "enableRule" | "disableRule" | "describeRule" => {
                let action = match op.as_str() {
                    "deleteRule" => "DeleteRule",
                    "enableRule" => "EnableRule",
                    "disableRule" => "DisableRule",
                    _ => "DescribeRule",
                };
                let r = self.call(action, self.with_bus(one("Name", Value::from(self.rule(ex)?))))?;
                if op == "describeRule" {
                    ex.set_body(r);
                }
            }
            "listRules" => {
                let mut m = ObjectMap::new();
                if let Some(p) = header_str(ex, headers::RULE_NAME_PREFIX) {
                    m.insert("NamePrefix".to_string(), Value::from(p));
                }
                let r = self.call("ListRules", self.with_bus(m))?;
                ex.set_body(r.get("Rules").cloned().unwrap_or(Value::Array(vec![])));
            }
            "listTargetsByRule" => {
                let r = self.call("ListTargetsByRule", self.with_bus(one("Rule", Value::from(self.rule(ex)?))))?;
                ex.set_body(r.get("Targets").cloned().unwrap_or(Value::Array(vec![])));
            }
            "listRuleNamesByTarget" => {
                let arn = header_str(ex, headers::TARGET_ARN).ok_or_else(|| CamelError::illegal_argument("listRuleNamesByTarget requires the CamelAwsEventbridgeTargetArn header"))?;
                let r = self.call("ListRuleNamesByTarget", self.with_bus(one("TargetArn", Value::from(arn))))?;
                ex.set_body(r.get("RuleNames").cloned().unwrap_or(Value::Array(vec![])));
            }
            "putEvent" => {
                let source = header_str(ex, headers::EVENT_SOURCE).ok_or_else(|| CamelError::illegal_argument("putEvent requires the CamelAwsEventbridgeEventSource header"))?;
                let detail_type = header_str(ex, headers::EVENT_DETAIL_TYPE).ok_or_else(|| CamelError::illegal_argument("putEvent requires the CamelAwsEventbridgeEventDetailType header"))?;
                let detail = match ex.body() {
                    v @ (Value::Object(_) | Value::Array(_)) => v.to_json(),
                    v => v.to_string(),
                };
                let mut entry = ObjectMap::new();
                entry.insert("Source".to_string(), Value::from(source));
                entry.insert("DetailType".to_string(), Value::from(detail_type));
                entry.insert("Detail".to_string(), Value::from(detail));
                entry.insert("EventBusName".to_string(), Value::from(self.event_bus.clone()));
                let res = header_list(ex, headers::EVENT_RESOURCES_ARN);
                if !res.is_empty() {
                    entry.insert("Resources".into(), Value::from(res));
                }
                let r = self.call("PutEvents", obj([("Entries", Value::Array(vec![Value::Object(entry)]))]))?;
                ex.set_header(headers::FAILED_ENTRY_COUNT, r.get("FailedEntryCount").and_then(|c| c.as_i64()).unwrap_or(0));
                let ids: Vec<Value> = r.get("Entries").and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().map(|e| e.get("EventId").cloned().unwrap_or_default()).collect();
                ex.set_body(Value::Array(ids));
            }
            other => return Err(unsupported("aws2-eventbridge", other)),
        }
        Ok(())
    }
}
