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
//! * `updateStateMachine`: `CamelAwsStepFunctionsStateMachineArn` plus a new definition
//!   (`CamelAwsStepFunctionsStateMachineDefinition` or a non-empty body) and / or role ARN;
//!   body = the update date.
//! * Activities: `createActivity` (`CamelAwsStepFunctionsActivityName`; body = activity ARN,
//!   also set as `CamelAwsStepFunctionsActivityArn`), `deleteActivity`, `describeActivity`
//!   (`CamelAwsStepFunctionsActivityArn`), `listActivities`, `getActivityTask`
//!   (`CamelAwsStepFunctionsActivityArn`, `CamelAwsStepFunctionsActivityWorkerName`; body = the
//!   task input, `CamelAwsStepFunctionsTaskToken` header, empty body when no task is waiting),
//!   and the worker callbacks (Cameleer operation names) `sendTaskSuccess` (body = output),
//!   `sendTaskFailure` (`CamelAwsStepFunctionsTaskError` / `...TaskCause`), `sendTaskHeartbeat`
//!   (`CamelAwsStepFunctionsTaskToken`).

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

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
    pub const ACTIVITY_NAME: &str = "CamelAwsStepFunctionsActivityName";
    pub const ACTIVITY_ARN: &str = "CamelAwsStepFunctionsActivityArn";
    pub const ACTIVITY_WORKER_NAME: &str = "CamelAwsStepFunctionsActivityWorkerName";
    pub const TASK_TOKEN: &str = "CamelAwsStepFunctionsTaskToken";
    pub const TASK_ERROR: &str = "CamelAwsStepFunctionsTaskError";
    pub const TASK_CAUSE: &str = "CamelAwsStepFunctionsTaskCause";
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
            "updateStateMachine" => {
                let mut m = ObjectMap::new();
                m.insert("stateMachineArn".to_string(), Value::from(need(ex, headers::STATE_MACHINE_ARN)?));
                let def = header_str(ex, headers::STATE_MACHINE_DEFINITION).or_else(|| (!ex.body().is_null() && !ex.body().to_string().is_empty()).then(|| json_text(ex.body())));
                if let Some(d) = def {
                    m.insert("definition".into(), Value::from(d));
                }
                if let Some(r) = header_str(ex, headers::STATE_MACHINE_ROLE_ARN) {
                    m.insert("roleArn".into(), Value::from(r));
                }
                let r = self.call("UpdateStateMachine", Value::Object(m))?;
                ex.set_body(r.get("updateDate").cloned().unwrap_or_default());
            }
            "createActivity" => {
                let r = self.call("CreateActivity", obj([("name", Value::from(need(ex, headers::ACTIVITY_NAME)?))]))?;
                let arn = r.get("activityArn").cloned().unwrap_or_default();
                ex.set_header(headers::ACTIVITY_ARN, arn.clone());
                ex.set_body(arn);
            }
            "deleteActivity" => {
                self.call("DeleteActivity", obj([("activityArn", Value::from(need(ex, headers::ACTIVITY_ARN)?))]))?;
            }
            "describeActivity" => {
                let r = self.call("DescribeActivity", obj([("activityArn", Value::from(need(ex, headers::ACTIVITY_ARN)?))]))?;
                ex.set_body(r);
            }
            "listActivities" => {
                let v = self.paged("ListActivities", ObjectMap::new(), "activities")?;
                ex.set_body(v);
            }
            "getActivityTask" => {
                let mut m = ObjectMap::new();
                m.insert("activityArn".to_string(), Value::from(need(ex, headers::ACTIVITY_ARN)?));
                if let Some(w) = header_str(ex, headers::ACTIVITY_WORKER_NAME) {
                    m.insert("workerName".into(), Value::from(w));
                }
                let r = self.call("GetActivityTask", Value::Object(m))?;
                match r.get("taskToken").map(|t| t.to_string()).filter(|t| !t.is_empty()) {
                    Some(t) => {
                        ex.set_header(headers::TASK_TOKEN, t);
                        let input = r.get("input").map(|i| i.to_string()).unwrap_or_default();
                        ex.set_body(cameleer_core::json::parse(&input).unwrap_or(Value::from(input)));
                    }
                    None => {
                        ex.message.remove_header(headers::TASK_TOKEN);
                        ex.set_body(Value::Null);
                    }
                }
            }
            "sendTaskSuccess" => {
                let output = json_text(ex.body());
                self.call("SendTaskSuccess", obj([("taskToken", Value::from(need(ex, headers::TASK_TOKEN)?)), ("output", Value::from(output))]))?;
            }
            "sendTaskFailure" => {
                let mut m = ObjectMap::new();
                m.insert("taskToken".to_string(), Value::from(need(ex, headers::TASK_TOKEN)?));
                if let Some(e) = header_str(ex, headers::TASK_ERROR) {
                    m.insert("error".into(), Value::from(e));
                }
                if let Some(c) = header_str(ex, headers::TASK_CAUSE) {
                    m.insert("cause".into(), Value::from(c));
                }
                self.call("SendTaskFailure", Value::Object(m))?;
            }
            "sendTaskHeartbeat" => {
                self.call("SendTaskHeartbeat", obj([("taskToken", Value::from(need(ex, headers::TASK_TOKEN)?))]))?;
            }
            "" => return Err(CamelError::illegal_argument("aws2-step-functions requires an operation (operation option or CamelAwsStepFunctionsOperation header)")),
            other => return Err(unsupported("aws2-step-functions", other)),
        }
        Ok(())
    }
}
