//! The `aws2-athena` component (Camel `camel-aws2-athena`, producer) over the Athena AWS
//! JSON 1.1 protocol (signing name `athena`).
//!
//! `aws2-athena://label?operation=...`
//!
//! Operations (`operation` / `CamelAwsAthenaOperation`):
//! * `startQueryExecution` (default): query = `queryString` option, `CamelAwsAthenaQueryString`
//!   header or the body; `database`, `outputLocation`, `workGroup`, `encryptionOption`,
//!   `kmsKey`, `clientRequestToken` (options or `CamelAwsAthena*` headers). With `waitTimeout`
//!   > 0 the query is polled (`GetQueryExecution`, first after `initialDelay`, then every
//!   `delay` ms) until it reaches a terminal state or the timeout elapses; a failed attempt is
//!   restarted up to `maxAttempts` times according to `retry` (`never` (default), `always`,
//!   `retryable` (failure reasons mentioning throttling / internal errors / `retryable`),
//!   `exhausted` (resource exhaustion), `generic` (`GENERIC_INTERNAL_ERROR`), comma
//!   separated). Sets `CamelAwsAthenaQueryExecutionId`, `CamelAwsAthenaQueryExecutionState`,
//!   `CamelAwsAthenaOutputLocation`, `CamelAwsAthenaStartQueryExecutionAttempts`,
//!   `CamelAwsAthenaStartQueryExecutionElapsedMillis`; body = query execution id.
//! * `getQueryExecution`: `CamelAwsAthenaQueryExecutionId`; body = the `QueryExecution` map.
//! * `getQueryResults`: `CamelAwsAthenaQueryExecutionId`, `outputType`:
//!   `StreamList` (default: every page fetched, body = list of rows, each a list of cell
//!   strings, the header row included), `SelectList` (one page of `maxResults` /
//!   `CamelAwsAthenaMaxResults` from `CamelAwsAthenaNextToken`; body = the raw
//!   `GetQueryResults` response, `CamelAwsAthenaNextToken` set when more pages exist) or
//!   `S3Pointer` (body = the S3 output location of the query).
//! * `listQueryExecutions`: body = list of ids (`workGroup`, `maxResults`, `nextToken`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{hdr, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{cloud_error, header_i64, header_str, kinds, obj, opt_i64, uuid, Opts};

/// Header names (Camel `Athena2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsAthenaOperation";
    pub const DATABASE: &str = "CamelAwsAthenaDatabase";
    pub const QUERY_EXECUTION_ID: &str = "CamelAwsAthenaQueryExecutionId";
    pub const WORK_GROUP: &str = "CamelAwsAthenaWorkGroup";
    pub const NEXT_TOKEN: &str = "CamelAwsAthenaNextToken";
    pub const MAX_RESULTS: &str = "CamelAwsAthenaMaxResults";
    pub const INCLUDE_TRACE: &str = "CamelAwsAthenaIncludeTrace";
    pub const OUTPUT_LOCATION: &str = "CamelAwsAthenaOutputLocation";
    pub const OUTPUT_TYPE: &str = "CamelAwsAthenaOutputType";
    pub const QUERY_EXECUTION_STATE: &str = "CamelAwsAthenaQueryExecutionState";
    pub const CLIENT_REQUEST_TOKEN: &str = "CamelAwsAthenaClientRequestToken";
    pub const QUERY_STRING: &str = "CamelAwsAthenaQueryString";
    pub const ENCRYPTION_OPTION: &str = "CamelAwsAthenaEncryptionOption";
    pub const KMS_KEY: &str = "CamelAwsAthenaKmsKey";
    pub const WAIT_TIMEOUT: &str = "CamelAwsAthenaWaitTimeout";
    pub const INITIAL_DELAY: &str = "CamelAwsAthenaInitialDelay";
    pub const DELAY: &str = "CamelAwsAthenaDelay";
    pub const MAX_ATTEMPTS: &str = "CamelAwsAthenaMaxAttempts";
    pub const RETRY: &str = "CamelAwsAthenaRetry";
    pub const START_QUERY_EXECUTION_ATTEMPTS: &str = "CamelAwsAthenaStartQueryExecutionAttempts";
    pub const START_QUERY_EXECUTION_ELAPSED_MILLIS: &str = "CamelAwsAthenaStartQueryExecutionElapsedMillis";
}

const API: JsonApi = JsonApi { host: "athena", service: "athena", target: "AmazonAthena", version: "1.1", kind: kinds::ATHENA };

