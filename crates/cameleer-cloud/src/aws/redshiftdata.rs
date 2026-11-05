//! The `aws2-redshift-data` component (Camel `camel-aws2-redshift-data`, producer) over the
//! Redshift Data API (AWS JSON 1.1, signing name `redshift-data`).
//!
//! `aws2-redshift-data://label?operation=...`
//!
//! Connection: `CamelAwsRedshiftDataDatabase` (or `database`), `...ClusterIdentifier` (or
//! `clusterIdentifier`) / `...WorkgroupName` (or `workgroupName`), optional `...SecretArn` /
//! `...DbUser`.
//!
//! Operations (`operation` / `CamelAwsRedshiftDataOperation`):
//! * `executeStatement`: SQL from `CamelAwsRedshiftDataSql` or the body, optional
//!   `...StatementName`, `...Parameters` (list of `{name, value}`); body = statement id, sets
//!   `CamelAwsRedshiftDataStatementId`. Cameleer extension: with `waitTimeout` > 0 (ms) the
//!   statement is polled every `delay` ms (default 200) until it finishes and the final
//!   `CamelAwsRedshiftDataStatus` is set (a failed statement raises `RedshiftDataException`).
//! * `batchExecuteStatement` (`CamelAwsRedshiftDataSqlStatementList` or a list body).
//! * `describeStatement` (`...StatementId`): body = statement map, sets `...Status`.
//! * `getStatementResult` (`...StatementId`, all pages): body = list of rows as maps column →
//!   value, `CamelAwsRedshiftDataTotalNumRows`.
//! * `cancelStatement`, `listStatements` (body = list), `listDatabases`, `listSchemas`
//!   (`...Schema` pattern), `listTables` (`...Schema` / `...Table` patterns),
//!   `describeTable` (`...Schema`, `...Table`; body = column list).

use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{header_json, hdr, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{cloud_error, header_str, kinds, obj, Opts};

/// Header names (Camel `RedshiftData2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsRedshiftDataOperation";
    pub const DATABASE: &str = "CamelAwsRedshiftDataDatabase";
    pub const CLUSTER_IDENTIFIER: &str = "CamelAwsRedshiftDataClusterIdentifier";
    pub const WORKGROUP_NAME: &str = "CamelAwsRedshiftDataWorkgroupName";
    pub const SECRET_ARN: &str = "CamelAwsRedshiftDataSecretArn";
    pub const DB_USER: &str = "CamelAwsRedshiftDataDbUser";
    pub const SQL: &str = "CamelAwsRedshiftDataSql";
    pub const SQL_STATEMENT_LIST: &str = "CamelAwsRedshiftDataSqlStatementList";
    pub const STATEMENT_NAME: &str = "CamelAwsRedshiftDataStatementName";
    pub const STATEMENT_ID: &str = "CamelAwsRedshiftDataStatementId";
    pub const PARAMETERS: &str = "CamelAwsRedshiftDataParameters";
    pub const STATUS: &str = "CamelAwsRedshiftDataStatus";
    pub const SCHEMA: &str = "CamelAwsRedshiftDataSchema";
    pub const TABLE: &str = "CamelAwsRedshiftDataTable";
    pub const TOTAL_NUM_ROWS: &str = "CamelAwsRedshiftDataTotalNumRows";
}

const API: JsonApi = JsonApi { host: "redshift-data", service: "redshift-data", target: "RedshiftData", version: "1.1", kind: kinds::REDSHIFT_DATA };

/// The Redshift Data endpoint configuration and client.
pub struct RedshiftDataService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub database: Option<String>,
    pub cluster_identifier: Option<String>,
    pub workgroup_name: Option<String>,
    pub secret_arn: Option<String>,
    pub db_user: Option<String>,
    pub wait_timeout: i64,
    pub delay: i64,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["awsRedshiftDataClient", "pojoRequest"]);
    Ok(Arc::new(RedshiftDataService {
        operation: o.get("operation"),
        database: o.get("database"),
        cluster_identifier: o.get("clusterIdentifier"),
        workgroup_name: o.get("workgroupName"),
        secret_arn: o.get("secretArn"),
        db_user: o.get("dbUser"),
        wait_timeout: o.i64("waitTimeout", 0)?,
        delay: o.i64("delay", 200)?.max(1),
        aws,
    }))
}

/// A Data API field (`{stringValue}`, `{longValue}`...) as a plain value.
pub fn field_value(f: &Value) -> Value {
    if f.get("isNull").is_some_and(|n| n.is_truthy()) {
        return Value::Null;
    }
    for k in ["stringValue", "longValue", "doubleValue", "booleanValue"] {
        if let Some(v) = f.get(k) {
            return v.clone();
        }
    }
    f.get("blobValue").cloned().unwrap_or(Value::Null)
}

