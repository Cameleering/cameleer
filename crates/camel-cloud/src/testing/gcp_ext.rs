//! More emulated Google services on the [`GcpMock`] port (all requiring a bearer token from the
//! emulator's token endpoint): BigQuery (`insertAll`, `jobs.query` / `getQueryResults` with a
//! small standard-SQL subset), Secret Manager, Cloud Functions (registered closures),
//! Firestore (documents + `runQuery`) and Sheets (values API; tokens must carry the
//! `spreadsheets` scope).

use std::collections::{BTreeMap, HashMap};

use camel_core::value::{compare_values, ObjectMap, Value, values_equal};

use super::{gerror, GcpMock, Shared};
use crate::gcp::firestore::{document_fields, fields_of, from_firestore};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, iso8601_millis, now_millis, obj, parse_json, random_hex};

/// A registered Cloud Function: data in, result out (`Err` = function error).
pub type CloudFn = std::sync::Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

struct BqTable {
    schema: Vec<(String, String)>,
    rows: Vec<ObjectMap>,
    insert_ids: Vec<String>,
}

struct BqJob {
    schema: Vec<(String, String)>,
    rows: Vec<Value>,
    dml: Option<i64>,
    pending_polls: usize,
}

struct Spreadsheet {
    title: String,
    sheets: Vec<(String, Vec<Vec<Value>>)>,
}

/// State of the extra services.
#[derive(Default)]
pub(super) struct ExtState {
    tables: BTreeMap<String, BqTable>,
    jobs: HashMap<String, BqJob>,
    bq_async_polls: usize,
    bq_page_size: usize,
    secrets: BTreeMap<String, Vec<Vec<u8>>>,
    functions: BTreeMap<String, CloudFn>,
    docs: BTreeMap<String, (ObjectMap, i64, i64)>,
    sheets: BTreeMap<String, Spreadsheet>,
    pub(super) token_scopes: HashMap<String, String>,
    /// BigQuery load / extract jobs: id -> (job resource, polls left before DONE)
    bq_jobs: HashMap<String, (Value, usize)>,
    /// BigQuery datasets `project.dataset` -> (location, created)
    bq_datasets: BTreeMap<String, (String, i64)>,
    /// table metadata (partitioning, clustering, description) by `project.dataset.table`
    bq_table_meta: BTreeMap<String, Value>,
    /// Cloud Functions resources created / updated through the API
    fn_meta: BTreeMap<String, Value>,
    /// uploaded source archives (upload id -> zip bytes)
    fn_uploads: BTreeMap<String, Vec<u8>>,
    /// long-running operations (name -> (resource, polls before done))
    fn_ops: BTreeMap<String, (Value, usize)>,
    /// Firestore transactions: id -> (read only, documents read with their update time)
    fs_tx: HashMap<String, (bool, BTreeMap<String, i64>)>,
    fs_aborted: usize,
}

impl GcpMock {
    // ---- BigQuery
    /// Create a table `project.dataset.table` with `(name, type)` columns (STRING, INTEGER, FLOAT, BOOLEAN...).
    pub fn create_bq_table(&self, project: &str, dataset: &str, table: &str, schema: &[(&str, &str)]) {
        self.state.lock().unwrap().ext.tables.insert(
            format!("{}.{}.{}", project, dataset, table),
            BqTable { schema: schema.iter().map(|(n, t)| (n.to_string(), t.to_string())).collect(), rows: vec![], insert_ids: vec![] },
        );
    }
    /// Rows of a table.
    pub fn bq_rows(&self, project: &str, dataset: &str, table: &str) -> Vec<Value> {
        self.state.lock().unwrap().ext.tables.get(&format!("{}.{}.{}", project, dataset, table)).map(|t| t.rows.iter().map(|r| Value::Object(r.clone())).collect()).unwrap_or_default()
    }
    /// Make queries report `jobComplete=false` for `polls` `getQueryResults` calls.
    pub fn set_bq_async_polls(&self, polls: usize) {
        self.state.lock().unwrap().ext.bq_async_polls = polls;
    }
    /// Page size of query results (0 = everything in one page).
    pub fn set_bq_page_size(&self, n: usize) {
        self.state.lock().unwrap().ext.bq_page_size = n;
    }

    // ---- Secret Manager
    /// Add a secret version directly.
    pub fn put_secret(&self, project: &str, secret: &str, data: &[u8]) {
        self.state.lock().unwrap().ext.secrets.entry(format!("projects/{}/secrets/{}", project, secret)).or_default().push(data.to_vec());
    }
    /// Versions of a secret.
    pub fn secret_versions(&self, project: &str, secret: &str) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().ext.secrets.get(&format!("projects/{}/secrets/{}", project, secret)).cloned().unwrap_or_default()
    }

    // ---- Cloud Functions
    /// Register a function implementation.
    pub fn register_function(&self, project: &str, location: &str, name: &str, f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) {
        self.state.lock().unwrap().ext.functions.insert(format!("projects/{}/locations/{}/functions/{}", project, location, name), std::sync::Arc::new(f));
    }
    pub fn function_names(&self) -> Vec<String> {
        self.state.lock().unwrap().ext.functions.keys().cloned().collect()
    }

    // ---- Firestore
    /// Fields of a document (`collection/id` path under the default database of `project`).
    pub fn firestore_doc(&self, project: &str, path: &str) -> Option<Value> {
        self.state.lock().unwrap().ext.docs.get(&format!("projects/{}/databases/(default)/documents/{}", project, path)).map(|(f, _, _)| Value::Object(f.clone()))
    }
    /// Firestore transactions aborted by a conflicting write.
    pub fn firestore_aborted_transactions(&self) -> usize {
        self.state.lock().unwrap().ext.fs_aborted
    }
    /// Store a document directly.
    pub fn put_firestore_doc(&self, project: &str, path: &str, fields: Value) {
        let now = now_millis();
        self.state.lock().unwrap().ext.docs.insert(format!("projects/{}/databases/(default)/documents/{}", project, path), (fields.as_object().cloned().unwrap_or_default(), now, now));
    }

    // ---- Sheets
    /// Create a spreadsheet with one sheet `Sheet1`; returns its id.
    pub fn create_spreadsheet(&self, title: &str) -> String {
        let id = format!("1{}", random_hex(10));
        self.state.lock().unwrap().ext.sheets.insert(id.clone(), Spreadsheet { title: title.into(), sheets: vec![("Sheet1".into(), vec![])] });
        id
    }
    /// The cells of a sheet (rows of string values).
    pub fn sheet_values(&self, id: &str, sheet: &str) -> Vec<Vec<String>> {
        let s = self.state.lock().unwrap();
        s.ext.sheets.get(id).and_then(|sp| sp.sheets.iter().find(|(n, _)| n == sheet)).map(|(_, g)| g.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap_or_default()
    }
}

