//! The `aws2-timestream` component (Camel `camel-aws2-timestream`, producer) over the
//! Timestream Write and Query AWS JSON 1.0 protocols (signing name `timestream`).
//!
//! `aws2-timestream://write:label?operation=...` or `aws2-timestream://query:label?operation=...`
//!
//! Without an endpoint override the client performs endpoint discovery (`DescribeEndpoints`
//! on `ingest.timestream.{region}` / `query.timestream.{region}`) and caches the address.
//!
//! Write operations (`operation` / `CamelAwsTimestreamOperation`): `describeEndpoints`,
//! `createDatabase`, `deleteDatabase`, `describeDatabase`, `listDatabases` (body = list),
//! `createTable` / `deleteTable` / `describeTable` (`CamelAwsTimestreamDatabaseName`,
//! `CamelAwsTimestreamTableName`, optional `CamelAwsTimestreamMemoryStoreRetentionPeriodInHours`
//! / `...MagneticStoreRetentionPeriodInDays`), `listTables` (body = list), `writeRecords`
//! (records from `CamelAwsTimestreamRecordList`, `CamelAwsTimestreamRecord` or the body (map
//! or list of Timestream records), `CamelAwsTimestreamCommonAttributes`; body =
//! `RecordsIngested`).
//!
//! Query operations: `query` (default; `CamelAwsTimestreamQueryString` or the body, all pages
//! fetched; body = list of rows as maps column name → scalar string, `CamelAwsTimestreamQueryId`
//! set), `cancelQuery` (`CamelAwsTimestreamQueryId`), `describeEndpoints`.

use std::sync::{Arc, Mutex};

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{header_json, need, put, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::http::{Request, Url};
use crate::util::{header_i64, header_str, kinds, now_secs, obj, parse_json, Opts};

/// Header names (Camel `Timestream2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsTimestreamOperation";
    pub const DATABASE_NAME: &str = "CamelAwsTimestreamDatabaseName";
    pub const TABLE_NAME: &str = "CamelAwsTimestreamTableName";
    pub const RECORD: &str = "CamelAwsTimestreamRecord";
    pub const RECORD_LIST: &str = "CamelAwsTimestreamRecordList";
    pub const COMMON_ATTRIBUTES: &str = "CamelAwsTimestreamCommonAttributes";
    pub const MEMORY_STORE_RETENTION_HOURS: &str = "CamelAwsTimestreamMemoryStoreRetentionPeriodInHours";
    pub const MAGNETIC_STORE_RETENTION_DAYS: &str = "CamelAwsTimestreamMagneticStoreRetentionPeriodInDays";
    pub const QUERY_STRING: &str = "CamelAwsTimestreamQueryString";
    pub const QUERY_ID: &str = "CamelAwsTimestreamQueryId";
}

const TARGET: &str = "Timestream_20181101";

/// The Timestream endpoint configuration and client.
pub struct TimestreamService {
    pub aws: AwsConfig,
    /// `write` or `query`.
    pub client_type: String,
    pub operation: Option<String>,
    discovered: Mutex<Option<(String, i64)>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let client_type = remaining.split(':').next().unwrap_or("").to_ascii_lowercase();
    if client_type != "write" && client_type != "query" {
        return Err(CamelError::illegal_argument("Expected aws2-timestream://write:label or aws2-timestream://query:label"));
    }
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["awsTimestreamWriteClient", "awsTimestreamQueryClient", "pojoRequest"]);
    Ok(Arc::new(TimestreamService { aws, client_type, operation: o.get("operation"), discovered: Mutex::new(None) }))
}

fn kind(t: &str) -> &'static str {
    if t == "write" {
        kinds::TIMESTREAM_WRITE
    } else {
        kinds::TIMESTREAM_QUERY
    }
}

