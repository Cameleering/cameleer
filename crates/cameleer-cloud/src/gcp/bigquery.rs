//! The `google-bigquery` (streaming insert) and `google-bigquery-sql` (standard SQL queries)
//! components (Camel `camel-google-bigquery`) over the BigQuery v2 REST API.
//!
//! `google-bigquery://projectId:datasetId[:tableId]?serviceAccountKey=...&useAsInsertId=id`
//!
//! Each exchange is a `tabledata.insertAll`: a map body is one row, an array body several
//! rows. Table from the uri or `CamelGoogleBigQueryTableId`; `CamelGoogleBigQueryTableSuffix`
//! sets the template suffix, `CamelGoogleBigQueryPartitionDecorator` the partition
//! (`table$20260101`), `CamelGoogleBigQueryInsertId` (or the `useAsInsertId` field) the insert
//! id for de-duplication. Rejected rows raise `BigQueryException` listing the insert errors.
//!
//! Load and extract jobs (Cameleer `operation` option / `CamelGoogleBigQueryOperation` header;
//! default `insert` = the streaming insert above), run with `jobs.insert` and polled
//! (`jobs.get`, every `jobPollInterval` ms, default 200, until `jobTimeout` ms, default 60000):
//! * `load`: loads Cloud Storage files into the table: `sourceUris` / `CamelGoogleBigQuerySourceUris`
//!   (or a list / text body of `gs://` uris), `sourceFormat` (`CSV` default,
//!   `NEWLINE_DELIMITED_JSON`, `AVRO`, `PARQUET`...), `skipLeadingRows`, `fieldDelimiter`,
//!   `writeDisposition` (`WRITE_APPEND` default, `WRITE_TRUNCATE`, `WRITE_EMPTY`),
//!   `createDisposition`, `autodetect`.
//! * `extract`: exports the table to `destinationUris` / `CamelGoogleBigQueryDestinationUris`
//!   (`destinationFormat` `NEWLINE_DELIMITED_JSON` default or `CSV`, `printHeader`, `compression`).
//!
//! Body = the finished job resource; `CamelGoogleBigQueryJobId` is set; a job finishing with an
//! `errorResult` raises `BigQueryException`.
//!
//! Tables and datasets (Cameleer operations): `createTable` (schema from `tableSchema` /
//! `CamelGoogleBigQueryTableSchema`: a list of `{name, type, mode, description}` maps, a JSON
//! text of it, or `name:TYPE,name:TYPE`; `timePartitioningField` + `timePartitioningType`
//! (`DAY` default), `clusteringFields`, `tableDescription`, `expirationMs`; body = the table
//! resource), `getTable`, `listTables` (body = table ids), `deleteTable`; `createDataset`
//! (`datasetLocation`, default `US`), `listDatasets` (body = dataset ids), `deleteDataset`
//! (`deleteContents`). `createTableIfNotExists=true` makes the streaming insert create a missing
//! table (with the configured schema) first.
//!
//! `google-bigquery-sql://projectId:query?...` runs a standard SQL query with `jobs.query`
//! (polling `getQueryResults` until complete, reading all pages). The query may be
//! `classpath:`/`file:` resource. `@name` parameters are bound from the header `name`, else
//! the body map entry `name`; `${header.x}` / `${body}`-style placeholders are substituted first
//! (`CamelGoogleBigQueryTranslatedQuery`). Body = number of affected rows for DML, otherwise
//! the list of rows (maps typed after the result schema). `CamelGoogleBigQueryJobId` is set.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::GcpConfig;
use crate::endpoint::CloudService;
use crate::util::{cloud_error, header_str, kinds, obj, uri_encode, Opts};