/// Dispatch a request for one of the extra services (`None` = not handled here).
pub(super) fn handle(st: &Shared, req: &ServerRequest, s: &[&str]) -> Option<Response> {
    Some(match s {
        ["bigquery", "v2", "projects", project, rest @ ..] => bigquery(st, req, project, rest),
        ["v1", "projects", project, "secrets", rest @ ..] => secrets(st, req, project, rest),
        ["v1", "projects", project, "locations", location, "functions", rest @ ..] => functions(st, req, project, location, rest),
        ["v1", "projects", project, "locations", location, "functions:generateUploadUrl"] => functions(st, req, project, location, &[":generateUploadUrl"]),
        ["v1", "projects", project, "databases", db, rest @ ..] => firestore(st, req, project, db, rest),
        ["v4", "spreadsheets", rest @ ..] => sheets(st, req, rest),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// BigQuery
// ---------------------------------------------------------------------------

fn bq_coerce(v: &Value, ty: &str) -> Result<Value, String> {
    Ok(match (v, ty) {
        (Value::Null, _) => Value::Null,
        (_, "INTEGER" | "INT64") => Value::Int(v.as_i64().ok_or_else(|| format!("Cannot convert value to integer: {}", v))?),
        (_, "FLOAT" | "FLOAT64" | "NUMERIC") => Value::Float(v.as_f64().ok_or_else(|| format!("Cannot convert value to float: {}", v))?),
        (_, "BOOLEAN" | "BOOL") => Value::Bool(v.as_bool().ok_or_else(|| format!("Cannot convert value to boolean: {}", v))?),
        _ => Value::from(v.to_string()),
    })
}

fn bigquery(st: &Shared, req: &ServerRequest, project: &str, rest: &[&str]) -> Response {
    let mut guard = st.lock().unwrap();
    let ext = &mut guard.ext;
    let body = parse_json(&req.body).unwrap_or_default();
    match (req.method.as_str(), rest) {
        ("POST", ["datasets", dataset, "tables", table, "insertAll"]) => {
            let table = table.split('$').next().unwrap_or(table);
            let base = format!("{}.{}.{}", project, dataset, table);
            let Some(schema) = ext.tables.get(&base).map(|t| t.schema.clone()) else { return gerror(404, "notFound", &format!("Not found: Table {}:{}.{}", project, dataset, table)) };
            let key = match body.get("templateSuffix").map(|s| s.to_string()).filter(|s| !s.is_empty()) {
                Some(sfx) => {
                    let k = format!("{}{}", base, sfx);
                    ext.tables.entry(k.clone()).or_insert_with(|| BqTable { schema: schema.clone(), rows: vec![], insert_ids: vec![] });
                    k
                }
                None => base,
            };
            let t = ext.tables.get_mut(&key).expect("exists");
            let mut errors = Vec::new();
            let rows = body.get("rows").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            let mut accepted = Vec::new();
            for (i, r) in rows.iter().enumerate() {
                let json = r.get("json").and_then(|j| j.as_object()).cloned().unwrap_or_default();
                let mut row = ObjectMap::new();
                let mut err: Option<String> = None;
                for (k, v) in &json {
                    match t.schema.iter().find(|(n, _)| n == k) {
                        None => err = Some(format!("no such field: {}.", k)),
                        Some((_, ty)) => match bq_coerce(v, ty) {
                            Ok(x) => {
                                row.insert(k.clone(), x);
                            }
                            Err(e) => err = Some(e),
                        },
                    }
                }
                match err {
                    Some(e) => errors.push(obj([("index", Value::Int(i as i64)), ("errors", Value::Array(vec![obj([("reason", Value::from("invalid")), ("message", Value::from(e))])]))])),
                    None => accepted.push((r.get("insertId").map(|x| x.to_string()), row)),
                }
            }
            // like BigQuery, a request with invalid rows inserts nothing (skipInvalidRows=false)
            if errors.is_empty() {
                for (id, row) in accepted {
                    if let Some(id) = id {
                        if t.insert_ids.contains(&id) {
                            continue;
                        }
                        t.insert_ids.push(id);
                    }
                    t.rows.push(row);
                }
                return Response::json(200, obj([("kind", Value::from("bigquery#tableDataInsertAllResponse"))]).to_json());
            }
            Response::json(200, obj([("kind", Value::from("bigquery#tableDataInsertAllResponse")), ("insertErrors", Value::Array(errors))]).to_json())
        }
        ("POST", ["jobs"]) => {
            drop(guard);
            bq_job(st, project, &body)
        }
        ("POST", ["datasets"]) => {
            let id = body.get("datasetReference").and_then(|r| r.get("datasetId")).map(|d| d.to_string()).unwrap_or_default();
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return gerror(400, "invalid", "Invalid dataset ID");
            }
            let key = format!("{}.{}", project, id);
            if ext.bq_datasets.contains_key(&key) {
                return gerror(409, "duplicate", &format!("Already Exists: Dataset {}:{}", project, id));
            }
            let location = body.get("location").map(|l| l.to_string()).unwrap_or_else(|| "US".into());
            ext.bq_datasets.insert(key, (location.clone(), crate::util::now_millis()));
            Response::json(200, obj([("kind", Value::from("bigquery#dataset")), ("id", Value::from(format!("{}:{}", project, id))), ("datasetReference", obj([("projectId", Value::from(project)), ("datasetId", Value::from(id))])), ("location", Value::from(location))]).to_json())
        }
        ("GET", ["datasets"]) => {
            let mut ids: std::collections::BTreeSet<String> = ext.bq_datasets.keys().filter_map(|k| k.strip_prefix(&format!("{}.", project)).map(|d| d.to_string())).collect();
            ids.extend(ext.tables.keys().filter_map(|k| k.strip_prefix(&format!("{}.", project)).and_then(|r| r.split('.').next()).map(|d| d.to_string())));
            let list: Vec<Value> = ids.into_iter().map(|d| obj([("kind", Value::from("bigquery#dataset")), ("datasetReference", obj([("projectId", Value::from(project)), ("datasetId", Value::from(d))]))])).collect();
            Response::json(200, obj([("kind", Value::from("bigquery#datasetList")), ("datasets", Value::Array(list))]).to_json())
        }
        ("DELETE", ["datasets", dataset]) => {
            let key = format!("{}.{}", project, dataset);
            let prefix = format!("{}.", key);
            let has_tables = ext.tables.keys().any(|k| k.starts_with(&prefix));
            if has_tables && req.query_param("deleteContents").as_deref() != Some("true") {
                return gerror(400, "resourceInUse", &format!("Dataset {}:{} is still in use", project, dataset));
            }
            if ext.bq_datasets.remove(&key).is_none() && !has_tables {
                return gerror(404, "notFound", &format!("Not found: Dataset {}:{}", project, dataset));
            }
            ext.tables.retain(|k, _| !k.starts_with(&prefix));
            Response::new(204)
        }
        ("POST", ["datasets", dataset, "tables"]) => {
            let dkey = format!("{}.{}", project, dataset);
            let prefix = format!("{}.", dkey);
            if !ext.bq_datasets.contains_key(&dkey) && !ext.tables.keys().any(|k| k.starts_with(&prefix)) {
                return gerror(404, "notFound", &format!("Not found: Dataset {}:{}", project, dataset));
            }
            let table = body.get("tableReference").and_then(|r| r.get("tableId")).map(|t| t.to_string()).unwrap_or_default();
            if table.is_empty() {
                return gerror(400, "invalid", "Invalid table ID");
            }
            let key = format!("{}{}", prefix, table);
            if ext.tables.contains_key(&key) {
                return gerror(409, "duplicate", &format!("Already Exists: Table {}:{}.{}", project, dataset, table));
            }
            let fields = body.get("schema").and_then(|sc| sc.get("fields")).and_then(|f| f.as_array()).cloned().unwrap_or_default();
            let mut schema = Vec::new();
            for f in &fields {
                let (n, t) = (f.get("name").map(|x| x.to_string()).unwrap_or_default(), f.get("type").map(|x| x.to_string()).unwrap_or_default().to_ascii_uppercase());
                if n.is_empty() || !["STRING", "INTEGER", "INT64", "FLOAT", "FLOAT64", "BOOLEAN", "BOOL", "TIMESTAMP", "DATE", "NUMERIC", "BYTES", "JSON"].contains(&t.as_str()) {
                    return gerror(400, "invalid", &format!("Invalid field: {}", f.to_json()));
                }
                schema.push((n, t));
            }
            if let Some(tp) = body.get("timePartitioning").and_then(|t| t.get("field")) {
                if !schema.iter().any(|(n, t)| *n == tp.to_string() && matches!(t.as_str(), "TIMESTAMP" | "DATE")) {
                    return gerror(400, "invalid", "The field specified for time partitioning must be a TIMESTAMP or DATE column");
                }
            }
            ext.tables.insert(key.clone(), BqTable { schema, rows: vec![], insert_ids: vec![] });
            let mut meta = body.as_object().cloned().unwrap_or_default();
            meta.insert("kind".into(), Value::from("bigquery#table"));
            meta.insert("id".into(), Value::from(format!("{}:{}.{}", project, dataset, table)));
            meta.insert("tableReference".into(), obj([("projectId", Value::from(project)), ("datasetId", Value::from(*dataset)), ("tableId", Value::from(table.clone()))]));
            meta.insert("type".into(), Value::from("TABLE"));
            meta.insert("creationTime".into(), Value::from(crate::util::now_millis().to_string()));
            ext.bq_table_meta.insert(key, Value::Object(meta.clone()));
            Response::json(200, Value::Object(meta).to_json())
        }
        ("GET", ["datasets", dataset, "tables"]) => {
            let prefix = format!("{}.{}.", project, dataset);
            let list: Vec<Value> = ext.tables.keys().filter_map(|k| k.strip_prefix(&prefix)).map(|t| obj([("kind", Value::from("bigquery#table")), ("tableReference", obj([("projectId", Value::from(project)), ("datasetId", Value::from(*dataset)), ("tableId", Value::from(t))]))])).collect();
            Response::json(200, obj([("kind", Value::from("bigquery#tableList")), ("totalItems", Value::Int(list.len() as i64)), ("tables", Value::Array(list))]).to_json())
        }
        ("GET", ["datasets", dataset, "tables", table]) => {
            let key = format!("{}.{}.{}", project, dataset, table);
            let Some(t) = ext.tables.get(&key) else { return gerror(404, "notFound", &format!("Not found: Table {}:{}.{}", project, dataset, table)) };
            let mut meta = ext.bq_table_meta.get(&key).and_then(|m| m.as_object().cloned()).unwrap_or_default();
            meta.insert("kind".into(), Value::from("bigquery#table"));
            meta.insert("tableReference".into(), obj([("projectId", Value::from(project)), ("datasetId", Value::from(*dataset)), ("tableId", Value::from(*table))]));
            meta.insert("schema".into(), obj([("fields", Value::Array(t.schema.iter().map(|(n, ty)| obj([("name", Value::from(n.clone())), ("type", Value::from(ty.clone()))])).collect()))]));
            meta.insert("numRows".into(), Value::from(t.rows.len().to_string()));
            Response::json(200, Value::Object(meta).to_json())
        }
        ("DELETE", ["datasets", dataset, "tables", table]) => {
            let key = format!("{}.{}.{}", project, dataset, table);
            ext.bq_table_meta.remove(&key);
            match ext.tables.remove(&key) {
                Some(_) => Response::new(204),
                None => gerror(404, "notFound", &format!("Not found: Table {}:{}.{}", project, dataset, table)),
            }
        }
        ("GET", ["jobs", id]) => match ext.bq_jobs.get_mut(*id) {
            Some((job, polls)) => {
                if *polls > 0 {
                    *polls -= 1;
                    let mut j = job.as_object().cloned().unwrap_or_default();
                    j.insert("status".into(), obj([("state", Value::from("RUNNING"))]));
                    return Response::json(200, Value::Object(j).to_json());
                }
                Response::json(200, job.to_json())
            }
            None => gerror(404, "notFound", &format!("Not found: Job {}:{}", project, id)),
        },
        ("POST", ["queries"]) => {
            let text = body.get("query").map(|q| q.to_string()).unwrap_or_default();
            if body.get("useLegacySql").is_some_and(|l| l.is_truthy()) {
                return gerror(400, "invalidQuery", "Legacy SQL is not supported by the emulator");
            }
            let mut params = HashMap::new();
            for p in body.get("queryParameters").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                let name = p.get("name").map(|n| n.to_string()).unwrap_or_default();
                let ty = p.get("parameterType").and_then(|t| t.get("type")).map(|t| t.to_string()).unwrap_or_else(|| "STRING".into());
                let raw = p.get("parameterValue").and_then(|v| v.get("value")).cloned().unwrap_or_default();
                params.insert(name, bq_coerce(&raw, &ty).unwrap_or(raw));
            }
            let job = match run_sql(ext, project, &text, &params) {
                Ok(j) => j,
                Err(e) => return gerror(400, "invalidQuery", &e),
            };
            let id = format!("job_{}", random_hex(8));
            let pending = ext.bq_async_polls;
            ext.jobs.insert(id.clone(), BqJob { pending_polls: pending, ..job });
            query_response(ext, project, &id, None)
        }
        ("GET", ["queries", job]) => {
            let Some(j) = ext.jobs.get_mut(*job) else { return gerror(404, "notFound", &format!("Not found: Job {}:{}", project, job)) };
            j.pending_polls = j.pending_polls.saturating_sub(1);
            query_response(ext, project, job, req.query_param("pageToken"))
        }
        _ => gerror(400, "invalid", "Unsupported BigQuery request"),
    }
}

fn csv_fields(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delim && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn csv_escape(v: &Value) -> String {
    let s = v.to_string();
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// Split `gs://bucket/object`.
fn gs(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("gs://")?;
    let (b, o) = rest.split_once('/')?;
    Some((b.to_string(), o.to_string()))
}

/// Run a load / extract job against the emulated Cloud Storage and tables.
fn bq_job(st: &Shared, project: &str, body: &Value) -> Response {
    let mut guard = st.lock().unwrap();
    let job_id = body.get("jobReference").and_then(|r| r.get("jobId")).map(|j| j.to_string()).unwrap_or_else(|| format!("job_{}", random_hex(8)));
    let cfg = body.get("configuration").cloned().unwrap_or_default();
    let table_key = |t: &Value| format!("{}.{}.{}", t.get("projectId").map(|p| p.to_string()).unwrap_or_else(|| project.to_string()), t.get("datasetId").map(|p| p.to_string()).unwrap_or_default(), t.get("tableId").map(|p| p.to_string()).unwrap_or_default());
    let mut stats = ObjectMap::new();
    let result: Result<(), (String, String)> = (|| {
        if let Some(load) = cfg.get("load") {
            let key = table_key(&load.get("destinationTable").cloned().unwrap_or_default());
            let format = load.get("sourceFormat").map(|f| f.to_string()).unwrap_or_else(|| "CSV".into());
            let skip = load.get("skipLeadingRows").and_then(|s| s.as_i64()).unwrap_or(0) as usize;
            let delim = load.get("fieldDelimiter").map(|d| d.to_string()).and_then(|d| d.chars().next()).unwrap_or(',');
            let schema = guard.ext.tables.get(&key).map(|t| t.schema.clone()).ok_or_else(|| ("notFound".to_string(), format!("Not found: Table {}", key)))?;
            let mut rows: Vec<ObjectMap> = Vec::new();
            for uri in load.get("sourceUris").and_then(|u| u.as_array()).cloned().unwrap_or_default() {
                let uri = uri.to_string();
                let (b, o) = gs(&uri).ok_or_else(|| ("invalid".to_string(), format!("Invalid source uri {}", uri)))?;
                let data = guard.buckets.get(&b).and_then(|bk| bk.objects.get(&o)).map(|x| x.data.clone()).ok_or_else(|| ("notFound".to_string(), format!("Not found: URI {}", uri)))?;
                let text = String::from_utf8_lossy(&data).into_owned();
                for line in text.lines().skip(if format == "CSV" { skip } else { 0 }).filter(|l| !l.trim().is_empty()) {
                    let mut row = ObjectMap::new();
                    match format.as_str() {
                        "CSV" => {
                            let fields = csv_fields(line, delim);
                            if fields.len() != schema.len() {
                                return Err(("invalid".into(), format!("Error while reading data, error message: CSV table encountered too many errors; expected {} columns but got {}", schema.len(), fields.len())));
                            }
                            for ((name, ty), f) in schema.iter().zip(fields) {
                                let v = if f.is_empty() { Value::Null } else { bq_coerce(&Value::from(f.clone()), ty).map_err(|e| ("invalid".to_string(), e))? };
                                row.insert(name.clone(), v);
                            }
                        }
                        "NEWLINE_DELIMITED_JSON" => {
                            let v = parse_json(line.as_bytes()).map_err(|e| ("invalid".to_string(), e.to_string()))?;
                            for (k, x) in v.as_object().cloned().unwrap_or_default() {
                                let ty = schema.iter().find(|(n, _)| *n == k).map(|(_, t)| t.clone()).ok_or_else(|| ("invalid".to_string(), format!("no such field: {}.", k)))?;
                                row.insert(k, bq_coerce(&x, &ty).map_err(|e| ("invalid".to_string(), e))?);
                            }
                        }
                        other => return Err(("invalid".into(), format!("Source format {} is not supported by the emulator", other))),
                    }
                    rows.push(row);
                }
            }
            let t = guard.ext.tables.get_mut(&key).expect("checked");
            match load.get("writeDisposition").map(|w| w.to_string()).as_deref().unwrap_or("WRITE_APPEND") {
                "WRITE_TRUNCATE" => t.rows.clear(),
                "WRITE_EMPTY" if !t.rows.is_empty() => return Err(("duplicate".into(), format!("Already Exists: Table {}", key))),
                _ => {}
            }
            stats.insert("load".into(), obj([("outputRows", Value::from(rows.len().to_string())), ("inputFiles", Value::from(load.get("sourceUris").and_then(|u| u.as_array()).map(|a| a.len()).unwrap_or(0).to_string()))]));
            t.rows.extend(rows);
            Ok(())
        } else if let Some(extract) = cfg.get("extract") {
            let key = table_key(&extract.get("sourceTable").cloned().unwrap_or_default());
            let (schema, rows) = guard.ext.tables.get(&key).map(|t| (t.schema.clone(), t.rows.clone())).ok_or_else(|| ("notFound".to_string(), format!("Not found: Table {}", key)))?;
            let format = extract.get("destinationFormat").map(|f| f.to_string()).unwrap_or_else(|| "NEWLINE_DELIMITED_JSON".into());
            let mut out = String::new();
            match format.as_str() {
                "CSV" => {
                    if extract.get("printHeader").and_then(|p| p.as_bool()).unwrap_or(true) {
                        out.push_str(&schema.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>().join(","));
                        out.push('\n');
                    }
                    for r in &rows {
                        out.push_str(&schema.iter().map(|(n, _)| r.get(n).map(csv_escape).unwrap_or_default()).collect::<Vec<_>>().join(","));
                        out.push('\n');
                    }
                }
                "NEWLINE_DELIMITED_JSON" => {
                    for r in &rows {
                        out.push_str(&Value::Object(r.clone()).to_json());
                        out.push('\n');
                    }
                }
                other => return Err(("invalid".into(), format!("Destination format {} is not supported by the emulator", other))),
            }
            for uri in extract.get("destinationUris").and_then(|u| u.as_array()).cloned().unwrap_or_default() {
                let uri = uri.to_string().replace('*', "000000000000");
                let (b, o) = gs(&uri).ok_or_else(|| ("invalid".to_string(), format!("Invalid destination uri {}", uri)))?;
                let gen = super::next(&mut guard);
                let ct = if format == "CSV" { "text/csv" } else { "application/json" };
                let bucket = guard.buckets.get_mut(&b).ok_or_else(|| ("notFound".to_string(), format!("Not found: Bucket {}", b)))?;
                bucket.objects.insert(o, super::new_obj(out.clone().into_bytes(), ct.into(), gen));
            }
            stats.insert("extract".into(), obj([("destinationUriFileCounts", Value::Array(vec![Value::from("1")]))]));
            Ok(())
        } else {
            Err(("invalid".into(), "Only load and extract jobs are supported by the emulator (use jobs.query)".into()))
        }
    })();
    let mut status = ObjectMap::new();
    status.insert("state".into(), Value::from("DONE"));
    if let Err((reason, message)) = &result {
        status.insert("errorResult".into(), obj([("reason", Value::from(reason.clone())), ("message", Value::from(message.clone()))]));
    }
    let job = obj([
        ("kind", Value::from("bigquery#job")),
        ("id", Value::from(format!("{}:{}", project, job_id))),
        ("jobReference", obj([("projectId", Value::from(project)), ("jobId", Value::from(job_id.clone()))])),
        ("configuration", cfg),
        ("status", Value::Object(status)),
        ("statistics", Value::Object(stats)),
    ]);
    guard.ext.bq_jobs.insert(job_id, (job.clone(), 1));
    let mut first = job.as_object().cloned().unwrap_or_default();
    first.insert("status".into(), obj([("state", Value::from("PENDING"))]));
    Response::json(200, Value::Object(first).to_json())
}

fn query_response(ext: &ExtState, project: &str, job_id: &str, token: Option<String>) -> Response {
    let j = &ext.jobs[job_id];
    let mut m = ObjectMap::new();
    m.insert("kind".to_string(), Value::from("bigquery#queryResponse"));
    m.insert("jobReference".to_string(), obj([("projectId", Value::from(project)), ("jobId", Value::from(job_id))]));
    if j.pending_polls > 0 && token.is_none() {
        m.insert("jobComplete".into(), Value::Bool(false));
        return Response::json(200, Value::Object(m).to_json());
    }
    m.insert("jobComplete".into(), Value::Bool(true));
    if let Some(n) = j.dml {
        m.insert("numDmlAffectedRows".into(), Value::from(n.to_string()));
        return Response::json(200, Value::Object(m).to_json());
    }
    m.insert(
        "schema".into(),
        obj([("fields", Value::Array(j.schema.iter().map(|(n, t)| obj([("name", Value::from(n.clone())), ("type", Value::from(t.clone()))])).collect()))]),
    );
    m.insert("totalRows".into(), Value::from(j.rows.len().to_string()));
    let start: usize = token.and_then(|t| t.parse().ok()).unwrap_or(0);
    let size = if ext.bq_page_size == 0 { usize::MAX } else { ext.bq_page_size };
    let end = start.saturating_add(size).min(j.rows.len());
    m.insert("rows".into(), Value::Array(j.rows[start.min(end)..end].to_vec()));
    if end < j.rows.len() {
        m.insert("pageToken".into(), Value::from(end.to_string()));
    }
    Response::json(200, Value::Object(m).to_json())
}

#[derive(Clone, Debug, PartialEq)]
enum T {
    W(String),
    S(String),
    N(f64),
    P(String),
    O(String),
}

fn tok(q: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = q.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() || ch == ';' {
            i += 1;
        } else if ch == '\'' || ch == '"' || ch == '`' {
            let mut s = String::new();
            i += 1;
            while i < c.len() && c[i] != ch {
                s.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push(if ch == '`' { T::W(s) } else { T::S(s) });
        } else if ch == '@' {
            let start = i + 1;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(T::P(c[start..i].iter().collect()));
        } else if ch.is_alphabetic() || ch == '_' || ch == '*' {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.' || c[i] == '-') {
                i += 1;
            }
            out.push(T::W(c[start..i].iter().collect()));
        } else if ch.is_ascii_digit() || (ch == '-' && i + 1 < c.len() && c[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            let n: String = c[start..i].iter().collect();
            out.push(T::N(n.parse().map_err(|_| format!("bad number {}", n))?));
        } else if "=<>!".contains(ch) {
            let start = i;
            i += 1;
            while i < c.len() && "=<>".contains(c[i]) {
                i += 1;
            }
            out.push(T::O(c[start..i].iter().collect()));
        } else if "(),".contains(ch) {
            out.push(T::O(ch.to_string()));
            i += 1;
        } else {
            return Err(format!("Syntax error: Unexpected character '{}'", ch));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    t: Vec<T>,
    i: usize,
    params: &'a HashMap<String, Value>,
}

impl Parser<'_> {
    fn kw(&self, w: &str) -> bool {
        matches!(self.t.get(self.i), Some(T::W(x)) if x.eq_ignore_ascii_case(w))
    }
    fn expect_kw(&mut self, w: &str) -> Result<(), String> {
        if self.kw(w) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("Syntax error: Expected keyword {} but got {:?}", w, self.t.get(self.i)))
        }
    }
    fn word(&mut self) -> Result<String, String> {
        match self.t.get(self.i) {
            Some(T::W(w)) => {
                self.i += 1;
                Ok(w.clone())
            }
            other => Err(format!("Syntax error: Unexpected {:?}", other)),
        }
    }
    fn op(&mut self, o: &str) -> bool {
        if matches!(self.t.get(self.i), Some(T::O(x)) if x == o) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        let v = match self.t.get(self.i) {
            Some(T::S(s)) => Value::from(s.clone()),
            Some(T::N(n)) if n.fract() == 0.0 => Value::Int(*n as i64),
            Some(T::N(n)) => Value::Float(*n),
            Some(T::P(p)) => self.params.get(p).cloned().ok_or_else(|| format!("Query parameter '{}' not found", p))?,
            Some(T::W(w)) if w.eq_ignore_ascii_case("true") => Value::Bool(true),
            Some(T::W(w)) if w.eq_ignore_ascii_case("false") => Value::Bool(false),
            Some(T::W(w)) if w.eq_ignore_ascii_case("null") => Value::Null,
            other => return Err(format!("Syntax error: Expected a value but got {:?}", other)),
        };
        self.i += 1;
        Ok(v)
    }
    fn conds(&mut self) -> Result<Vec<(String, String, Value)>, String> {
        let mut out = Vec::new();
        if !self.kw("WHERE") {
            return Ok(out);
        }
        self.i += 1;
        loop {
            let col = self.word()?;
            let op = match self.t.get(self.i) {
                Some(T::O(o)) => o.clone(),
                other => return Err(format!("Syntax error: Expected an operator but got {:?}", other)),
            };
            self.i += 1;
            out.push((col, op, self.value()?));
            if self.kw("AND") {
                self.i += 1;
                continue;
            }
            return Ok(out);
        }
    }
}

fn matches_row(row: &ObjectMap, conds: &[(String, String, Value)]) -> bool {
    use std::cmp::Ordering::*;
    conds.iter().all(|(col, op, v)| {
        let x = row.get(col.rsplit('.').next().unwrap_or(col)).cloned().unwrap_or_default();
        let ord = compare_values(&x, v);
        match op.as_str() {
            "=" => values_equal(&x, v),
            "!=" | "<>" => !values_equal(&x, v),
            "<" => ord == Some(Less),
            ">" => ord == Some(Greater),
            "<=" => matches!(ord, Some(Less | Equal)),
            ">=" => matches!(ord, Some(Greater | Equal)),
            _ => false,
        }
    })
}

fn table_key(project: &str, name: &str) -> String {
    if name.matches('.').count() >= 2 {
        name.to_string()
    } else {
        format!("{}.{}", project, name)
    }
}

fn cell(v: &Value) -> Value {
    obj([("v", if v.is_null() { Value::Null } else { Value::from(v.to_string()) })])
}

fn run_sql(ext: &mut ExtState, project: &str, q: &str, params: &HashMap<String, Value>) -> Result<BqJob, String> {
    let mut p = Parser { t: tok(q)?, i: 0, params };
    let not_found = |k: &str| format!("Not found: Table {}", k);
    if p.kw("SELECT") {
        p.i += 1;
        let mut cols = Vec::new();
        let mut count = false;
        loop {
            if p.kw("COUNT") {
                p.i += 1;
                if !(p.op("(") && (p.op("*") || p.word().is_ok_and(|w| w == "*") || p.value().is_ok()) && p.op(")")) {
                    return Err("Syntax error in COUNT".into());
                }
                count = true;
            } else {
                cols.push(p.word()?);
            }
            if !p.op(",") {
                break;
            }
        }
        p.expect_kw("FROM")?;
        let key = table_key(project, &p.word()?);
        let conds = p.conds()?;
        let mut order: Option<(String, bool)> = None;
        if p.kw("ORDER") {
            p.i += 1;
            p.expect_kw("BY")?;
            let col = p.word()?;
            let desc = p.kw("DESC");
            if desc || p.kw("ASC") {
                p.i += 1;
            }
            order = Some((col, desc));
        }
        let mut limit = None;
        if p.kw("LIMIT") {
            p.i += 1;
            limit = p.value()?.as_i64().map(|l| l as usize);
        }
        if p.i < p.t.len() {
            return Err(format!("Syntax error: Unexpected {:?}", p.t[p.i]));
        }
        let t = ext.tables.get(&key).ok_or_else(|| not_found(&key))?;
        let mut rows: Vec<&ObjectMap> = t.rows.iter().filter(|r| matches_row(r, &conds)).collect();
        if let Some((col, desc)) = &order {
            rows.sort_by(|a, b| {
                let o = compare_values(a.get(col).unwrap_or(&Value::Null), b.get(col).unwrap_or(&Value::Null)).unwrap_or(std::cmp::Ordering::Equal);
                if *desc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        if let Some(l) = limit {
            rows.truncate(l);
        }
        if count {
            return Ok(BqJob { schema: vec![("f0_".into(), "INTEGER".into())], rows: vec![obj([("f", Value::Array(vec![cell(&Value::Int(rows.len() as i64))]))])], dml: None, pending_polls: 0 });
        }
        let schema: Vec<(String, String)> = if cols.iter().any(|c| c == "*") {
            t.schema.clone()
        } else {
            cols.iter().map(|c| t.schema.iter().find(|(n, _)| n == c).cloned().ok_or_else(|| format!("Unrecognized name: {}", c))).collect::<Result<_, _>>()?
        };
        let out = rows.iter().map(|r| obj([("f", Value::Array(schema.iter().map(|(n, _)| cell(r.get(n).unwrap_or(&Value::Null))).collect()))])).collect();
        return Ok(BqJob { schema, rows: out, dml: None, pending_polls: 0 });
    }
    if p.kw("DELETE") {
        p.i += 1;
        p.expect_kw("FROM")?;
        let key = table_key(project, &p.word()?);
        let conds = p.conds()?;
        if conds.is_empty() {
            return Err("DELETE must have a WHERE clause".into());
        }
        let t = ext.tables.get_mut(&key).ok_or_else(|| not_found(&key))?;
        let before = t.rows.len();
        t.rows.retain(|r| !matches_row(r, &conds));
        return Ok(BqJob { schema: vec![], rows: vec![], dml: Some((before - t.rows.len()) as i64), pending_polls: 0 });
    }
    if p.kw("UPDATE") {
        p.i += 1;
        let key = table_key(project, &p.word()?);
        p.expect_kw("SET")?;
        let mut sets = Vec::new();
        loop {
            let col = p.word()?;
            if !p.op("=") {
                return Err("Syntax error: expected =".into());
            }
            sets.push((col, p.value()?));
            if !p.op(",") {
                break;
            }
        }
        let conds = p.conds()?;
        let t = ext.tables.get_mut(&key).ok_or_else(|| not_found(&key))?;
        let mut n = 0;
        for r in t.rows.iter_mut().filter(|r| matches_row(r, &conds)) {
            for (c, v) in &sets {
                r.insert(c.clone(), v.clone());
            }
            n += 1;
        }
        return Ok(BqJob { schema: vec![], rows: vec![], dml: Some(n), pending_polls: 0 });
    }
    if p.kw("INSERT") {
        p.i += 1;
        if p.kw("INTO") {
            p.i += 1;
        }
        let key = table_key(project, &p.word()?);
        let mut cols = Vec::new();
        if !p.op("(") {
            return Err("Syntax error: expected a column list".into());
        }
        loop {
            cols.push(p.word()?);
            if !p.op(",") {
                break;
            }
        }
        p.op(")");
        p.expect_kw("VALUES")?;
        let t = ext.tables.get_mut(&key).ok_or_else(|| not_found(&key))?;
        let mut n = 0;
        while p.op("(") {
            let mut row = ObjectMap::new();
            for c in &cols {
                let v = p.value()?;
                let ty = t.schema.iter().find(|(name, _)| name == c).map(|(_, ty)| ty.clone()).ok_or_else(|| format!("Column {} is not present in table {}", c, key))?;
                row.insert(c.clone(), bq_coerce(&v, &ty)?);
                p.op(",");
            }
            p.op(")");
            p.op(",");
            t.rows.push(row);
            n += 1;
        }
        return Ok(BqJob { schema: vec![], rows: vec![], dml: Some(n), pending_polls: 0 });
    }
    Err("Syntax error: Expected SELECT, INSERT, UPDATE or DELETE".into())
}

// ---------------------------------------------------------------------------
// Secret Manager
// ---------------------------------------------------------------------------

fn secrets(st: &Shared, req: &ServerRequest, project: &str, rest: &[&str]) -> Response {
    let mut guard = st.lock().unwrap();
    let ext = &mut guard.ext;
    let prefix = format!("projects/{}/secrets/", project);
    let body = parse_json(&req.body).unwrap_or_default();
    match (req.method.as_str(), rest) {
        ("GET", []) => {
            let list: Vec<Value> = ext.secrets.keys().filter(|k| k.starts_with(&prefix)).map(|k| obj([("name", Value::from(k.clone()))])).collect();
            Response::json(200, obj([("secrets", Value::Array(list))]).to_json())
        }
        ("POST", []) => {
            let Some(id) = req.query_param("secretId").filter(|i| !i.is_empty()) else { return gerror(400, "INVALID_ARGUMENT", "secretId is required") };
            if body.get("replication").is_none() {
                return gerror(400, "INVALID_ARGUMENT", "Secret.replication is required");
            }
            let name = format!("{}{}", prefix, id);
            if ext.secrets.contains_key(&name) {
                return gerror(409, "ALREADY_EXISTS", &format!("Secret [{}] already exists.", name));
            }
            ext.secrets.insert(name.clone(), vec![]);
            Response::json(200, obj([("name", Value::from(name)), ("createTime", Value::from(iso8601_millis(now_millis())))]).to_json())
        }
        (m, [one]) => {
            let (secret, verb) = one.split_once(':').unwrap_or((one, ""));
            let name = format!("{}{}", prefix, secret);
            match (m, verb) {
                ("POST", "addVersion") => {
                    let Some(v) = ext.secrets.get_mut(&name) else { return gerror(404, "NOT_FOUND", &format!("Secret [{}] not found.", name)) };
                    let Some(data) = body.get("payload").and_then(|p| p.get("data")).and_then(|d| base64_decode(&d.to_string())) else { return gerror(400, "INVALID_ARGUMENT", "payload.data is required") };
                    v.push(data);
                    Response::json(200, obj([("name", Value::from(format!("{}/versions/{}", name, v.len()))), ("state", Value::from("ENABLED"))]).to_json())
                }
                ("GET", "") => match ext.secrets.get(&name) {
                    Some(_) => Response::json(200, obj([("name", Value::from(name))]).to_json()),
                    None => gerror(404, "NOT_FOUND", &format!("Secret [{}] not found.", name)),
                },
                ("DELETE", "") => match ext.secrets.remove(&name) {
                    Some(_) => Response::json(200, "{}"),
                    None => gerror(404, "NOT_FOUND", &format!("Secret [{}] not found.", name)),
                },
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Secret Manager request"),
            }
        }
        ("GET", [secret, "versions", v]) => {
            let (version, verb) = v.split_once(':').unwrap_or((v, ""));
            let name = format!("{}{}", prefix, secret);
            let Some(versions) = ext.secrets.get(&name) else { return gerror(404, "NOT_FOUND", &format!("Secret [{}] not found or has no versions.", name)) };
            let idx = if version == "latest" { versions.len() } else { version.parse().unwrap_or(0) };
            let Some(data) = idx.checked_sub(1).and_then(|i| versions.get(i)) else { return gerror(404, "NOT_FOUND", &format!("Secret Version [{}/versions/{}] not found.", name, version)) };
            if verb != "access" {
                return Response::json(200, obj([("name", Value::from(format!("{}/versions/{}", name, idx))), ("state", Value::from("ENABLED"))]).to_json());
            }
            Response::json(200, obj([("name", Value::from(format!("{}/versions/{}", name, idx))), ("payload", obj([("data", Value::from(crate::util::base64_encode(data)))]))]).to_json())
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Secret Manager request"),
    }
}

// ---------------------------------------------------------------------------
// Cloud Functions
// ---------------------------------------------------------------------------

/// Source archive uploads (`PUT /fn-uploads/{id}`, signed upload URLs) and downloads.
pub(super) fn function_sources(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let id = req.path.strip_prefix("/fn-uploads/")?.split('?').next().unwrap_or("").to_string();
    let mut s = st.lock().unwrap();
    Some(match req.method.as_str() {
        "PUT" => {
            if req.header("Content-Type") != Some("application/zip") || req.header("x-goog-content-length-range").is_none() {
                return Some(gerror(400, "INVALID_ARGUMENT", "Uploads require Content-Type: application/zip and x-goog-content-length-range"));
            }
            match s.ext.fn_uploads.get_mut(&id) {
                Some(slot) => {
                    *slot = req.body.clone();
                    Response::new(200)
                }
                None => gerror(403, "PERMISSION_DENIED", "Invalid upload URL"),
            }
        }
        "GET" => match s.ext.fn_uploads.get(&id) {
            Some(d) => Response::new(200).with_header("Content-Type", "application/zip").with_body(d.clone()),
            None => gerror(404, "NOT_FOUND", "No such archive"),
        },
        _ => gerror(405, "METHOD_NOT_ALLOWED", "Method not allowed"),
    })
}

/// Long-running operations of Cloud Functions (`GET /v1/operations/{id}`).
pub(super) fn function_operation(st: &Shared, id: &str) -> Response {
    let mut s = st.lock().unwrap();
    match s.ext.fn_ops.get_mut(&format!("operations/{}", id)) {
        Some((resp, polls)) => {
            if *polls > 0 {
                *polls -= 1;
                return Response::json(200, obj([("name", Value::from(format!("operations/{}", id))), ("done", Value::Bool(false))]).to_json());
            }
            Response::json(200, obj([("name", Value::from(format!("operations/{}", id))), ("done", Value::Bool(true)), ("response", resp.clone())]).to_json())
        }
        None => gerror(404, "NOT_FOUND", "Operation not found"),
    }
}

fn functions(st: &Shared, req: &ServerRequest, project: &str, location: &str, rest: &[&str]) -> Response {
    let prefix = format!("projects/{}/locations/{}/functions", project, location);
    let meta = st.lock().unwrap().ext.fn_meta.clone();
    let resource = |name: &str| meta.get(name).cloned().unwrap_or_else(|| obj([("name", Value::from(name)), ("status", Value::from("ACTIVE")), ("entryPoint", Value::from(name.rsplit('/').next().unwrap_or(""))), ("runtime", Value::from("rust-camel-mock"))]));
    let host = req.header("Host").unwrap_or("localhost").to_string();
    let new_op = |s: &mut super::State, resource: Value| {
        let name = format!("operations/{}", random_hex(10));
        s.ext.fn_ops.insert(name.clone(), (resource, 1));
        Response::json(200, obj([("name", Value::from(name)), ("done", Value::Bool(false))]).to_json())
    };
    let body = parse_json(&req.body).unwrap_or_default();
    let validate = |s: &super::State, f: &Value| -> Option<Response> {
        if f.get("entryPoint").is_none_or(|e| e.to_string().is_empty()) || f.get("runtime").is_none_or(|r| r.to_string().is_empty()) {
            return Some(gerror(400, "INVALID_ARGUMENT", "entryPoint and runtime are required"));
        }
        match (f.get("sourceUploadUrl").map(|u| u.to_string()), f.get("sourceArchiveUrl").map(|u| u.to_string())) {
            (Some(u), _) => {
                let id = u.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string();
                if s.ext.fn_uploads.get(&id).is_none_or(|d| d.is_empty()) {
                    return Some(gerror(400, "INVALID_ARGUMENT", "The source upload URL has no uploaded archive"));
                }
            }
            (None, Some(a)) if a.starts_with("gs://") => {}
            _ => return Some(gerror(400, "INVALID_ARGUMENT", "A source (sourceUploadUrl or a gs:// sourceArchiveUrl) is required")),
        }
        None
    };
    match (req.method.as_str(), rest) {
        ("GET", []) => {
            let s = st.lock().unwrap();
            let list: Vec<Value> = s.ext.functions.keys().filter(|k| k.starts_with(&prefix)).map(|k| resource(k)).collect();
            Response::json(200, obj([("functions", Value::Array(list))]).to_json())
        }
        ("POST", []) => {
            let mut s = st.lock().unwrap();
            let name = body.get("name").map(|n| n.to_string()).unwrap_or_default();
            if !name.starts_with(&format!("{}/", prefix)) {
                return gerror(400, "INVALID_ARGUMENT", "The function name must be projects/*/locations/*/functions/*");
            }
            if s.ext.functions.contains_key(&name) {
                return gerror(409, "ALREADY_EXISTS", &format!("Function {} already exists", name));
            }
            if let Some(r) = validate(&s, &body) {
                return r;
            }
            let mut m = body.as_object().cloned().unwrap_or_default();
            m.insert("status".into(), Value::from("ACTIVE"));
            m.insert("versionId".into(), Value::from("1"));
            if m.contains_key("httpsTrigger") {
                m.insert("httpsTrigger".into(), obj([("url", Value::from(format!("https://{}-{}.cloudfunctions.net/{}", location, project, name.rsplit('/').next().unwrap_or(""))))]));
            }
            let res = Value::Object(m);
            s.ext.fn_meta.insert(name.clone(), res.clone());
            // a deployed function echoes its call data
            s.ext.functions.insert(name, std::sync::Arc::new(|d: &str| Ok(d.to_string())));
            new_op(&mut s, res)
        }
        ("POST", [":generateUploadUrl"]) => {
            let mut s = st.lock().unwrap();
            let id = random_hex(12);
            s.ext.fn_uploads.insert(id.clone(), Vec::new());
            Response::json(200, obj([("uploadUrl", Value::from(format!("http://{}/fn-uploads/{}?GoogleAccessId=mock&Signature=sig", host, id)))]).to_json())
        }
        (m, [one]) => {
            let (fname, verb) = one.split_once(':').unwrap_or((one, ""));
            let name = format!("{}/{}", prefix, fname);
            let f = st.lock().unwrap().ext.functions.get(&name).cloned();
            let Some(f) = f else { return gerror(404, "NOT_FOUND", &format!("Function {} in region {} in project {} does not exist", fname, location, project)) };
            match (m, verb) {
                ("POST", "call") => {
                    let data = parse_json(&req.body).ok().and_then(|b| b.get("data").map(|d| d.to_string())).unwrap_or_default();
                    let id = random_hex(6);
                    match f(&data) {
                        Ok(r) => Response::json(200, obj([("executionId", Value::from(id)), ("result", Value::from(r))]).to_json()),
                        Err(e) => Response::json(200, obj([("executionId", Value::from(id)), ("error", Value::from(e))]).to_json()),
                    }
                }
                ("GET", "") => Response::json(200, resource(&name).to_json()),
                ("PATCH", "") => {
                    let mut s = st.lock().unwrap();
                    let mut cur = resource(&name).as_object().cloned().unwrap_or_default();
                    let mask = req.query_param("updateMask").unwrap_or_default();
                    for (k, v) in body.as_object().cloned().unwrap_or_default() {
                        if mask.is_empty() || mask.split(',').any(|f| f.trim() == k) {
                            cur.insert(k, v);
                        }
                    }
                    if let Some(r) = validate(&s, &Value::Object(cur.clone())) {
                        return r;
                    }
                    let v = cur.get("versionId").and_then(|v| v.to_string().parse::<i64>().ok()).unwrap_or(1) + 1;
                    cur.insert("versionId".into(), Value::from(v.to_string()));
                    let res = Value::Object(cur);
                    s.ext.fn_meta.insert(name.clone(), res.clone());
                    new_op(&mut s, res)
                }
                ("POST", "generateDownloadUrl") => {
                    let s = st.lock().unwrap();
                    let url = s.ext.fn_meta.get(&name).and_then(|m| m.get("sourceUploadUrl").map(|u| u.to_string()));
                    match url {
                        Some(u) => Response::json(200, obj([("downloadUrl", Value::from(u))]).to_json()),
                        None => gerror(400, "FAILED_PRECONDITION", "The function has no uploaded source"),
                    }
                }
                ("DELETE", "") => {
                    st.lock().unwrap().ext.functions.remove(&name);
                    Response::json(200, obj([("name", Value::from(format!("operations/{}", random_hex(8)))), ("done", Value::Bool(true))]).to_json())
                }
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Cloud Functions request"),
            }
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Cloud Functions request"),
    }
}

// ---------------------------------------------------------------------------
// Firestore
// ---------------------------------------------------------------------------

/// One Firestore write: (document name, Some(fields, mask) for an update / None for a delete,
/// `currentDocument.exists` precondition).
type FsWrite = (String, Option<(ObjectMap, Option<Vec<String>>)>, Option<bool>);

fn parse_write(w: &Value) -> Result<FsWrite, String> {
    let exists = w.get("currentDocument").and_then(|c| c.get("exists")).and_then(|e| e.as_bool());
    if let Some(d) = w.get("delete") {
        return Ok((d.to_string(), None, exists));
    }
    let u = w.get("update").ok_or("A write needs update or delete")?;
    let name = u.get("name").map(|n| n.to_string()).ok_or("A write needs a document name")?;
    let fields = document_fields(u).as_object().cloned().unwrap_or_default();
    let mask = w.get("updateMask").and_then(|m| m.get("fieldPaths")).and_then(|f| f.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect());
    Ok((name, Some((fields, mask)), exists))
}

fn check_write(docs: &BTreeMap<String, (ObjectMap, i64, i64)>, w: &FsWrite) -> Result<(), (u16, &'static str, String)> {
    match (w.2, docs.contains_key(&w.0)) {
        (Some(true), false) => Err((404, "NOT_FOUND", format!("No document to update: {}", w.0))),
        (Some(false), true) => Err((409, "ALREADY_EXISTS", format!("Document already exists: {}", w.0))),
        _ => Ok(()),
    }
}

fn apply_write(docs: &mut BTreeMap<String, (ObjectMap, i64, i64)>, w: FsWrite, now: i64) {
    match w.1 {
        None => {
            docs.remove(&w.0);
        }
        Some((fields, mask)) => {
            let e = docs.entry(w.0).or_insert_with(|| (ObjectMap::new(), now, now));
            match mask {
                None => e.0 = fields,
                Some(m) => {
                    for k in m {
                        match fields.get(&k) {
                            Some(v) => {
                                e.0.insert(k, v.clone());
                            }
                            None => {
                                e.0.remove(&k);
                            }
                        }
                    }
                }
            }
            e.2 = now;
        }
    }
}

/// beginTransaction / commit (atomic, transactional reads checked) / batchWrite (independent
/// writes, per-write status) / rollback.
fn firestore_writes(ext: &mut ExtState, verb: &str, body: &Value, now: i64) -> Response {
    let ts = Value::from(iso8601_millis(now));
    match verb {
        "beginTransaction" => {
            let read_only = body.get("options").and_then(|o| o.get("readOnly")).is_some();
            let id = crate::util::base64_encode(random_hex(12).as_bytes());
            ext.fs_tx.insert(id.clone(), (read_only, BTreeMap::new()));
            Response::json(200, obj([("transaction", Value::from(id))]).to_json())
        }
        "rollback" => {
            let tx = body.get("transaction").map(|t| t.to_string()).unwrap_or_default();
            match ext.fs_tx.remove(&tx) {
                Some(_) => Response::json(200, "{}"),
                None => gerror(400, "INVALID_ARGUMENT", "Invalid transaction."),
            }
        }
        "commit" => {
            let writes: Vec<FsWrite> = match body.get("writes").and_then(|w| w.as_array()).cloned().unwrap_or_default().iter().map(parse_write).collect::<Result<_, _>>() {
                Ok(w) => w,
                Err(e) => return gerror(400, "INVALID_ARGUMENT", &e),
            };
            if let Some(tx) = body.get("transaction").map(|t| t.to_string()) {
                let Some((read_only, reads)) = ext.fs_tx.remove(&tx) else { return gerror(400, "INVALID_ARGUMENT", "Invalid transaction.") };
                if read_only && !writes.is_empty() {
                    return gerror(400, "INVALID_ARGUMENT", "Cannot modify entities in a read-only transaction.");
                }
                // optimistic concurrency: documents read in the transaction must be unchanged
                if reads.iter().any(|(n, t)| ext.docs.get(n).map(|d| d.2).unwrap_or(0) != *t) {
                    ext.fs_aborted += 1;
                    return gerror(409, "ABORTED", "Transaction aborted: a document read in the transaction was modified concurrently (too much contention).");
                }
            }
            for w in &writes {
                if let Err((code, status, msg)) = check_write(&ext.docs, w) {
                    return gerror(code, status, &msg);
                }
            }
            let n = writes.len();
            for w in writes {
                apply_write(&mut ext.docs, w, now);
            }
            Response::json(200, obj([("writeResults", Value::Array(vec![obj([("updateTime", ts.clone())]); n])), ("commitTime", ts)]).to_json())
        }
        _ => {
            let mut results = Vec::new();
            let mut status = Vec::new();
            for w in body.get("writes").and_then(|w| w.as_array()).cloned().unwrap_or_default() {
                match parse_write(&w).map_err(|e| (400u16, "INVALID_ARGUMENT", e)).and_then(|w| check_write(&ext.docs, &w).map(|_| w)) {
                    Ok(w) => {
                        apply_write(&mut ext.docs, w, now);
                        results.push(obj([("updateTime", ts.clone())]));
                        status.push(obj([("code", Value::Int(0))]));
                    }
                    Err((_, st, msg)) => {
                        results.push(obj([]));
                        status.push(obj([("code", Value::Int(if st == "NOT_FOUND" { 5 } else if st == "ALREADY_EXISTS" { 6 } else { 3 })), ("message", Value::from(msg))]));
                    }
                }
            }
            Response::json(200, obj([("writeResults", Value::Array(results)), ("status", Value::Array(status))]).to_json())
        }
    }
}

fn doc_resource(name: &str, d: &(ObjectMap, i64, i64)) -> Value {
    obj([("name", Value::from(name)), ("fields", fields_of(&d.0)), ("createTime", Value::from(iso8601_millis(d.1))), ("updateTime", Value::from(iso8601_millis(d.2)))])
}

fn ffilter(doc: &ObjectMap, f: &Value) -> bool {
    use std::cmp::Ordering::*;
    if let Some(c) = f.get("compositeFilter") {
        let subs = c.get("filters").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        return match c.get("op").map(|o| o.to_string()).as_deref() {
            Some("OR") => subs.iter().any(|s| ffilter(doc, s)),
            _ => subs.iter().all(|s| ffilter(doc, s)),
        };
    }
    let Some(ff) = f.get("fieldFilter") else { return true };
    let path = ff.get("field").and_then(|x| x.get("fieldPath")).map(|p| p.to_string()).unwrap_or_default();
    let mut v = Some(Value::Object(doc.clone()));
    for seg in path.split('.') {
        v = v.and_then(|x| x.get(seg).cloned());
    }
    let Some(v) = v else { return false };
    let want = from_firestore(&ff.get("value").cloned().unwrap_or_default());
    let ord = compare_values(&v, &want);
    match ff.get("op").map(|o| o.to_string()).unwrap_or_default().as_str() {
        "EQUAL" => values_equal(&v, &want),
        "NOT_EQUAL" => !values_equal(&v, &want),
        "LESS_THAN" => ord == Some(Less),
        "LESS_THAN_OR_EQUAL" => matches!(ord, Some(Less | Equal)),
        "GREATER_THAN" => ord == Some(Greater),
        "GREATER_THAN_OR_EQUAL" => matches!(ord, Some(Greater | Equal)),
        "ARRAY_CONTAINS" => v.as_array().is_some_and(|a| a.iter().any(|x| values_equal(x, &want))),
        _ => false,
    }
}

fn firestore(st: &Shared, req: &ServerRequest, project: &str, db: &str, rest: &[&str]) -> Response {
    let mut guard = st.lock().unwrap();
    let ext = &mut guard.ext;
    let root = format!("projects/{}/databases/{}/documents", project, db);
    let body = parse_json(&req.body).unwrap_or_default();
    // `documents[/path...][:runQuery]`
    let Some(first) = rest.first() else { return gerror(404, "NOT_FOUND", "Unknown Firestore resource") };
    let mut segs: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
    let mut verb = String::new();
    if let Some(last) = segs.last_mut() {
        if let Some((a, v)) = last.clone().split_once(':') {
            *last = a.to_string();
            verb = v.to_string();
        }
    }
    if !first.starts_with("documents") {
        return gerror(404, "NOT_FOUND", "Unknown Firestore resource");
    }
    let path = segs[1..].join("/");
    let full = if path.is_empty() { root.clone() } else { format!("{}/{}", root, path) };
    let now = now_millis();
    if path.is_empty() && matches!(verb.as_str(), "beginTransaction" | "commit" | "batchWrite" | "rollback") {
        return firestore_writes(ext, &verb, &body, now);
    }
    if let (Some(tx), "GET") = (req.query_param("transaction"), req.method.as_str()) {
        let Some((_, reads)) = ext.fs_tx.get_mut(&tx) else { return gerror(400, "INVALID_ARGUMENT", "Invalid transaction.") };
        let t = ext.docs.get(&full).map(|d| d.2).unwrap_or(0);
        reads.insert(full.clone(), t);
    }
    if verb == "runQuery" {
        let Some(sq) = body.get("structuredQuery") else { return gerror(400, "INVALID_ARGUMENT", "structuredQuery is required") };
        let coll = sq.get("from").and_then(|f| f.as_array()).and_then(|a| a.first()).and_then(|f| f.get("collectionId")).map(|c| c.to_string()).unwrap_or_default();
        let parent = format!("{}/{}/", full, coll);
        let mut docs: Vec<(&String, &(ObjectMap, i64, i64))> =
            ext.docs.iter().filter(|(n, d)| n.starts_with(&parent) && !n[parent.len()..].contains('/') && sq.get("where").is_none_or(|w| ffilter(&d.0, w))).collect();
        if let Some(o) = sq.get("orderBy").and_then(|o| o.as_array()).and_then(|a| a.first()) {
            let field = o.get("field").and_then(|f| f.get("fieldPath")).map(|p| p.to_string()).unwrap_or_default();
            let desc = o.get("direction").map(|d| d.to_string()).as_deref() == Some("DESCENDING");
            docs.sort_by(|a, b| {
                let x = compare_values(a.1 .0.get(&field).unwrap_or(&Value::Null), b.1 .0.get(&field).unwrap_or(&Value::Null)).unwrap_or(std::cmp::Ordering::Equal);
                if desc {
                    x.reverse()
                } else {
                    x
                }
            });
        }
        if let Some(l) = sq.get("limit").and_then(|l| l.as_i64()) {
            docs.truncate(l.max(0) as usize);
        }
        let read = Value::from(iso8601_millis(now));
        let mut out: Vec<Value> = docs.iter().map(|(n, d)| obj([("document", doc_resource(n, d)), ("readTime", read.clone())])).collect();
        if out.is_empty() {
            out.push(obj([("readTime", read)]));
        }
        return Response::json(200, Value::Array(out).to_json());
    }
    let is_collection = segs.len() % 2 == 0;
    match (req.method.as_str(), is_collection) {
        ("GET", true) => {
            let prefix = format!("{}/", full);
            let list: Vec<Value> = ext.docs.iter().filter(|(n, _)| n.starts_with(&prefix) && !n[prefix.len()..].contains('/')).map(|(n, d)| doc_resource(n, d)).collect();
            Response::json(200, obj([("documents", Value::Array(list))]).to_json())
        }
        ("POST", true) => {
            let id = req.query_param("documentId").filter(|i| !i.is_empty()).unwrap_or_else(|| random_hex(10));
            let name = format!("{}/{}", full, id);
            if ext.docs.contains_key(&name) {
                return gerror(409, "ALREADY_EXISTS", &format!("Document already exists: {}", name));
            }
            let fields = document_fields(&body).as_object().cloned().unwrap_or_default();
            ext.docs.insert(name.clone(), (fields, now, now));
            Response::json(200, doc_resource(&name, &ext.docs[&name]).to_json())
        }
        ("GET", false) => match ext.docs.get(&full) {
            Some(d) => Response::json(200, doc_resource(&full, d).to_json()),
            None => gerror(404, "NOT_FOUND", &format!("Document \"{}\" not found.", full)),
        },
        ("PATCH", false) => {
            let exists = ext.docs.contains_key(&full);
            if req.query_param("currentDocument.exists").as_deref() == Some("true") && !exists {
                return gerror(404, "NOT_FOUND", &format!("No document to update: {}", full));
            }
            let fields = document_fields(&body).as_object().cloned().unwrap_or_default();
            let mask: Vec<String> = req.query_params().into_iter().filter(|(k, _)| k == "updateMask.fieldPaths").map(|(_, v)| v).collect();
            let entry = ext.docs.entry(full.clone()).or_insert_with(|| (ObjectMap::new(), now, now));
            if mask.is_empty() {
                entry.0 = fields;
            } else {
                for m in mask {
                    match fields.get(&m) {
                        Some(v) => {
                            entry.0.insert(m, v.clone());
                        }
                        None => {
                            entry.0.remove(&m);
                        }
                    }
                }
            }
            entry.2 = now;
            Response::json(200, doc_resource(&full, &ext.docs[&full]).to_json())
        }
        ("DELETE", false) => {
            ext.docs.remove(&full);
            Response::json(200, "{}")
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Firestore request"),
    }
}

// ---------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------

/// Parse `Sheet1!A1:B2` → (sheet, (col0, row0), Option<(col1, row1)>), zero-based.
fn parse_a1(range: &str) -> (Option<String>, (usize, usize), Option<(usize, usize)>) {
    let (sheet, cells) = match range.rsplit_once('!') {
        Some((s, c)) => (Some(s.trim_matches('\'').to_string()), c.to_string()),
        None if !range.chars().any(|c| c.is_ascii_digit()) && !range.contains(':') && range.chars().any(|c| c.is_lowercase()) => (Some(range.to_string()), String::new()),
        None => (None, range.to_string()),
    };
    let cell = |c: &str| -> Option<(usize, usize)> {
        let letters: String = c.chars().take_while(|x| x.is_ascii_alphabetic()).collect();
        let digits: String = c.chars().skip(letters.len()).collect();
        let col = letters.to_ascii_uppercase().chars().fold(0usize, |a, ch| a * 26 + (ch as usize - 'A' as usize + 1));
        let row = digits.parse::<usize>().ok().unwrap_or(1);
        Some((col.max(1) - 1, row.max(1) - 1))
    };
    if cells.is_empty() {
        return (sheet, (0, 0), None);
    }
    match cells.split_once(':') {
        Some((a, b)) => (sheet, cell(a).unwrap_or((0, 0)), cell(b)),
        None => (sheet, cell(&cells).unwrap_or((0, 0)), None),
    }
}

fn col_name(mut c: usize) -> String {
    let mut s = String::new();
    c += 1;
    while c > 0 {
        let r = (c - 1) % 26;
        s.insert(0, (b'A' + r as u8) as char);
        c = (c - 1) / 26;
    }
    s
}

fn sheets(st: &Shared, req: &ServerRequest, rest: &[&str]) -> Response {
    let mut guard = st.lock().unwrap();
    let tok = req.header("Authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("").to_string();
    if guard.require_auth && !guard.ext.token_scopes.get(&tok).is_some_and(|s| s.split(' ').any(|x| x.ends_with("/auth/spreadsheets") || x.ends_with("/auth/drive"))) {
        guard.auth_failures += 1;
        return gerror(403, "PERMISSION_DENIED", "Request had insufficient authentication scopes.");
    }
    let ext = &mut guard.ext;
    let body = parse_json(&req.body).unwrap_or_default();
    match (req.method.as_str(), rest) {
        ("POST", []) => {
            let title = body.get("properties").and_then(|p| p.get("title")).map(|t| t.to_string()).unwrap_or_else(|| "Untitled spreadsheet".into());
            let id = format!("1{}", random_hex(10));
            ext.sheets.insert(id.clone(), Spreadsheet { title: title.clone(), sheets: vec![("Sheet1".into(), vec![])] });
            Response::json(200, obj([("spreadsheetId", Value::from(id)), ("properties", obj([("title", Value::from(title))]))]).to_json())
        }
        (m, [id, tail @ ..]) => {
            let Some(sp) = ext.sheets.get_mut(*id) else { return gerror(404, "NOT_FOUND", "Requested entity was not found.") };
            match (m, tail) {
                ("GET", []) => {
                    let sheets: Vec<Value> = sp.sheets.iter().enumerate().map(|(i, (n, _))| obj([("properties", obj([("sheetId", Value::Int(i as i64)), ("title", Value::from(n.clone()))]))])).collect();
                    Response::json(200, obj([("spreadsheetId", Value::from(*id)), ("properties", obj([("title", Value::from(sp.title.clone()))])), ("sheets", Value::Array(sheets))]).to_json())
                }
                ("GET", ["values:batchGet"]) => {
                    let ranges: Vec<String> = req.query_params().into_iter().filter(|(k, _)| k == "ranges").map(|(_, v)| v).collect();
                    let vrs: Vec<Value> = ranges.iter().map(|r| obj([("range", Value::from(r.clone())), ("values", read_range(sp, r))])).collect();
                    Response::json(200, obj([("spreadsheetId", Value::from(*id)), ("valueRanges", Value::Array(vrs))]).to_json())
                }
                (m, ["values", r]) => {
                    let (range, verb) = match r.rsplit_once(':') {
                        Some((a, v)) if v == "append" || v == "clear" => (a.to_string(), v.to_string()),
                        _ => (r.to_string(), String::new()),
                    };
                    let (sheet_name, start, end) = parse_a1(&range);
                    let sheet_name = sheet_name.unwrap_or_else(|| sp.sheets[0].0.clone());
                    let Some(grid) = sp.sheets.iter_mut().find(|(n, _)| *n == sheet_name).map(|(_, g)| g) else { return gerror(400, "INVALID_ARGUMENT", &format!("Unable to parse range: {}", range)) };
                    let values: Vec<Vec<Value>> = body.get("values").and_then(|v| v.as_array()).map(|rows| rows.iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect()).unwrap_or_default();
                    let raw = req.query_param("valueInputOption").as_deref() == Some("RAW");
                    let entered = |v: &Value| if raw { v.clone() } else { v.as_str().and_then(|s| s.parse::<i64>().ok().map(Value::Int)).unwrap_or_else(|| v.clone()) };
                    match (m, verb.as_str()) {
                        ("GET", "") => Response::json(200, obj([("range", Value::from(range.clone())), ("majorDimension", Value::from("ROWS")), ("values", read_range(sp, &range))]).to_json()),
                        ("PUT", "") | ("POST", "append") => {
                            if req.query_param("valueInputOption").is_none() {
                                return gerror(400, "INVALID_ARGUMENT", "'valueInputOption' is required but not specified");
                            }
                            let start_row = if verb == "append" { grid.iter().rposition(|r| r.iter().any(|c| !c.is_null() && !c.to_string().is_empty())).map(|i| i + 1).unwrap_or(start.1).max(start.1) } else { start.1 };
                            for (i, row) in values.iter().enumerate() {
                                let ri = start_row + i;
                                while grid.len() <= ri {
                                    grid.push(Vec::new());
                                }
                                for (j, v) in row.iter().enumerate() {
                                    let cj = start.0 + j;
                                    while grid[ri].len() <= cj {
                                        grid[ri].push(Value::Null);
                                    }
                                    grid[ri][cj] = entered(v);
                                }
                            }
                            let cols = values.iter().map(|r| r.len()).max().unwrap_or(0);
                            let cells: usize = values.iter().map(|r| r.len()).sum();
                            let updated = format!("{}!{}{}:{}{}", sheet_name, col_name(start.0), start_row + 1, col_name(start.0 + cols.max(1) - 1), start_row + values.len().max(1));
                            let upd = obj([
                                ("spreadsheetId", Value::from(*id)),
                                ("updatedRange", Value::from(updated)),
                                ("updatedRows", Value::Int(values.len() as i64)),
                                ("updatedColumns", Value::Int(cols as i64)),
                                ("updatedCells", Value::Int(cells as i64)),
                            ]);
                            if verb == "append" {
                                Response::json(200, obj([("spreadsheetId", Value::from(*id)), ("tableRange", Value::from(range.clone())), ("updates", upd)]).to_json())
                            } else {
                                Response::json(200, upd.to_json())
                            }
                        }
                        ("POST", "clear") => {
                            let (r1, c1) = end.map(|(c, r)| (r, c)).unwrap_or((usize::MAX, usize::MAX));
                            for (ri, row) in grid.iter_mut().enumerate().filter(|(ri, _)| *ri >= start.1 && *ri <= r1) {
                                let _ = ri;
                                for (cj, c) in row.iter_mut().enumerate() {
                                    if cj >= start.0 && cj <= c1 {
                                        *c = Value::Null;
                                    }
                                }
                            }
                            Response::json(200, obj([("spreadsheetId", Value::from(*id)), ("clearedRange", Value::from(range.clone()))]).to_json())
                        }
                        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported values request"),
                    }
                }
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Sheets request"),
            }
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Sheets request"),
    }
}

fn read_range(sp: &Spreadsheet, range: &str) -> Value {
    let (sheet_name, start, end) = parse_a1(range);
    let sheet_name = sheet_name.unwrap_or_else(|| sp.sheets[0].0.clone());
    let Some((_, grid)) = sp.sheets.iter().find(|(n, _)| *n == sheet_name) else { return Value::Array(vec![]) };
    let (c1, r1) = end.unwrap_or((usize::MAX, usize::MAX));
    let mut rows: Vec<Value> = Vec::new();
    for ri in start.1..grid.len().min(r1.saturating_add(1)) {
        let row = &grid[ri];
        let mut cells: Vec<Value> = (start.0..row.len().min(c1.saturating_add(1))).map(|cj| Value::from(if row[cj].is_null() { String::new() } else { row[cj].to_string() })).collect();
        while cells.last().is_some_and(|c| c.to_string().is_empty()) {
            cells.pop();
        }
        rows.push(Value::Array(cells));
    }
    while rows.last().is_some_and(|r| r.as_array().is_some_and(|a| a.is_empty())) {
        rows.pop();
    }
    Value::Array(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_notation() {
        assert_eq!(parse_a1("Sheet1!A1:B2"), (Some("Sheet1".into()), (0, 0), Some((1, 1))));
        assert_eq!(parse_a1("C3"), (None, (2, 2), None));
        assert_eq!(parse_a1("'My Sheet'!AA10"), (Some("My Sheet".into()), (26, 9), None));
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(27), "AB");
    }
}