impl RedshiftDataService {
    /// Invoke a Data API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.api(&API, action, &body)
    }

    fn connection(&self, ex: &Exchange, with_db: bool) -> Result<ObjectMap> {
        let mut m = ObjectMap::new();
        if with_db {
            m.insert("Database".into(), Value::from(hdr(ex, headers::DATABASE, &self.database).ok_or_else(|| CamelError::illegal_argument("The database (CamelAwsRedshiftDataDatabase or database option) is required"))?));
        }
        put(&mut m, "ClusterIdentifier", hdr(ex, headers::CLUSTER_IDENTIFIER, &self.cluster_identifier));
        put(&mut m, "WorkgroupName", hdr(ex, headers::WORKGROUP_NAME, &self.workgroup_name));
        put(&mut m, "SecretArn", hdr(ex, headers::SECRET_ARN, &self.secret_arn));
        put(&mut m, "DbUser", hdr(ex, headers::DB_USER, &self.db_user));
        Ok(m)
    }

    fn wait(&self, ex: &mut Exchange, id: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(self.wait_timeout as u64);
        loop {
            let d = self.call("DescribeStatement", obj([("Id", Value::from(id))]))?;
            let status = d.get("Status").map(|s| s.to_string()).unwrap_or_default();
            ex.set_header(headers::STATUS, status.clone());
            match status.as_str() {
                "FINISHED" => return Ok(()),
                "FAILED" | "ABORTED" => {
                    let err = d.get("Error").map(|e| e.to_string()).unwrap_or_default();
                    return Err(cloud_error(kinds::REDSHIFT_DATA, format!("Statement {} {}: {}", id, status, err)));
                }
                _ if Instant::now() >= deadline => return Ok(()),
                _ => std::thread::sleep(Duration::from_millis(self.delay as u64)),
            }
        }
    }
}

impl CloudService for RedshiftDataService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "");
        let id = |ex: &Exchange| -> Result<Value> { Ok(Value::from(need(ex, headers::STATEMENT_ID)?)) };
        match op.as_str() {
            "executeStatement" | "batchExecuteStatement" => {
                let mut m = self.connection(ex, true)?;
                put(&mut m, "StatementName", header_str(ex, headers::STATEMENT_NAME));
                let action = if op == "executeStatement" {
                    let sql = header_str(ex, headers::SQL)
                        .or_else(|| Some(ex.body().to_string()).filter(|s| !s.trim().is_empty()))
                        .ok_or_else(|| CamelError::illegal_argument("executeStatement needs CamelAwsRedshiftDataSql or the SQL as body"))?;
                    m.insert("Sql".into(), Value::from(sql));
                    put(&mut m, "Parameters", header_json(ex, headers::PARAMETERS));
                    "ExecuteStatement"
                } else {
                    let sqls = match header_json(ex, headers::SQL_STATEMENT_LIST) {
                        Some(Value::Array(a)) => a,
                        _ => ex.body().as_array().cloned().ok_or_else(|| CamelError::illegal_argument("batchExecuteStatement needs CamelAwsRedshiftDataSqlStatementList or a list body"))?,
                    };
                    m.insert("Sqls".into(), Value::Array(sqls));
                    "BatchExecuteStatement"
                };
                let r = self.call(action, Value::Object(m))?;
                let sid = r.get("Id").map(|i| i.to_string()).unwrap_or_default();
                ex.set_header(headers::STATEMENT_ID, sid.clone());
                if self.wait_timeout > 0 {
                    self.wait(ex, &sid)?;
                }
                ex.set_body(sid);
            }
            "describeStatement" => {
                let r = self.call("DescribeStatement", obj([("Id", id(ex)?)]))?;
                ex.set_header(headers::STATUS, r.get("Status").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            "getStatementResult" => {
                let sid = id(ex)?;
                let mut rows = Vec::new();
                let mut token: Option<String> = None;
                let mut total = Value::Null;
                loop {
                    let mut m = ObjectMap::new();
                    m.insert("Id".into(), sid.clone());
                    put(&mut m, "NextToken", token.clone());
                    let page = self.call("GetStatementResult", Value::Object(m))?;
                    total = page.get("TotalNumRows").cloned().unwrap_or(total);
                    let cols: Vec<String> = page.get("ColumnMetadata").and_then(|c| c.as_array()).map(|c| c.iter().map(|x| x.get("name").map(|n| n.to_string()).unwrap_or_default()).collect()).unwrap_or_default();
                    for rec in page.get("Records").and_then(|r| r.as_array()).cloned().unwrap_or_default() {
                        let fields = rec.as_array().cloned().unwrap_or_default();
                        rows.push(Value::Object(cols.iter().cloned().zip(fields.iter().map(field_value)).collect()));
                    }
                    token = page.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_header(headers::TOTAL_NUM_ROWS, total);
                ex.set_body(Value::Array(rows));
            }
            "cancelStatement" => {
                let r = self.call("CancelStatement", obj([("Id", id(ex)?)]))?;
                ex.set_body(r.get("Status").cloned().unwrap_or_default());
            }
            "listStatements" => ex.set_body(Value::Array(self.aws.api_paged(&API, "ListStatements", ObjectMap::new(), "Statements", "NextToken")?)),
            "listDatabases" => {
                let m = self.connection(ex, true)?;
                ex.set_body(Value::Array(self.aws.api_paged(&API, "ListDatabases", m, "Databases", "NextToken")?));
            }
            "listSchemas" => {
                let mut m = self.connection(ex, true)?;
                put(&mut m, "SchemaPattern", header_str(ex, headers::SCHEMA));
                ex.set_body(Value::Array(self.aws.api_paged(&API, "ListSchemas", m, "Schemas", "NextToken")?));
            }
            "listTables" => {
                let mut m = self.connection(ex, true)?;
                put(&mut m, "SchemaPattern", header_str(ex, headers::SCHEMA));
                put(&mut m, "TablePattern", header_str(ex, headers::TABLE));
                ex.set_body(Value::Array(self.aws.api_paged(&API, "ListTables", m, "Tables", "NextToken")?));
            }
            "describeTable" => {
                let mut m = self.connection(ex, true)?;
                put(&mut m, "Schema", header_str(ex, headers::SCHEMA));
                m.insert("Table".into(), Value::from(need(ex, headers::TABLE)?));
                ex.set_body(Value::Array(self.aws.api_paged(&API, "DescribeTable", m, "ColumnList", "NextToken")?));
            }
            other => return Err(unsupported("Redshift Data", other)),
        }
        Ok(())
    }
}