/// Header names (Camel `GoogleBigQueryConstants`).
pub mod headers {
    pub const TABLE_SUFFIX: &str = "CamelGoogleBigQueryTableSuffix";
    pub const TABLE_ID: &str = "CamelGoogleBigQueryTableId";
    pub const INSERT_ID: &str = "CamelGoogleBigQueryInsertId";
    pub const PARTITION_DECORATOR: &str = "CamelGoogleBigQueryPartitionDecorator";
    pub const TRANSLATED_QUERY: &str = "CamelGoogleBigQueryTranslatedQuery";
    pub const JOB_ID: &str = "CamelGoogleBigQueryJobId";
    /// Cameleer extensions.
    pub const OPERATION: &str = "CamelGoogleBigQueryOperation";
    pub const SOURCE_URIS: &str = "CamelGoogleBigQuerySourceUris";
    pub const DESTINATION_URIS: &str = "CamelGoogleBigQueryDestinationUris";
}

fn base(gcp: &GcpConfig, project: &str) -> String {
    format!("{}/bigquery/v2/projects/{}", gcp.base("https://bigquery.googleapis.com"), project)
}

/// The streaming insert endpoint.
pub struct BigQueryService {
    pub gcp: GcpConfig,
    pub project: String,
    pub dataset: String,
    pub table: Option<String>,
    pub use_as_insert_id: Option<String>,
    pub operation: Option<String>,
    /// Job options (`sourceFormat`, `writeDisposition`, ...).
    pub job_options: std::collections::BTreeMap<String, String>,
    pub job_poll_interval: std::time::Duration,
    pub job_timeout: std::time::Duration,
    /// Table / dataset options (`tableSchema`, `timePartitioningField`, ...).
    pub table_options: std::collections::BTreeMap<String, String>,
    pub create_table_if_not_exists: bool,
}

const TABLE_OPTIONS: &[&str] = &["tableSchema", "timePartitioningField", "timePartitioningType", "clusteringFields", "tableDescription", "expirationMs", "datasetLocation", "deleteContents"];

const JOB_OPTIONS: &[&str] = &[
    "sourceUris",
    "sourceFormat",
    "skipLeadingRows",
    "fieldDelimiter",
    "writeDisposition",
    "createDisposition",
    "autodetect",
    "destinationUris",
    "destinationFormat",
    "printHeader",
    "compression",
];

/// Factory for `google-bigquery`.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let parts: Vec<&str> = remaining.split(':').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(CamelError::illegal_argument("Expected google-bigquery://projectId:datasetId[:tableId]"));
    }
    let gcp = GcpConfig::from_opts(o)?;
    o.ignore(&["connectionFactory"]);
    let mut job_options = std::collections::BTreeMap::new();
    for k in JOB_OPTIONS {
        if let Some(v) = o.get(k) {
            job_options.insert(k.to_string(), v);
        }
    }
    Ok(Arc::new(BigQueryService {
        project: parts[0].to_string(),
        dataset: parts[1].to_string(),
        table: parts.get(2).map(|t| t.to_string()).filter(|t| !t.is_empty()),
        use_as_insert_id: o.get("useAsInsertId"),
        operation: o.get("operation"),
        job_options,
        job_poll_interval: std::time::Duration::from_millis(o.i64("jobPollInterval", 200)?.max(1) as u64),
        job_timeout: std::time::Duration::from_millis(o.i64("jobTimeout", 60_000)?.max(1) as u64),
        table_options: TABLE_OPTIONS.iter().filter_map(|k| o.get(k).map(|v| (k.to_string(), v))).collect(),
        create_table_if_not_exists: o.bool("createTableIfNotExists", false)?,
        gcp,
    }))
}

/// A table schema (`fields`) from a list of maps, JSON text or `name:TYPE,...`.
pub fn schema_fields(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a
            .iter()
            .map(|f| match f {
                Value::Object(_) => f.clone(),
                other => {
                    let t = other.to_string();
                    let (n, ty) = t.split_once(':').unwrap_or((&t, "STRING"));
                    obj([("name", Value::from(n.trim())), ("type", Value::from(ty.trim().to_ascii_uppercase()))])
                }
            })
            .collect()),
        Value::String(s) if s.trim_start().starts_with('[') || s.trim_start().starts_with('{') => {
            let v = crate::util::parse_json(s.as_bytes())?;
            match v.get("fields") {
                Some(f) => schema_fields(f),
                None => schema_fields(&v),
            }
        }
        Value::Object(m) if m.contains_key("fields") => schema_fields(&m["fields"]),
        other => schema_fields(&Value::Array(other.to_string().split(',').filter(|x| !x.trim().is_empty()).map(|x| Value::from(x.trim())).collect())),
    }
}