impl TimestreamService {
    fn host_prefix(&self) -> &'static str {
        if self.client_type == "write" {
            "ingest.timestream"
        } else {
            "query.timestream"
        }
    }

    /// Call `DescribeEndpoints` on the regional discovery endpoint (or the override).
    pub fn describe_endpoints(&self) -> Result<Value> {
        let api = JsonApi { host: self.host_prefix(), service: "timestream", target: TARGET, version: "1.0", kind: kind(&self.client_type) };
        self.aws.api(&api, "DescribeEndpoints", &obj([]))
    }

    fn endpoint(&self) -> Result<Url> {
        if self.aws.is_overridden() {
            return self.aws.endpoint(self.host_prefix());
        }
        let mut d = self.discovered.lock().unwrap();
        if let Some((addr, exp)) = d.as_ref() {
            if *exp > now_secs() {
                return Url::parse(&format!("https://{}", addr));
            }
        }
        let r = self.describe_endpoints()?;
        let ep = r.get("Endpoints").and_then(|e| e.as_array()).and_then(|e| e.first().cloned()).unwrap_or_default();
        let addr = ep.get("Address").map(|a| a.to_string()).filter(|a| !a.is_empty()).ok_or_else(|| CamelError::illegal_state("Timestream endpoint discovery returned no address"))?;
        let ttl = ep.get("CachePeriodInMinutes").and_then(|c| c.as_i64()).unwrap_or(10);
        *d = Some((addr.clone(), now_secs() + ttl * 60));
        Url::parse(&format!("https://{}", addr))
    }

    /// Invoke a Timestream action on the discovered endpoint.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        let req = Request::new("POST", self.endpoint()?)
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("X-Amz-Target", format!("{}.{}", TARGET, action))
            .body(body.to_json().into_bytes());
        let resp = self.aws.send(req, "timestream", kind(&self.client_type))?;
        parse_json(&resp.body)
    }

    fn paged(&self, action: &str, req: ObjectMap, field: &str) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut r = req.clone();
            put(&mut r, "NextToken", token.clone());
            let v = self.call(action, Value::Object(r))?;
            all.extend(v.get(field).and_then(|a| a.as_array()).cloned().unwrap_or_default());
            token = v.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() {
                return Ok(all);
            }
        }
    }

    fn db_table(ex: &Exchange) -> Result<ObjectMap> {
        let mut m = ObjectMap::new();
        m.insert("DatabaseName".into(), Value::from(need(ex, headers::DATABASE_NAME)?));
        m.insert("TableName".into(), Value::from(need(ex, headers::TABLE_NAME)?));
        Ok(m)
    }

    fn write(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let db = || -> Result<ObjectMap> {
            let mut m = ObjectMap::new();
            m.insert("DatabaseName".into(), Value::from(need(ex, headers::DATABASE_NAME)?));
            Ok(m)
        };
        match op {
            "createDatabase" => {
                let r = self.call("CreateDatabase", Value::Object(db()?))?;
                ex.set_body(r.get("Database").cloned().unwrap_or_default());
            }
            "deleteDatabase" => {
                self.call("DeleteDatabase", Value::Object(db()?))?;
            }
            "describeDatabase" => {
                let r = self.call("DescribeDatabase", Value::Object(db()?))?;
                ex.set_body(r.get("Database").cloned().unwrap_or_default());
            }
            "listDatabases" => ex.set_body(Value::Array(self.paged("ListDatabases", ObjectMap::new(), "Databases")?)),
            "createTable" => {
                let mut m = Self::db_table(ex)?;
                let hours = header_i64(ex, headers::MEMORY_STORE_RETENTION_HOURS);
                let days = header_i64(ex, headers::MAGNETIC_STORE_RETENTION_DAYS);
                if hours.is_some() || days.is_some() {
                    let mut rp = ObjectMap::new();
                    put(&mut rp, "MemoryStoreRetentionPeriodInHours", hours);
                    put(&mut rp, "MagneticStoreRetentionPeriodInDays", days);
                    m.insert("RetentionProperties".into(), Value::Object(rp));
                }
                let r = self.call("CreateTable", Value::Object(m))?;
                ex.set_body(r.get("Table").cloned().unwrap_or_default());
            }
            "deleteTable" => {
                self.call("DeleteTable", Value::Object(Self::db_table(ex)?))?;
            }
            "describeTable" => {
                let r = self.call("DescribeTable", Value::Object(Self::db_table(ex)?))?;
                ex.set_body(r.get("Table").cloned().unwrap_or_default());
            }
            "listTables" => {
                let mut m = ObjectMap::new();
                put(&mut m, "DatabaseName", header_str(ex, headers::DATABASE_NAME));
                ex.set_body(Value::Array(self.paged("ListTables", m, "Tables")?));
            }
            "writeRecords" => {
                let records = match (header_json(ex, headers::RECORD_LIST), header_json(ex, headers::RECORD), ex.body()) {
                    (Some(Value::Array(l)), _, _) => l,
                    (_, Some(r), _) => vec![r],
                    (_, _, Value::Array(l)) => l.clone(),
                    (_, _, Value::Object(m)) => vec![Value::Object(m.clone())],
                    _ => return Err(CamelError::illegal_argument("writeRecords needs records: CamelAwsTimestreamRecordList, CamelAwsTimestreamRecord or a map / list body")),
                };
                let mut m = Self::db_table(ex)?;
                m.insert("Records".into(), Value::Array(records));
                put(&mut m, "CommonAttributes", header_json(ex, headers::COMMON_ATTRIBUTES));
                let r = self.call("WriteRecords", Value::Object(m))?;
                ex.set_body(r.get("RecordsIngested").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("Timestream write", other)),
        }
        Ok(())
    }

    /// Rows of a `Query` response as maps (column name → scalar string or nested value).
    pub fn rows(page: &Value) -> Vec<Value> {
        let cols: Vec<String> = page.get("ColumnInfo").and_then(|c| c.as_array()).map(|c| c.iter().map(|ci| ci.get("Name").map(|n| n.to_string()).unwrap_or_default()).collect()).unwrap_or_default();
        page.get("Rows")
            .and_then(|r| r.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        let data = row.get("Data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                        Value::Object(
                            cols.iter()
                                .zip(data.iter())
                                .map(|(c, d)| {
                                    let v = if d.get("NullValue").is_some_and(|n| n.is_truthy()) { Value::Null } else { d.get("ScalarValue").cloned().unwrap_or_else(|| d.clone()) };
                                    (c.clone(), v)
                                })
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        match op {
            "query" => {
                let q = header_str(ex, headers::QUERY_STRING)
                    .or_else(|| Some(ex.body().to_string()).filter(|s| !s.trim().is_empty()))
                    .ok_or_else(|| CamelError::illegal_argument("query needs CamelAwsTimestreamQueryString or the query as body"))?;
                let mut rows = Vec::new();
                let mut token: Option<String> = None;
                let mut query_id = Value::Null;
                loop {
                    let mut m = ObjectMap::new();
                    m.insert("QueryString".into(), Value::from(q.clone()));
                    put(&mut m, "NextToken", token.clone());
                    let page = self.call("Query", Value::Object(m))?;
                    query_id = page.get("QueryId").cloned().unwrap_or(query_id);
                    rows.extend(Self::rows(&page));
                    token = page.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_header(headers::QUERY_ID, query_id);
                ex.set_body(Value::Array(rows));
            }
            "cancelQuery" => {
                let r = self.call("CancelQuery", obj([("QueryId", Value::from(need(ex, headers::QUERY_ID)?))]))?;
                ex.set_body(r.get("CancellationMessage").cloned().unwrap_or_default());
            }
            other => return Err(unsupported("Timestream query", other)),
        }
        Ok(())
    }
}

impl CloudService for TimestreamService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let default = if self.client_type == "write" { "" } else { "query" };
        let op = operation(ex, headers::OPERATION, &self.operation, default);
        if op == "describeEndpoints" {
            let r = self.describe_endpoints()?;
            ex.set_body(r.get("Endpoints").cloned().unwrap_or(Value::Array(vec![])));
            return Ok(());
        }
        if self.client_type == "write" {
            self.write(ex, &op)
        } else {
            self.query(ex, &op)
        }
    }
}
