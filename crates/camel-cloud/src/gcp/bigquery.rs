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
//! `google-bigquery-sql://projectId:query?...` runs a standard SQL query with `jobs.query`
//! (polling `getQueryResults` until complete, reading all pages). The query may be
//! `classpath:`/`file:` resource. `@name` parameters are bound from the header `name`, else
//! the body map entry `name`; `${header.x}` / `${body}`-style placeholders are substituted first
//! (`CamelGoogleBigQueryTranslatedQuery`). Body = number of affected rows for DML, otherwise
//! the list of rows (maps typed after the result schema). `CamelGoogleBigQueryJobId` is set.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

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
}

/// Factory for `google-bigquery`.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let parts: Vec<&str> = remaining.split(':').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(CamelError::illegal_argument("Expected google-bigquery://projectId:datasetId[:tableId]"));
    }
    let gcp = GcpConfig::from_opts(o)?;
    o.ignore(&["connectionFactory"]);
    Ok(Arc::new(BigQueryService {
        project: parts[0].to_string(),
        dataset: parts[1].to_string(),
        table: parts.get(2).map(|t| t.to_string()).filter(|t| !t.is_empty()),
        use_as_insert_id: o.get("useAsInsertId"),
        gcp,
    }))
}

impl CloudService for BigQueryService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
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