impl BigQueryService {
    fn table_option(&self, ex: &Exchange, header: &str, option: &str) -> Option<Value> {
        ex.header(header).cloned().filter(|v| !v.is_null()).or_else(|| self.table_options.get(option).map(|v| Value::from(v.clone())))
    }

    fn create_table(&self, ex: &Exchange, table: &str) -> Result<Value> {
        let schema = self.table_option(ex, "CamelGoogleBigQueryTableSchema", "tableSchema").ok_or_else(|| CamelError::illegal_argument("createTable requires a schema (tableSchema option or CamelGoogleBigQueryTableSchema header)"))?;
        let mut m = ObjectMap::new();
        m.insert("tableReference".into(), obj([("projectId", Value::from(self.project.clone())), ("datasetId", Value::from(self.dataset.clone())), ("tableId", Value::from(table))]));
        m.insert("schema".into(), obj([("fields", Value::Array(schema_fields(&schema)?))]));
        if let Some(f) = self.table_option(ex, "CamelGoogleBigQueryTimePartitioningField", "timePartitioningField") {
            let ty = self.table_options.get("timePartitioningType").cloned().unwrap_or_else(|| "DAY".into());
            m.insert("timePartitioning".into(), obj([("type", Value::from(ty)), ("field", f)]));
        }
        if let Some(c) = self.table_options.get("clusteringFields") {
            m.insert("clustering".into(), obj([("fields", Value::from(c.split(',').map(|x| x.trim().to_string()).collect::<Vec<_>>()))]));
        }
        if let Some(d) = self.table_option(ex, "CamelGoogleBigQueryTableDescription", "tableDescription") {
            m.insert("description".into(), d);
        }
        if let Some(e) = self.table_options.get("expirationMs") {
            let ms: i64 = e.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid expirationMs: {}", e)))?;
            m.insert("expirationTime".into(), Value::from((crate::util::now_millis() + ms).to_string()));
        }
        let url = format!("{}/datasets/{}/tables", base(&self.gcp, &self.project), uri_encode(&self.dataset, false));
        self.gcp.json("POST", &url, Some(&Value::Object(m)), kinds::BIGQUERY)
    }

    fn table_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let b = base(&self.gcp, &self.project);
        let table = || header_str(ex, headers::TABLE_ID).or_else(|| self.table.clone()).ok_or_else(|| CamelError::illegal_argument("Table id must be specified (uri or CamelGoogleBigQueryTableId)"));
        let ds = uri_encode(&self.dataset, false);
        match op {
            "createTable" => {
                let t = table()?;
                let r = self.create_table(ex, &t)?;
                ex.set_body(r);
            }
            "getTable" => {
                let r = self.gcp.json("GET", &format!("{}/datasets/{}/tables/{}", b, ds, uri_encode(&table()?, false)), None, kinds::BIGQUERY)?;
                ex.set_body(r);
            }
            "deleteTable" => {
                self.gcp.send(crate::http::Request::new("DELETE", crate::http::Url::parse(&format!("{}/datasets/{}/tables/{}", b, ds, uri_encode(&table()?, false)))?), kinds::BIGQUERY)?;
            }
            "listTables" | "listDatasets" => {
                let (url, field, id) = if op == "listTables" { (format!("{}/datasets/{}/tables", b, ds), "tables", ("tableReference", "tableId")) } else { (format!("{}/datasets", b), "datasets", ("datasetReference", "datasetId")) };
                let mut out = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let u = match &token {
                        Some(t) => format!("{}?pageToken={}", url, uri_encode(t, false)),
                        None => url.clone(),
                    };
                    let r = self.gcp.json("GET", &u, None, kinds::BIGQUERY)?;
                    out.extend(r.get(field).and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().map(|x| x.get(id.0).and_then(|r| r.get(id.1)).cloned().unwrap_or_default()));
                    token = r.get("nextPageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(out));
            }
            "createDataset" => {
                let mut m = ObjectMap::new();
                m.insert("datasetReference".into(), obj([("projectId", Value::from(self.project.clone())), ("datasetId", Value::from(self.dataset.clone()))]));
                m.insert("location".into(), Value::from(self.table_options.get("datasetLocation").cloned().unwrap_or_else(|| "US".into())));
                let r = self.gcp.json("POST", &format!("{}/datasets", b), Some(&Value::Object(m)), kinds::BIGQUERY)?;
                ex.set_body(r);
            }
            _ => {
                let contents = self.table_options.get("deleteContents").is_some_and(|v| v == "true");
                self.gcp.send(crate::http::Request::new("DELETE", crate::http::Url::parse(&format!("{}/datasets/{}?deleteContents={}", b, ds, contents))?), kinds::BIGQUERY)?;
            }
        }
        Ok(())
    }

