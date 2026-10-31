//! The `aws2-step-functions` component (Camel `camel-aws2-step-functions`, producer) over the
//! Step Functions AWS JSON 1.0 protocol (signing name `states`).
//!
//! `aws2-step-functions://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsStepFunctionsOperation`, required):
//! * `createStateMachine`: `CamelAwsStepFunctionsStateMachineName`,
//!   `CamelAwsStepFunctionsStateMachineDefinition` (or the body), `CamelAwsStepFunctionsStateMachineRoleArn`;
//!   body = state machine ARN.
//! * `deleteStateMachine`, `describeStateMachine` (`CamelAwsStepFunctionsStateMachineArn`),
//!   `listStateMachines` (body = list).
//! * `startExecution` / `startSyncExecution`: `CamelAwsStepFunctionsStateMachineArn`, input =
//!   `CamelAwsStepFunctionsExecutionInput` or the body, name `CamelAwsStepFunctionsExecutionName`;
//!   sets `CamelAwsStepFunctionsExecutionArn` (body = execution ARN, or the sync result).
//! * `describeExecution`, `stopExecution`, `getExecutionHistory` (`CamelAwsStepFunctionsExecutionArn`);
//!   `listExecutions` (`CamelAwsStepFunctionsStateMachineArn`, optional
//!   `CamelAwsStepFunctionsExecutionStatusFilter`).

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::AwsConfig;
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_str, kinds, obj, Opts};

/// Header names (Camel `StepFunctions2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsStepFunctionsOperation";
    pub const STATE_MACHINE_NAME: &str = "CamelAwsStepFunctionsStateMachineName";
    pub const STATE_MACHINE_DEFINITION: &str = "CamelAwsStepFunctionsStateMachineDefinition";
    pub const STATE_MACHINE_ROLE_ARN: &str = "CamelAwsStepFunctionsStateMachineRoleArn";
    pub const STATE_MACHINE_ARN: &str = "CamelAwsStepFunctionsStateMachineArn";
    pub const EXECUTION_ARN: &str = "CamelAwsStepFunctionsExecutionArn";
    pub const EXECUTION_NAME: &str = "CamelAwsStepFunctionsExecutionName";
    pub const EXECUTION_INPUT: &str = "CamelAwsStepFunctionsExecutionInput";
    pub const EXECUTION_STATUS_FILTER: &str = "CamelAwsStepFunctionsExecutionStatusFilter";
}

/// The Step Functions endpoint configuration and client.
pub struct StepFunctionsService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["awsSfnClient", "pojoRequest"]);
    Ok(Arc::new(StepFunctionsService { operation: o.get("operation"), aws }))
}

fn need(ex: &Exchange, h: &str) -> Result<String> {
    header_str(ex, h).ok_or_else(|| CamelError::illegal_argument(format!("The {} header is required", h)))
}

fn json_text(v: &Value) -> String {
    match v {
        Value::Object(_) | Value::Array(_) => v.to_json(),
        Value::Null => "{}".to_string(),
        other => other.to_string(),
    }
}

impl StepFunctionsService {
    /// Invoke a Step Functions API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("states", "states", "AWSStepFunctions", action, &body, "1.0", kinds::SFN)
    }

    fn paged(&self, action: &str, req: ObjectMap, field: &str) -> Result<Value> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut r = req.clone();
            if let Some(t) = &token {
                r.insert("nextToken".into(), Value::from(t.clone()));
            }
            let v = self.call(action, Value::Object(r))?;
            all.extend(v.get(field).and_then(|a| a.as_array()).cloned().unwrap_or_default());
            token = v.get("nextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() {
                return Ok(Value::Array(all));
            }
        }
    }
}

impl CloudService for StepFunctionsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        match op.as_str() {
            "createStateMachine" => {
                let def = header_str(ex, headers::STATE_MACHINE_DEFINITION).unwrap_or_else(|| json_text(ex.body()));
                let r = self.call(
                    "CreateStateMachine",
                    obj([
                        ("name", Value::from(need(ex, headers::STATE_MACHINE_NAME)?)),
                        ("definition", Value::from(def)),
                        ("roleArn", Value::from(need(ex, headers::STATE_MACHINE_ROLE_ARN)?)),
                    ]),
                )?;
                let arn = r.get("stateMachineArn").cloned().unwrap_or_default();
                ex.set_header(headers::STATE_MACHINE_ARN, arn.clone());
                ex.set_body(arn);
            }
            "deleteStateMachine" => {
                self.call("DeleteStateMachine", obj([("stateMachineArn", Value::from(need(ex, headers::STATE_MACHINE_ARN)?))]))?;
            }
            "describeStateMachine" => {
                let r = self.call("DescribeStateMachine", obj([("stateMachineArn", Value::from(need(ex, headers::STATE_MACHINE_ARN)?))]))?;
                ex.set_body(r);
            }
            "listStateMachines" => {
                let v = self.paged("ListStateMachines", ObjectMap::new(), "stateMachines")?;
                ex.set_body(v);
            }
            "startExecution" | "startSyncExecution" => {
                let input = header_str(ex, headers::EXECUTION_INPUT).unwrap_or_else(|| json_text(ex.body()));
                let mut m = ObjectMap::new();
                m.insert("stateMachineArn".to_string(), Value::from(need(ex, headers::STATE_MACHINE_ARN)?));
                m.insert("input".to_string(), Value::from(input));
                if let Some(n) = header_str(ex, headers::EXECUTION_NAME) {
                    m.insert("name".into(), Value::from(n));
                }
                let sync = op == "startSyncExecution";
                let r = self.call(if sync { "StartSyncExecution" } else { "StartExecution" }, Value::Object(m))?;
                let arn = r.get("executionArn").cloned().unwrap_or_default();
                ex.set_header(headers::EXECUTION_ARN, arn.clone());
                ex.set_body(if sync { r } else { arn });
            }
            "describeExecution" => {
                let r = self.call("DescribeExecution", obj([("executionArn", Value::from(need(ex, headers::EXECUTION_ARN)?))]))?;
                ex.set_body(r);
            }
            "stopExecution" => {
                let r = self.call("StopExecution", obj([("executionArn", Value::from(need(ex, headers::EXECUTION_ARN)?))]))?;
                ex.set_body(r);
            }
            "getExecutionHistory" => {
                let mut m = ObjectMap::new();
                m.insert("executionArn".to_string(), Value::from(need(ex, headers::EXECUTION_ARN)?));
                let v = self.paged("GetExecutionHistory", m, "events")?;
                ex.set_body(v);
            }
            "listExecutions" => {
                let mut m = ObjectMap::new();
                m.insert("stateMachineArn".to_string(), Value::from(need(ex, headers::STATE_MACHINE_ARN)?));
                if let Some(f) = header_str(ex, headers::EXECUTION_STATUS_FILTER) {
                    m.insert("statusFilter".into(), Value::from(f));
                }
                let v = self.paged("ListExecutions", m, "executions")?;
                ex.set_body(v);
            }
            "" => return Err(CamelError::illegal_argument("aws2-step-functions requires an operation (operation option or CamelAwsStepFunctionsOperation header)")),
            other => return Err(unsupported("aws2-step-functions", other)),
        }
        Ok(())
    }
}