/// The Athena endpoint configuration and client.
pub struct AthenaService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub database: Option<String>,
    pub output_location: Option<String>,
    pub work_group: Option<String>,
    pub query_string: Option<String>,
    pub encryption_option: Option<String>,
    pub kms_key: Option<String>,
    pub client_request_token: Option<String>,
    pub output_type: String,
    pub max_results: Option<i64>,
    pub next_token: Option<String>,
    pub wait_timeout: i64,
    pub initial_delay: i64,
    pub delay: i64,
    pub max_attempts: i64,
    pub retry: String,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["amazonAthenaClient", "resetWaitTimeoutOnRetry", "includeTrace"]);
    let output_type = o.string("outputType", "StreamList");
    if !matches!(output_type.as_str(), "StreamList" | "SelectList" | "S3Pointer") {
        return Err(CamelError::illegal_argument(format!("Invalid outputType: {} (StreamList, SelectList or S3Pointer)", output_type)));
    }
    Ok(Arc::new(AthenaService {
        operation: o.get("operation"),
        database: o.get("database"),
        output_location: o.get("outputLocation"),
        work_group: o.get("workGroup"),
        query_string: o.get("queryString"),
        encryption_option: o.get("encryptionOption"),
        kms_key: o.get("kmsKey"),
        client_request_token: o.get("clientRequestToken"),
        output_type,
        max_results: opt_i64(o, "maxResults")?,
        next_token: o.get("nextToken"),
        wait_timeout: o.i64("waitTimeout", 0)?,
        initial_delay: o.i64("initialDelay", 1000)?,
        delay: o.i64("delay", 2000)?,
        max_attempts: o.i64("maxAttempts", 1)?.max(1),
        retry: o.string("retry", "never"),
        aws,
    }))
}

fn terminal(state: &str) -> bool {
    matches!(state, "SUCCEEDED" | "FAILED" | "CANCELLED")
}

fn should_retry(retry: &str, reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    retry.split(',').map(|x| x.trim()).any(|p| match p {
        "always" => true,
        "retryable" => r.contains("retryable") || r.contains("throttl") || r.contains("internal") || r.contains("slow down"),
        "exhausted" => r.contains("exhausted"),
        "generic" => r.contains("generic_internal_error"),
        _ => false,
    })
}