    fn uris(&self, ex: &Exchange, header: &str, option: &str) -> Vec<String> {
        let from_header = crate::util::header_list(ex, header);
        if !from_header.is_empty() {
            return from_header;
        }
        if let Some(o) = self.job_options.get(option) {
            return crate::util::str_list(&Value::from(o.clone()));
        }
        match ex.body() {
            Value::Array(_) | Value::String(_) => crate::util::str_list(ex.body()).into_iter().filter(|u| u.starts_with("gs://")).collect(),
            _ => Vec::new(),
        }
    }

    fn job(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let table = header_str(ex, headers::TABLE_ID).or_else(|| self.table.clone()).ok_or_else(|| CamelError::illegal_argument("Table id must be specified (uri or CamelGoogleBigQueryTableId)"))?;
        let table_ref = obj([("projectId", Value::from(self.project.clone())), ("datasetId", Value::from(self.dataset.clone())), ("tableId", Value::from(table))]);
        let opt = |k: &str| self.job_options.get(k).cloned();
        let mut cfg = ObjectMap::new();
        if op == "load" {
            let uris = self.uris(ex, headers::SOURCE_URIS, "sourceUris");
            if uris.is_empty() {
                return Err(CamelError::illegal_argument("load needs sourceUris (option, CamelGoogleBigQuerySourceUris header or a list body of gs:// uris)"));
            }
            cfg.insert("sourceUris".into(), Value::from(uris));
            cfg.insert("destinationTable".into(), table_ref);
            cfg.insert("sourceFormat".into(), Value::from(opt("sourceFormat").unwrap_or_else(|| "CSV".into())));
            cfg.insert("writeDisposition".into(), Value::from(opt("writeDisposition").unwrap_or_else(|| "WRITE_APPEND".into())));
            if let Some(n) = opt("skipLeadingRows") {
                cfg.insert("skipLeadingRows".into(), Value::Int(n.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid skipLeadingRows: {}", n)))?));
            }
            for k in ["fieldDelimiter", "createDisposition"] {
                if let Some(v) = opt(k) {
                    cfg.insert(k.into(), Value::from(v));
                }
            }
            if let Some(a) = opt("autodetect") {
                cfg.insert("autodetect".into(), Value::Bool(a.eq_ignore_ascii_case("true")));
            }
        } else {
            let uris = self.uris(ex, headers::DESTINATION_URIS, "destinationUris");
            if uris.is_empty() {
                return Err(CamelError::illegal_argument("extract needs destinationUris (option, CamelGoogleBigQueryDestinationUris header or a list body of gs:// uris)"));
            }
            cfg.insert("sourceTable".into(), table_ref);
            cfg.insert("destinationUris".into(), Value::from(uris));
            cfg.insert("destinationFormat".into(), Value::from(opt("destinationFormat").unwrap_or_else(|| "NEWLINE_DELIMITED_JSON".into())));
            if let Some(p) = opt("printHeader") {
                cfg.insert("printHeader".into(), Value::Bool(p.eq_ignore_ascii_case("true")));
            }
            if let Some(c) = opt("compression") {
                cfg.insert("compression".into(), Value::from(c));
            }
        }
        let job_id = format!("camel_{}_{}", op, crate::util::random_hex(8));
        let body = obj([
            ("jobReference", obj([("projectId", Value::from(self.project.clone())), ("jobId", Value::from(job_id.clone()))])),
            ("configuration", obj([(op, Value::Object(cfg))])),
        ]);
        let base = base(&self.gcp, &self.project);
        let mut job = self.gcp.json("POST", &format!("{}/jobs", base), Some(&body), kinds::BIGQUERY)?;
        let deadline = std::time::Instant::now() + self.job_timeout;
        while job.get("status").and_then(|s| s.get("state")).map(|s| s.to_string()).as_deref() != Some("DONE") {
            if std::time::Instant::now() >= deadline {
                return Err(cloud_error(kinds::BIGQUERY, format!("BigQuery job {} did not finish within {:?} (code: 408, status: DEADLINE_EXCEEDED)", job_id, self.job_timeout)));
            }
            std::thread::sleep(self.job_poll_interval);
            job = self.gcp.json("GET", &format!("{}/jobs/{}", base, uri_encode(&job_id, false)), None, kinds::BIGQUERY)?;
        }
        ex.set_header(headers::JOB_ID, job_id);
        if let Some(err) = job.get("status").and_then(|s| s.get("errorResult")) {
            return Err(cloud_error(kinds::BIGQUERY, format!("BigQuery {} job failed: {} (code: 400, status: {})", op, err.get("message").map(|m| m.to_string()).unwrap_or_default(), err.get("reason").map(|m| m.to_string()).unwrap_or_default())));
        }
        ex.set_body(job);
        Ok(())
    }
}

impl CloudService for BigQueryService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = crate::endpoint::operation(ex, headers::OPERATION, &self.operation, "insert");
        match op.as_str() {
            "insert" => {
                if self.create_table_if_not_exists {
                    let table = header_str(ex, headers::TABLE_ID).or_else(|| self.table.clone()).ok_or_else(|| CamelError::illegal_argument("Table id must be specified (uri or CamelGoogleBigQueryTableId)"))?;
                    let url = format!("{}/datasets/{}/tables/{}", base(&self.gcp, &self.project), uri_encode(&self.dataset, false), uri_encode(&table, false));
                    if let Err(e) = self.gcp.json("GET", &url, None, kinds::BIGQUERY) {
                        if super::error_status(&e) != Some(404) {
                            return Err(e);
                        }
                        self.create_table(ex, &table)?;
                    }
                }
            }
            "load" | "extract" => return self.job(ex, &op),
            "createTable" | "getTable" | "listTables" | "deleteTable" | "createDataset" | "listDatasets" | "deleteDataset" => return self.table_op(ex, &op),
            other => return Err(crate::endpoint::unsupported("google-bigquery", other)),
        }
        let mut table = header_str(ex, headers::TABLE_ID).or_else(|| self.table.clone()).ok_or_else(|| CamelError::illegal_argument("Table id must be specified (uri or CamelGoogleBigQueryTableId)"))?;
        if let Some(p) = header_str(ex, headers::PARTITION_DECORATOR) {
            table = format!("{}${}", table, p);
        }
        let rows_in: Vec<Value> = match ex.body() {
            Value::Array(a) => a.clone(),
            v @ Value::Object(_) => vec![v.clone()],
            other => vec![crate::util::parse_json(&other.to_bytes())?],
        };
        let header_insert_id = header_str(ex, headers::INSERT_ID);
        let mut rows = Vec::new();
        for (i, r) in rows_in.iter().enumerate() {
            if r.as_object().is_none() {
                return Err(CamelError::illegal_argument("BigQuery rows must be maps"));
            }
            let mut m = ObjectMap::new();
            m.insert("json".to_string(), r.clone());
            let id = match (&self.use_as_insert_id, &header_insert_id) {
                (Some(f), _) => r.get(f).map(|v| v.to_string()),
                (None, Some(h)) if rows_in.len() == 1 => Some(h.clone()),
                (None, Some(h)) => Some(format!("{}-{}", h, i)),
                _ => None,
            };
            if let Some(id) = id {
                m.insert("insertId".into(), Value::from(id));
            }
            rows.push(Value::Object(m));
        }
        let mut req = ObjectMap::new();
        req.insert("rows".to_string(), Value::Array(rows));
        if let Some(sfx) = header_str(ex, headers::TABLE_SUFFIX) {
            req.insert("templateSuffix".into(), Value::from(sfx));
        }
        let url = format!("{}/datasets/{}/tables/{}/insertAll", base(&self.gcp, &self.project), uri_encode(&self.dataset, false), uri_encode(&table, false));
        let r = self.gcp.json("POST", &url, Some(&Value::Object(req)), kinds::BIGQUERY)?;
        if let Some(errs) = r.get("insertErrors").and_then(|e| e.as_array()).filter(|e| !e.is_empty()) {
            return Err(cloud_error(kinds::BIGQUERY, format!("{} row(s) were rejected by BigQuery: {} (code: 400, status: INVALID_ARGUMENT)", errs.len(), Value::Array(errs.clone()).to_json())));
        }
        Ok(())
    }
}

