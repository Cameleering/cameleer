//! The `aws-config` component (Camel `camel-aws-config`, producer) over the AWS Config AWS
//! JSON 1.1 protocol (signing name `config`).
//!
//! `aws-config://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsConfigOperation`):
//! * `putConfigRule`: `CamelAwsConfigRuleName`, `CamelAwsConfigRuleSourceIdentifier` (managed
//!   rule identifier or Lambda ARN), `CamelAwsConfigRuleSourceOwner` (default `AWS`), optional
//!   `CamelAwsConfigRuleInputParameters` (map or JSON text) and `...RuleScope` (map).
//! * `removeConfigRule` (`CamelAwsConfigRuleName`).
//! * `describeConfigRules` (optional `CamelAwsConfigRuleName`): body = list of rules.
//! * `describeRuleCompliance` (optional `CamelAwsConfigRuleName`): body = list of
//!   `ComplianceByConfigRule`.
//! * `putConformancePack` (`CamelAwsConformancePackName`, template body from
//!   `CamelAwsConformancePackTemplateBody` or the body), `removeConformancePack`,
//!   `describeConformancePackCompliance` (`CamelAwsConformancePackName`; body = rule
//!   compliance list).

use std::sync::Arc;

use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{header_json, need, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_str, kinds, obj, Opts};

/// Header names (Camel `AWSConfigConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsConfigOperation";
    pub const RULE_NAME: &str = "CamelAwsConfigRuleName";
    pub const RULE_SOURCE_IDENTIFIER: &str = "CamelAwsConfigRuleSourceIdentifier";
    pub const RULE_SOURCE_OWNER: &str = "CamelAwsConfigRuleSourceOwner";
    pub const RULE_INPUT_PARAMETERS: &str = "CamelAwsConfigRuleInputParameters";
    pub const RULE_SCOPE: &str = "CamelAwsConfigRuleScope";
    pub const CONFORMANCE_PACK_NAME: &str = "CamelAwsConformancePackName";
    pub const CONFORMANCE_PACK_TEMPLATE_BODY: &str = "CamelAwsConformancePackTemplateBody";
}

const API: JsonApi = JsonApi { host: "config", service: "config", target: "StarlingDoveService", version: "1.1", kind: kinds::CONFIG };

/// The AWS Config endpoint configuration and client.
pub struct ConfigService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["configClient", "pojoRequest"]);
    Ok(Arc::new(ConfigService { operation: o.get("operation"), aws }))
}

impl CloudService for ConfigService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let call = |action: &str, body: Value| self.aws.api(&API, action, &body);
        let names = |ex: &Exchange| -> ObjectMap {
            let mut m = ObjectMap::new();
            if let Some(n) = header_str(ex, headers::RULE_NAME) {
                m.insert("ConfigRuleNames".into(), Value::from(vec![n]));
            }
            m
        };
        match op.as_str() {
            "putConfigRule" => {
                let mut rule = ObjectMap::new();
                rule.insert("ConfigRuleName".into(), Value::from(need(ex, headers::RULE_NAME)?));
                rule.insert(
                    "Source".into(),
                    obj([
                        ("Owner", Value::from(header_str(ex, headers::RULE_SOURCE_OWNER).unwrap_or_else(|| "AWS".into()))),
                        ("SourceIdentifier", Value::from(need(ex, headers::RULE_SOURCE_IDENTIFIER)?)),
                    ]),
                );
                if let Some(p) = header_json(ex, headers::RULE_INPUT_PARAMETERS) {
                    rule.insert("InputParameters".into(), Value::from(if let Value::String(s) = &p { s.clone() } else { p.to_json() }));
                }
                if let Some(s) = header_json(ex, headers::RULE_SCOPE) {
                    rule.insert("Scope".into(), s);
                }
                call("PutConfigRule", obj([("ConfigRule", Value::Object(rule))]))?;
            }
            "removeConfigRule" => {
                call("DeleteConfigRule", obj([("ConfigRuleName", Value::from(need(ex, headers::RULE_NAME)?))]))?;
            }
            "describeConfigRules" => ex.set_body(Value::Array(self.aws.api_paged(&API, "DescribeConfigRules", names(ex), "ConfigRules", "NextToken")?)),
            "describeRuleCompliance" => ex.set_body(Value::Array(self.aws.api_paged(&API, "DescribeComplianceByConfigRule", names(ex), "ComplianceByConfigRules", "NextToken")?)),
            "putConformancePack" => {
                let template = header_str(ex, headers::CONFORMANCE_PACK_TEMPLATE_BODY).unwrap_or_else(|| ex.body().to_string());
                let r = call("PutConformancePack", obj([("ConformancePackName", Value::from(need(ex, headers::CONFORMANCE_PACK_NAME)?)), ("TemplateBody", Value::from(template))]))?;
                ex.set_body(r.get("ConformancePackArn").cloned().unwrap_or_default());
            }
            "removeConformancePack" => {
                call("DeleteConformancePack", obj([("ConformancePackName", Value::from(need(ex, headers::CONFORMANCE_PACK_NAME)?))]))?;
            }
            "describeConformancePackCompliance" => {
                let mut m = ObjectMap::new();
                m.insert("ConformancePackName".into(), Value::from(need(ex, headers::CONFORMANCE_PACK_NAME)?));
                ex.set_body(Value::Array(self.aws.api_paged(&API, "DescribeConformancePackCompliance", m, "ConformancePackRuleComplianceList", "NextToken")?));
            }
            other => return Err(unsupported("Config", other)),
        }
        Ok(())
    }
}