impl AthenaService {
    /// Invoke an Athena API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.api(&API, action, &body)
    }

    fn start(&self, ex: &Exchange, query: &str, token: Option<String>) -> Result<String> {
        let mut m = ObjectMap::new();
        m.insert("QueryString".into(), Value::from(query));
        put(&mut m, "ClientRequestToken", token);
        put(&mut m, "WorkGroup", hdr(ex, headers::WORK_GROUP, &self.work_group));
        if let Some(db) = hdr(ex, headers::DATABASE, &self.database) {
            m.insert("QueryExecutionContext".into(), obj([("Database", Value::from(db))]));
        }
        let mut rc = ObjectMap::new();
        put(&mut rc, "OutputLocation", hdr(ex, headers::OUTPUT_LOCATION, &self.output_location));
        if let Some(e) = hdr(ex, headers::ENCRYPTION_OPTION, &self.encryption_option) {
            let mut enc = ObjectMap::new();
            enc.insert("EncryptionOption".into(), Value::from(e));
            put(&mut enc, "KmsKey", hdr(ex, headers::KMS_KEY, &self.kms_key));
            rc.insert("EncryptionConfiguration".into(), Value::Object(enc));
        }
        if !rc.is_empty() {
            m.insert("ResultConfiguration".into(), Value::Object(rc));
        }
        let r = self.call("StartQueryExecution", Value::Object(m))?;
        r.get("QueryExecutionId").map(|v| v.to_string()).ok_or_else(|| cloud_error(kinds::ATHENA, "StartQueryExecution returned no QueryExecutionId"))
    }

    /// `GetQueryExecution`: the `QueryExecution` map.
    pub fn get_execution(&self, id: &str) -> Result<Value> {
        let r = self.call("GetQueryExecution", obj([("QueryExecutionId", Value::from(id))]))?;
        Ok(r.get("QueryExecution").cloned().unwrap_or_default())
    }

    fn state_of(exec: &Value) -> (String, String) {
        let st = exec.get("Status").cloned().unwrap_or_default();
        (st.get("State").map(|s| s.to_string()).unwrap_or_default(), st.get("StateChangeReason").map(|s| s.to_string()).unwrap_or_default())
    }

    fn start_query(&self, ex: &mut Exchange) -> Result<()> {
        let query = header_str(ex, headers::QUERY_STRING)
            .or_else(|| self.query_string.clone())
            .or_else(|| Some(ex.body().to_string()).filter(|s| !s.trim().is_empty()))
            .ok_or_else(|| CamelError::illegal_argument("A query is required (queryString option, CamelAwsAthenaQueryString header or the body)"))?;
        let wait = header_i64(ex, headers::WAIT_TIMEOUT).unwrap_or(self.wait_timeout);
        let initial = header_i64(ex, headers::INITIAL_DELAY).unwrap_or(self.initial_delay);
        let delay = header_i64(ex, headers::DELAY).unwrap_or(self.delay).max(1);
        let attempts_max = header_i64(ex, headers::MAX_ATTEMPTS).unwrap_or(self.max_attempts).max(1);
        let retry = header_str(ex, headers::RETRY).unwrap_or_else(|| self.retry.clone());
        let token = hdr(ex, headers::CLIENT_REQUEST_TOKEN, &self.client_request_token);
        let started = Instant::now();
        let mut attempts = 0;
        let (id, state, exec) = loop {
            attempts += 1;
            // a retried query needs a fresh idempotency token
            let tok = if attempts == 1 { token.clone() } else { token.as_ref().map(|_| uuid()) };
            let id = self.start(ex, &query, tok)?;
            if wait <= 0 {
                break (id, "QUEUED".to_string(), Value::Null);
            }
            let deadline = Instant::now() + Duration::from_millis(wait as u64);
            std::thread::sleep(Duration::from_millis(initial.max(0) as u64).min(Duration::from_millis(wait as u64)));
            let (state, reason, exec) = loop {
                let exec = self.get_execution(&id)?;
                let (state, reason) = Self::state_of(&exec);
                if terminal(&state) || Instant::now() >= deadline {
                    break (state, reason, exec);
                }
                std::thread::sleep(Duration::from_millis(delay as u64).min(deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1)));
            };
            if state == "FAILED" && attempts < attempts_max && should_retry(&retry, &reason) {
                continue;
            }
            break (id, state, exec);
        };
        ex.set_header(headers::QUERY_EXECUTION_ID, id.clone());
        ex.set_header(headers::QUERY_EXECUTION_STATE, state);
        ex.set_header(headers::START_QUERY_EXECUTION_ATTEMPTS, attempts as i64);
        ex.set_header(headers::START_QUERY_EXECUTION_ELAPSED_MILLIS, started.elapsed().as_millis() as i64);
        if let Some(loc) = exec.get("ResultConfiguration").and_then(|r| r.get("OutputLocation")) {
            ex.set_header(headers::OUTPUT_LOCATION, loc.clone());
        }
        ex.set_body(id);
        Ok(())
    }

    fn results_page(&self, id: &str, max: Option<i64>, token: Option<String>) -> Result<Value> {
        let mut m = ObjectMap::new();
        m.insert("QueryExecutionId".into(), Value::from(id));
        put(&mut m, "MaxResults", max);
        put(&mut m, "NextToken", token);
        self.call("GetQueryResults", Value::Object(m))
    }

    /// Rows of a `GetQueryResults` page as lists of cell strings.
    pub fn rows(page: &Value) -> Vec<Value> {
        page.get("ResultSet")
            .and_then(|r| r.get("Rows"))
            .and_then(|r| r.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|row| Value::Array(row.get("Data").and_then(|d| d.as_array()).map(|cells| cells.iter().map(|c| c.get("VarCharValue").cloned().unwrap_or(Value::Null)).collect()).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_results(&self, ex: &mut Exchange) -> Result<()> {
        let id = need(ex, headers::QUERY_EXECUTION_ID)?;
        let output = header_str(ex, headers::OUTPUT_TYPE).unwrap_or_else(|| self.output_type.clone());
        let max = header_i64(ex, headers::MAX_RESULTS).or(self.max_results);
        match output.as_str() {
            "S3Pointer" => {
                let exec = self.get_execution(&id)?;
                let loc = exec.get("ResultConfiguration").and_then(|r| r.get("OutputLocation")).cloned().unwrap_or_default();
                ex.set_header(headers::OUTPUT_LOCATION, loc.clone());
                ex.set_body(loc);
            }
            "SelectList" => {
                let page = self.results_page(&id, max, hdr(ex, headers::NEXT_TOKEN, &self.next_token))?;
                match page.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty()) {
                    Some(t) => ex.set_header(headers::NEXT_TOKEN, t),
                    None => {
                        ex.message.remove_header(headers::NEXT_TOKEN);
                    }
                }
                ex.set_body(page);
            }
            _ => {
                let mut rows = Vec::new();
                let mut token = hdr(ex, headers::NEXT_TOKEN, &self.next_token);
                loop {
                    let page = self.results_page(&id, max, token)?;
                    rows.extend(Self::rows(&page));
                    token = page.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.message.remove_header(headers::NEXT_TOKEN);
                ex.set_body(Value::Array(rows));
            }
        }
        Ok(())
    }
}

impl CloudService for AthenaService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "startQueryExecution");
        match op.as_str() {
            "startQueryExecution" => self.start_query(ex)?,
            "getQueryExecution" => {
                let exec = self.get_execution(&need(ex, headers::QUERY_EXECUTION_ID)?)?;
                let (state, _) = Self::state_of(&exec);
                ex.set_header(headers::QUERY_EXECUTION_STATE, state);
                ex.set_body(exec);
            }
            "getQueryResults" => self.get_results(ex)?,
            "listQueryExecutions" => {
                let mut m = ObjectMap::new();
                put(&mut m, "WorkGroup", hdr(ex, headers::WORK_GROUP, &self.work_group));
                put(&mut m, "MaxResults", header_i64(ex, headers::MAX_RESULTS).or(self.max_results));
                put(&mut m, "NextToken", hdr(ex, headers::NEXT_TOKEN, &self.next_token));
                let r = self.call("ListQueryExecutions", Value::Object(m))?;
                if let Some(t) = r.get("NextToken").filter(|t| !t.to_string().is_empty()) {
                    ex.set_header(headers::NEXT_TOKEN, t.clone());
                }
                ex.set_body(r.get("QueryExecutionIds").cloned().unwrap_or(Value::Array(vec![])));
            }
            other => return Err(unsupported("Athena", other)),
        }
        Ok(())
    }
}