/// The SQL query endpoint.
pub struct BigQuerySqlService {
    pub gcp: GcpConfig,
    pub project: String,
    pub query: String,
}

/// Factory for `google-bigquery-sql`.
pub fn create_sql(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (project, query) = remaining.split_once(':').filter(|(p, q)| !p.is_empty() && !q.is_empty()).ok_or_else(|| CamelError::illegal_argument("Expected google-bigquery-sql://projectId:query"))?;
    let query = if let Some(path) = query.strip_prefix("file:").or_else(|| query.strip_prefix("classpath:")) {
        std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read query resource {}: {}", path, e)))?
    } else {
        crate::util::percent_decode(query, false)
    };
    let gcp = GcpConfig::from_opts(o)?;
    o.ignore(&["connectionFactory", "projectId"]);
    Ok(Arc::new(BigQuerySqlService { project: project.to_string(), query, gcp }))
}

/// Replace `${header.name}`, `${headers.name}`, `${exchangeProperty.name}` and `${body}` in a query.
pub fn translate_query(q: &str, ex: &Exchange) -> String {
    let mut out = String::new();
    let mut rest = q;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = &rest[start + 2..start + end];
        let v = if let Some(h) = expr.strip_prefix("header.").or_else(|| expr.strip_prefix("headers.")) {
            ex.header(h).map(|v| v.to_string()).unwrap_or_default()
        } else if let Some(p) = expr.strip_prefix("exchangeProperty.") {
            ex.property(p).map(|v| v.to_string()).unwrap_or_default()
        } else if expr == "body" {
            ex.body().to_string()
        } else {
            format!("${{{}}}", expr)
        };
        out.push_str(&v);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Names of the `@params` of a query (outside string literals).
pub fn query_parameters(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let c: Vec<char> = q.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < c.len() {
        match (quote, c[i]) {
            (Some(qc), ch) if ch == qc => quote = None,
            (None, '\'' | '"' | '`') => quote = Some(c[i]),
            (None, '@') => {
                let start = i + 1;
                let mut j = start;
                while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                    j += 1;
                }
                let name: String = c[start..j].iter().collect();
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn param_type(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "BOOL",
        Value::Int(_) => "INT64",
        Value::Float(_) => "FLOAT64",
        Value::Bytes(_) => "BYTES",
        _ => "STRING",
    }
}

/// Convert the `rows` of a query response to maps after the `schema`.
pub fn rows_to_maps(schema: &Value, rows: &[Value]) -> Vec<Value> {
    let fields: Vec<(String, String)> = schema
        .get("fields")
        .and_then(|f| f.as_array())
        .map(|a| a.iter().map(|f| (f.get("name").map(|n| n.to_string()).unwrap_or_default(), f.get("type").map(|t| t.to_string()).unwrap_or_default())).collect())
        .unwrap_or_default();
    rows.iter()
        .map(|r| {
            let cells = r.get("f").and_then(|f| f.as_array()).cloned().unwrap_or_default();
            let mut m = ObjectMap::new();
            for (i, (name, ty)) in fields.iter().enumerate() {
                let v = cells.get(i).and_then(|c| c.get("v")).cloned().unwrap_or_default();
                let typed = match (&v, ty.as_str()) {
                    (Value::Null, _) => Value::Null,
                    (_, "INTEGER" | "INT64") => v.as_i64().map(Value::Int).unwrap_or(v),
                    (_, "FLOAT" | "FLOAT64" | "NUMERIC") => v.as_f64().map(Value::Float).unwrap_or(v),
                    (_, "BOOLEAN" | "BOOL") => v.as_bool().map(Value::Bool).unwrap_or(v),
                    _ => v,
                };
                m.insert(name.clone(), typed);
            }
            Value::Object(m)
        })
        .collect()
}

impl CloudService for BigQuerySqlService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let translated = translate_query(&self.query, ex);
        ex.set_header(headers::TRANSLATED_QUERY, translated.clone());
        let mut params = Vec::new();
        for name in query_parameters(&translated) {
            let v = ex.header(&name).cloned().or_else(|| ex.body().get(&name).cloned()).ok_or_else(|| CamelError::runtime(format!("Query parameter @{} was not found in the headers or the body", name)))?;
            params.push(obj([
                ("name", Value::from(name)),
                ("parameterType", obj([("type", Value::from(param_type(&v)))])),
                ("parameterValue", obj([("value", Value::from(v.to_string()))])),
            ]));
        }
        let req = obj([
            ("query", Value::from(translated)),
            ("useLegacySql", Value::Bool(false)),
            ("parameterMode", Value::from("NAMED")),
            ("queryParameters", Value::Array(params)),
            ("timeoutMs", Value::Int(10_000)),
        ]);
        let b = base(&self.gcp, &self.project);
        let mut r = self.gcp.json("POST", &format!("{}/queries", b), Some(&req), kinds::BIGQUERY)?;
        let job = r.get("jobReference").and_then(|j| j.get("jobId")).map(|j| j.to_string()).unwrap_or_default();
        ex.set_header(headers::JOB_ID, job.clone());
        let mut polls = 0;
        while !r.get("jobComplete").is_some_and(|c| c.is_truthy()) {
            polls += 1;
            if polls > 600 {
                return Err(cloud_error(kinds::BIGQUERY, format!("Query job {} did not complete", job)));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            r = self.gcp.json("GET", &format!("{}/queries/{}?timeoutMs=10000", b, uri_encode(&job, false)), None, kinds::BIGQUERY)?;
        }
        if let Some(n) = r.get("numDmlAffectedRows") {
            ex.set_body(Value::Int(n.as_i64().unwrap_or(0)));
            return Ok(());
        }
        let schema = r.get("schema").cloned().unwrap_or_default();
        let mut rows = rows_to_maps(&schema, &r.get("rows").and_then(|x| x.as_array()).cloned().unwrap_or_default());
        let mut token = r.get("pageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
        while let Some(t) = token {
            let page = self.gcp.json("GET", &format!("{}/queries/{}?pageToken={}", b, uri_encode(&job, false), uri_encode(&t, false)), None, kinds::BIGQUERY)?;
            rows.extend(rows_to_maps(&schema, &page.get("rows").and_then(|x| x.as_array()).cloned().unwrap_or_default()));
            token = page.get("pageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
        }
        ex.set_body(Value::Array(rows));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_and_translation() {
        assert_eq!(query_parameters("SELECT * FROM t WHERE a = @a AND b = '@notparam' OR c = @c_2 AND a = @a"), vec!["a", "c_2"]);
        let mut ex = Exchange::new(None);
        ex.set_header("table", "events");
        assert_eq!(translate_query("SELECT * FROM ds.${header.table} WHERE x = ${unknown}", &ex), "SELECT * FROM ds.events WHERE x = ${unknown}");
        let schema = crate::util::parse_json(br#"{"fields":[{"name":"n","type":"INTEGER"},{"name":"ok","type":"BOOLEAN"},{"name":"s","type":"STRING"}]}"#).unwrap();
        let rows = crate::util::parse_json(br#"[{"f":[{"v":"42"},{"v":"true"},{"v":null}]}]"#).unwrap();
        let m = rows_to_maps(&schema, rows.as_array().unwrap());
        assert_eq!(m[0], Value::object([("n", Value::Int(42)), ("ok", Value::Bool(true)), ("s", Value::Null)]));
    }
}
