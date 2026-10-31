//! Further emulated AWS services on the [`AwsMock`] port (SigV4 verified by the parent
//! handler): MSK (`kafka` REST), Athena, Textract, Translate, Comprehend, Polly, Rekognition,
//! Transcribe, Bedrock Runtime / Agent Runtime (`bedrock` REST), Amazon MQ, ECS, EKS, KMS,
//! Timestream (write + query), CloudTrail, Config and the Redshift Data API.
//!
//! The "intelligent" services are deterministic stand-ins: Textract returns one `LINE` block
//! per line of the (UTF-8) document, Translate prefixes the text with `[lang]` (or uses the
//! phrases registered with [`AwsMock::add_translation`]), Comprehend uses keyword rules,
//! Polly returns `MOCKAUDIO:{voice}:{format}:{text}`, Rekognition derives labels from words in
//! the image bytes, Bedrock echoes the last user message, the knowledge bases rank documents by
//! word overlap, and Athena / Redshift return the results registered for a query (one column
//! `result` = `1` otherwise).

use std::collections::{BTreeMap, BTreeSet};

use camel_core::value::{ObjectMap, Value};

use super::{json_error, request_id, AwsMock, Shared, ACCOUNT};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, now_millis, now_secs, obj, parse_json, percent_decode, random_hex, uuid};

struct AthenaQuery {
    sql: String,
    database: String,
    work_group: String,
    output: String,
    state: String,
    reason: String,
    polls_left: usize,
    rows: Vec<Vec<String>>,
}

struct TranscribeJob {
    status: String,
    language: String,
    media: String,
    gets: usize,
}

struct KmsKey {
    arn: String,
    description: String,
    state: String,
    material: Vec<u8>,
    created: i64,
}

struct RsStatement {
    sql: String,
    database: String,
    status: String,
    error: String,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    has_result: bool,
    created: i64,
}

/// State of these services.
#[derive(Default)]
pub(super) struct MoreState {
    msk: BTreeMap<String, Value>,
    msk_describes: BTreeMap<String, usize>,
    athena: BTreeMap<String, AthenaQuery>,
    athena_results: Vec<(String, Vec<Vec<String>>)>,
    athena_fail_next: usize,
    athena_polls: usize,
    translations: BTreeMap<(String, String), String>,
    lexicons: BTreeMap<String, String>,
    synthesis_tasks: BTreeMap<String, Value>,
    transcribe: BTreeMap<String, TranscribeJob>,
    knowledge_bases: BTreeMap<String, Vec<String>>,
    mq: BTreeMap<String, Value>,
    ecs: BTreeMap<String, Value>,
    eks: BTreeMap<String, Value>,
    kms: BTreeMap<String, KmsKey>,
    ts_dbs: BTreeMap<String, i64>,
    ts_tables: BTreeMap<(String, String), Vec<Value>>,
    ts_page_size: usize,
    trail: Vec<Value>,
    config_rules: BTreeMap<String, Value>,
    compliance: BTreeMap<String, String>,
    packs: BTreeMap<String, String>,
    rs_tables: BTreeMap<String, (Vec<String>, Vec<Vec<Value>>)>,
    rs_statements: BTreeMap<String, RsStatement>,
    bedrock_requests: Vec<(String, Value)>,
}

impl AwsMock {
    // ---- Athena
    /// Rows (first row = header) returned by queries containing `sql_fragment`.
    pub fn set_athena_result(&self, sql_fragment: &str, rows: Vec<Vec<&str>>) {
        self.lock().more.athena_results.push((sql_fragment.into(), rows.into_iter().map(|r| r.into_iter().map(|c| c.to_string()).collect()).collect()));
    }
    /// The next `n` queries fail with `GENERIC_INTERNAL_ERROR`.
    pub fn athena_fail_next(&self, n: usize) {
        self.lock().more.athena_fail_next = n;
    }
    /// `GetQueryExecution` calls a query stays `RUNNING` before completing (default 1).
    pub fn set_athena_polls(&self, n: usize) {
        self.lock().more.athena_polls = n;
    }
    /// Number of queries started.
    pub fn athena_query_count(&self) -> usize {
        self.lock().more.athena.len()
    }

    // ---- Translate / Bedrock / KB
    /// Register a translation of `text` into `target`.
    pub fn add_translation(&self, text: &str, target: &str, translated: &str) {
        self.lock().more.translations.insert((text.into(), target.into()), translated.into());
    }
    /// Documents of a Bedrock knowledge base.
    pub fn add_knowledge_base(&self, id: &str, docs: &[&str]) {
        self.lock().more.knowledge_bases.insert(id.into(), docs.iter().map(|d| d.to_string()).collect());
    }
    /// (model id, request JSON) of the Bedrock runtime calls served.
    pub fn bedrock_requests(&self) -> Vec<(String, Value)> {
        self.lock().more.bedrock_requests.clone()
    }

    // ---- Timestream / CloudTrail / Config / Redshift
    /// Records of a Timestream table.
    pub fn timestream_records(&self, db: &str, table: &str) -> Vec<Value> {
        self.lock().more.ts_tables.get(&(db.to_string(), table.to_string())).cloned().unwrap_or_default()
    }
    /// Rows per Timestream query page (default 1000).
    pub fn set_timestream_page_size(&self, n: usize) {
        self.lock().more.ts_page_size = n;
    }
    /// Record a CloudTrail event (event time = now); returns its id.
    pub fn add_cloudtrail_event(&self, source: &str, name: &str, user: &str) -> String {
        let id = uuid();
        let t = now_millis() as f64 / 1000.0;
        let doc = obj([("eventVersion", Value::from("1.08")), ("eventSource", Value::from(source)), ("eventName", Value::from(name)), ("userIdentity", obj([("userName", Value::from(user))]))]);
        let ev = obj([
            ("EventId", Value::from(id.clone())),
            ("EventName", Value::from(name)),
            ("EventSource", Value::from(source)),
            ("Username", Value::from(user)),
            ("EventTime", Value::Float(t)),
            ("CloudTrailEvent", Value::from(doc.to_json())),
        ]);
        self.lock().more.trail.push(ev);
        id
    }
    /// Names of the AWS Config rules.
    pub fn config_rule_names(&self) -> Vec<String> {
        self.lock().more.config_rules.keys().cloned().collect()
    }
    /// Set the compliance type of a Config rule (default `COMPLIANT`).
    pub fn set_config_compliance(&self, rule: &str, compliance: &str) {
        self.lock().more.compliance.insert(rule.into(), compliance.into());
    }
    /// A Redshift table served by `SELECT * FROM name`.
    pub fn add_redshift_table(&self, name: &str, columns: &[&str], rows: Vec<Vec<Value>>) {
        self.lock().more.rs_tables.insert(name.to_ascii_lowercase(), (columns.iter().map(|c| c.to_string()).collect(), rows));
    }
    /// KMS key ids.
    pub fn kms_keys(&self) -> Vec<String> {
        self.lock().more.kms.keys().cloned().collect()
    }
}

/// Dispatch (`None` = not one of these services).
pub(super) fn handle(st: &Shared, service: &str, req: &ServerRequest) -> Option<Response> {
    Some(match service {
        "kafka" => msk(st, req),
        "athena" => athena(st, req),
        "textract" => textract(st, req),
        "translate" => translate(st, req),
        "comprehend" => comprehend(req),
        "polly" => polly(st, req),
        "rekognition" => rekognition(st, req),
        "transcribe" => transcribe(st, req),
        "bedrock" => bedrock(st, req),
        "mq" => mq(st, req),
        "ecs" => ecs(st, req),
        "eks" => eks(st, req),
        "kms" => kms(st, req),
        "timestream" => timestream(st, req),
        "cloudtrail" => cloudtrail(st, req),
        "config" => config(st, req),
        "redshift-data" => redshift(st, req),
        _ => return None,
    })
}

fn ok(v: Value) -> Response {
    Response::json(200, v.to_json()).with_header("x-amzn-RequestId", request_id())
}

fn target(req: &ServerRequest) -> String {
    req.header("X-Amz-Target").and_then(|t| t.rsplit_once('.')).map(|(_, a)| a.to_string()).unwrap_or_default()
}

fn s_(v: &Value, k: &str) -> String {
    v.get(k).map(|x| x.to_string()).unwrap_or_default()
}

fn body(req: &ServerRequest) -> Value {
    parse_json(&req.body).unwrap_or_default()
}

/// REST-JSON error (`x-amzn-ErrorType`).
fn rest_err(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("message", Value::from(msg))]).to_json()).with_header("x-amzn-ErrorType", format!("{}:", code)).with_header("x-amzn-RequestId", request_id())
}

fn jerr(prefix: &str, code: &str, msg: &str) -> Response {
    json_error(400, prefix, code, msg)
}

fn segs(req: &ServerRequest) -> Vec<String> {
    req.path.split('/').filter(|s| !s.is_empty()).map(|s| percent_decode(s, false)).collect()
}

/// Page a list: (items of the page, next token).
fn page(items: Vec<Value>, token: Option<&str>, max: Option<i64>, default_max: usize) -> (Vec<Value>, Option<String>) {
    let start: usize = token.and_then(|t| t.parse().ok()).unwrap_or(0);
    let max = max.filter(|m| *m > 0).map(|m| m as usize).unwrap_or(default_max);
    let end = (start + max).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    (items.get(start..end).map(|s| s.to_vec()).unwrap_or_default(), next)
}

fn with_token(mut m: ObjectMap, field: &str, token: Option<String>) -> Value {
    if let Some(t) = token {
        m.insert(field.into(), Value::from(t));
    }
    Value::Object(m)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '\'').filter(|w| !w.is_empty()).map(|w| w.trim_end_matches('.').to_string()).collect()
}

// ---------------------------------------------------------------------------
// MSK
// ---------------------------------------------------------------------------

fn msk(st: &Shared, req: &ServerRequest) -> Response {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["v1", "clusters"]) => {
            let b = body(req);
            let name = s_(&b, "clusterName");
            if name.is_empty() || b.get("kafkaVersion").is_none() || b.get("numberOfBrokerNodes").is_none() {
                return rest_err(400, "BadRequestException", "clusterName, kafkaVersion and numberOfBrokerNodes are required");
            }
            if m.msk.values().any(|c| s_(c, "clusterName") == name) {
                return rest_err(409, "ConflictException", &format!("A cluster with name {} already exists", name));
            }
            let arn = format!("arn:aws:kafka:us-east-1:{}:cluster/{}/{}", ACCOUNT, name, uuid());
            let info = obj([
                ("clusterArn", Value::from(arn.clone())),
                ("clusterName", Value::from(name.clone())),
                ("state", Value::from("CREATING")),
                ("currentBrokerSoftwareInfo", obj([("kafkaVersion", b.get("kafkaVersion").cloned().unwrap_or_default())])),
                ("numberOfBrokerNodes", b.get("numberOfBrokerNodes").cloned().unwrap_or_default()),
                ("brokerNodeGroupInfo", b.get("brokerNodeGroupInfo").cloned().unwrap_or_default()),
            ]);
            m.msk.insert(arn.clone(), info);
            ok(obj([("clusterArn", Value::from(arn)), ("clusterName", Value::from(name)), ("state", Value::from("CREATING"))]))
        }
        ("GET", ["v1", "clusters"]) => {
            let filter = req.query_param("clusterNameFilter").unwrap_or_default();
            let items: Vec<Value> = m.msk.values().filter(|c| s_(c, "clusterName").starts_with(&filter)).cloned().collect();
            let (items, next) = page(items, req.query_param("nextToken").as_deref(), req.query_param("maxResults").and_then(|x| x.parse().ok()), 2);
            let mut out = ObjectMap::new();
            out.insert("clusterInfoList".into(), Value::Array(items));
            ok(with_token(out, "nextToken", next))
        }
        ("GET", ["v1", "clusters", arn]) => {
            let n = m.msk_describes.entry(arn.to_string()).or_insert(0);
            *n += 1;
            match m.msk.get_mut(*arn) {
                Some(c) => {
                    if let Value::Object(o) = c {
                        if o.get("state").map(|v| v.to_string()).as_deref() == Some("CREATING") {
                            o.insert("state".into(), Value::from("ACTIVE"));
                        }
                    }
                    ok(obj([("clusterInfo", c.clone())]))
                }
                None => rest_err(404, "NotFoundException", "The specified cluster was not found"),
            }
        }
        ("GET", ["v1", "clusters", arn, "bootstrap-brokers"]) => match m.msk.get(*arn) {
            Some(c) => {
                let name = s_(c, "clusterName");
                ok(obj([
                    ("bootstrapBrokerStringTls", Value::from(format!("b-1.{}.kafka.us-east-1.amazonaws.com:9094", name))),
                    ("bootstrapBrokerStringSaslIam", Value::from(format!("b-1.{}.kafka.us-east-1.amazonaws.com:9098", name))),
                ]))
            }
            None => rest_err(404, "NotFoundException", "The specified cluster was not found"),
        },
        ("DELETE", ["v1", "clusters", arn]) => match m.msk.remove(*arn) {
            Some(_) => ok(obj([("clusterArn", Value::from(arn.to_string())), ("state", Value::from("DELETING"))])),
            None => rest_err(404, "NotFoundException", "The specified cluster was not found"),
        },
        _ => rest_err(400, "BadRequestException", "Unsupported MSK request"),
    }
}

// ---------------------------------------------------------------------------
// Athena
// ---------------------------------------------------------------------------

const ATHENA: &str = "com.amazonaws.athena";

fn athena(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match target(req).as_str() {
        "StartQueryExecution" => {
            let sql = s_(&b, "QueryString");
            if sql.trim().is_empty() {
                return jerr(ATHENA, "InvalidRequestException", "QueryString is required");
            }
            let id = uuid();
            let output = b.get("ResultConfiguration").and_then(|r| r.get("OutputLocation")).map(|o| o.to_string()).unwrap_or_else(|| "s3://aws-athena-query-results/".into());
            let rows = m
                .athena_results
                .iter()
                .rev()
                .find(|(frag, _)| sql.contains(frag.as_str()))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| vec![vec!["result".to_string()], vec!["1".to_string()]]);
            let fail = m.athena_fail_next > 0;
            if fail {
                m.athena_fail_next -= 1;
            }
            m.athena.insert(
                id.clone(),
                AthenaQuery {
                    sql,
                    database: b.get("QueryExecutionContext").and_then(|c| c.get("Database")).map(|d| d.to_string()).unwrap_or_default(),
                    work_group: b.get("WorkGroup").map(|w| w.to_string()).unwrap_or_else(|| "primary".into()),
                    output: format!("{}/{}.csv", output.trim_end_matches('/'), id),
                    state: if fail { "FAILED_PENDING".into() } else { "QUEUED".into() },
                    reason: String::new(),
                    polls_left: m.athena_polls.max(1),
                    rows,
                },
            );
            ok(obj([("QueryExecutionId", Value::from(id))]))
        }
        "GetQueryExecution" => {
            let id = s_(&b, "QueryExecutionId");
            let Some(q) = m.athena.get_mut(&id) else { return jerr(ATHENA, "InvalidRequestException", &format!("QUERY_NOT_FOUND: Query {} was not found", id)) };
            if q.polls_left > 0 {
                q.polls_left -= 1;
                if q.state == "QUEUED" {
                    q.state = "RUNNING".into();
                }
            } else if q.state == "RUNNING" || q.state == "QUEUED" {
                q.state = "SUCCEEDED".into();
            } else if q.state == "FAILED_PENDING" {
                q.state = "FAILED".into();
                q.reason = "GENERIC_INTERNAL_ERROR: simulated failure".into();
            }
            let shown = if q.state == "FAILED_PENDING" { "RUNNING" } else { q.state.as_str() };
            let mut status = ObjectMap::new();
            status.insert("State".into(), Value::from(shown));
            if !q.reason.is_empty() {
                status.insert("StateChangeReason".into(), Value::from(q.reason.clone()));
            }
            ok(obj([(
                "QueryExecution",
                obj([
                    ("QueryExecutionId", Value::from(id.clone())),
                    ("Query", Value::from(q.sql.clone())),
                    ("QueryExecutionContext", obj([("Database", Value::from(q.database.clone()))])),
                    ("ResultConfiguration", obj([("OutputLocation", Value::from(q.output.clone()))])),
                    ("WorkGroup", Value::from(q.work_group.clone())),
                    ("Status", Value::Object(status)),
                ]),
            )]))
        }
        "GetQueryResults" => {
            let id = s_(&b, "QueryExecutionId");
            let Some(q) = m.athena.get(&id) else { return jerr(ATHENA, "InvalidRequestException", "QUERY_NOT_FOUND") };
            if q.state != "SUCCEEDED" {
                return jerr(ATHENA, "InvalidRequestException", &format!("Query has not yet finished. Current state: {}", q.state));
            }
            let rows: Vec<Value> = q.rows.iter().map(|r| obj([("Data", Value::Array(r.iter().map(|c| obj([("VarCharValue", Value::from(c.clone()))])).collect()))])).collect();
            let (rows, next) = page(rows, b.get("NextToken").map(|t| t.to_string()).as_deref(), b.get("MaxResults").and_then(|x| x.as_i64()), 1000);
            let cols: Vec<Value> = q.rows.first().map(|h| h.iter().map(|c| obj([("Name", Value::from(c.clone())), ("Type", Value::from("varchar"))])).collect()).unwrap_or_default();
            let mut out = ObjectMap::new();
            out.insert("ResultSet".into(), obj([("Rows", Value::Array(rows)), ("ResultSetMetadata", obj([("ColumnInfo", Value::Array(cols))]))]));
            ok(with_token(out, "NextToken", next))
        }
        "ListQueryExecutions" => {
            let wg = b.get("WorkGroup").map(|w| w.to_string());
            let ids: Vec<Value> = m.athena.iter().filter(|(_, q)| wg.as_ref().is_none_or(|w| *w == q.work_group)).map(|(id, _)| Value::from(id.clone())).collect();
            let (ids, next) = page(ids, b.get("NextToken").map(|t| t.to_string()).as_deref(), b.get("MaxResults").and_then(|x| x.as_i64()), 50);
            let mut out = ObjectMap::new();
            out.insert("QueryExecutionIds".into(), Value::Array(ids));
            ok(with_token(out, "NextToken", next))
        }
        other => jerr(ATHENA, "InvalidRequestException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Textract / Rekognition (document & image content)
// ---------------------------------------------------------------------------

/// Document bytes from `{Bytes}` or `{S3Object: {Bucket, Name}}`.
fn document_bytes(st: &Shared, doc: &Value) -> Result<Vec<u8>, String> {
    if let Some(b) = doc.get("Bytes") {
        return base64_decode(&b.to_string()).ok_or_else(|| "invalid base64".to_string());
    }
    let s3 = doc.get("S3Object").ok_or("Document must have Bytes or S3Object")?;
    let (bucket, name) = (s_(s3, "Bucket"), s_(s3, "Name"));
    st.0.lock().unwrap().buckets.get(&bucket).and_then(|b| b.objects.get(&name)).map(|o| o.data.clone()).ok_or_else(|| format!("Unable to get object metadata from S3: s3://{}/{}", bucket, name))
}

fn text_blocks(text: &str, forms: bool) -> Vec<Value> {
    let mut blocks = vec![obj([("BlockType", Value::from("PAGE")), ("Id", Value::from(uuid())), ("Page", Value::Int(1))])];
    for line in text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        blocks.push(obj([("BlockType", Value::from("LINE")), ("Id", Value::from(uuid())), ("Text", Value::from(line)), ("Confidence", Value::Float(99.5)), ("Page", Value::Int(1))]));
        for w in line.split_whitespace() {
            blocks.push(obj([("BlockType", Value::from("WORD")), ("Id", Value::from(uuid())), ("Text", Value::from(w)), ("Confidence", Value::Float(99.0)), ("Page", Value::Int(1))]));
        }
        if forms {
            if let Some((k, v)) = line.split_once(':') {
                blocks.push(obj([("BlockType", Value::from("KEY_VALUE_SET")), ("Id", Value::from(uuid())), ("EntityTypes", Value::from(vec!["KEY"])), ("Text", Value::from(k.trim()))]));
                blocks.push(obj([("BlockType", Value::from("KEY_VALUE_SET")), ("Id", Value::from(uuid())), ("EntityTypes", Value::from(vec!["VALUE"])), ("Text", Value::from(v.trim()))]));
            }
        }
    }
    blocks
}

const TEXTRACT: &str = "com.amazonaws.textract";

fn textract(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let action = target(req);
    let meta = obj([("Pages", Value::Int(1))]);
    match action.as_str() {
        "DetectDocumentText" | "AnalyzeDocument" | "AnalyzeExpense" => {
            let data = match document_bytes(st, &b.get("Document").cloned().unwrap_or_default()) {
                Ok(d) => d,
                Err(e) => return jerr(TEXTRACT, "InvalidS3ObjectException", &e),
            };
            let text = String::from_utf8_lossy(&data).into_owned();
            if action == "AnalyzeExpense" {
                let fields: Vec<Value> = text.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| obj([("Type", obj([("Text", Value::from(k.trim().to_ascii_uppercase()))])), ("ValueDetection", obj([("Text", Value::from(v.trim()))]))])).collect();
                return ok(obj([("DocumentMetadata", meta), ("ExpenseDocuments", Value::Array(vec![obj([("ExpenseIndex", Value::Int(1)), ("SummaryFields", Value::Array(fields))])]))]));
            }
            if action == "AnalyzeDocument" && b.get("FeatureTypes").and_then(|f| f.as_array()).is_none_or(|f| f.is_empty()) {
                return jerr(TEXTRACT, "InvalidParameterException", "FeatureTypes is required");
            }
            ok(obj([("DocumentMetadata", meta), ("Blocks", Value::Array(text_blocks(&text, action == "AnalyzeDocument")))]))
        }
        "StartDocumentTextDetection" | "StartDocumentAnalysis" | "StartExpenseAnalysis" => {
            let doc = b.get("DocumentLocation").cloned().unwrap_or_default();
            if doc.get("S3Object").is_none() {
                return jerr(TEXTRACT, "InvalidParameterException", "DocumentLocation.S3Object is required");
            }
            match document_bytes(st, &doc) {
                Ok(d) => {
                    let id = random_hex(16);
                    st.0.lock().unwrap().more.synthesis_tasks.insert(format!("textract:{}", id), Value::from(String::from_utf8_lossy(&d).into_owned()));
                    ok(obj([("JobId", Value::from(id))]))
                }
                Err(e) => jerr(TEXTRACT, "InvalidS3ObjectException", &e),
            }
        }
        "GetDocumentTextDetection" | "GetDocumentAnalysis" | "GetExpenseAnalysis" => {
            let id = s_(&b, "JobId");
            let text = st.0.lock().unwrap().more.synthesis_tasks.get(&format!("textract:{}", id)).map(|t| t.to_string());
            match text {
                Some(t) => ok(obj([("JobStatus", Value::from("SUCCEEDED")), ("DocumentMetadata", meta), ("Blocks", Value::Array(text_blocks(&t, action == "GetDocumentAnalysis")))])),
                None => jerr(TEXTRACT, "InvalidJobIdException", "Job id not found"),
            }
        }
        other => jerr(TEXTRACT, "InvalidParameterException", &format!("Unsupported action {}", other)),
    }
}

const REKOGNITION: &str = "com.amazonaws.rekognition";

fn rekognition(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let data = match document_bytes(st, &b.get("Image").cloned().unwrap_or_default()) {
        Ok(d) => d,
        Err(e) => return jerr(REKOGNITION, "InvalidS3ObjectException", &e),
    };
    let text = String::from_utf8_lossy(&data).to_ascii_lowercase();
    match target(req).as_str() {
        "DetectLabels" => {
            let min = b.get("MinConfidence").and_then(|m| m.as_f64()).unwrap_or(55.0);
            let max = b.get("MaxLabels").and_then(|m| m.as_i64()).unwrap_or(1000) as usize;
            let mut labels: Vec<Value> = Vec::new();
            for (i, w) in words(&text).into_iter().filter(|w| w.len() > 2).collect::<BTreeSet<_>>().into_iter().enumerate() {
                let conf = 99.0 - i as f64 * 10.0;
                if conf >= min {
                    let name = format!("{}{}", w[..1].to_ascii_uppercase(), &w[1..]);
                    labels.push(obj([("Name", Value::from(name)), ("Confidence", Value::Float(conf)), ("Instances", Value::Array(vec![]))]));
                }
            }
            labels.truncate(max);
            ok(obj([("Labels", Value::Array(labels)), ("LabelModelVersion", Value::from("3.0"))]))
        }
        "DetectText" => {
            let dets: Vec<Value> = String::from_utf8_lossy(&data).lines().enumerate().map(|(i, l)| obj([("DetectedText", Value::from(l.trim())), ("Type", Value::from("LINE")), ("Id", Value::Int(i as i64)), ("Confidence", Value::Float(98.0))])).collect();
            ok(obj([("TextDetections", Value::Array(dets))]))
        }
        "DetectFaces" => {
            let faces: Vec<Value> = (0..text.matches("face").count()).map(|_| obj([("Confidence", Value::Float(99.9)), ("BoundingBox", obj([("Width", Value::Float(0.2)), ("Height", Value::Float(0.3)), ("Left", Value::Float(0.1)), ("Top", Value::Float(0.1))]))])).collect();
            ok(obj([("FaceDetails", Value::Array(faces))]))
        }
        "DetectModerationLabels" => ok(obj([("ModerationLabels", Value::Array(vec![]))])),
        "RecognizeCelebrities" => ok(obj([("CelebrityFaces", Value::Array(vec![])), ("UnrecognizedFaces", Value::Array(vec![]))])),
        other => jerr(REKOGNITION, "InvalidParameterException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Translate / Comprehend / Polly / Transcribe
// ---------------------------------------------------------------------------

fn detect_language(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    if ["ciao", "grazie", "buongiorno"].iter().any(|w| t.contains(w)) {
        "it"
    } else if ["bonjour", "merci"].iter().any(|w| t.contains(w)) {
        "fr"
    } else if ["hola", "gracias"].iter().any(|w| t.contains(w)) {
        "es"
    } else {
        "en"
    }
}

const TRANSLATE: &str = "com.amazonaws.translate";

fn translate(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    if target(req) != "TranslateText" {
        return jerr(TRANSLATE, "InvalidRequestException", "Unsupported action");
    }
    let (text, src, tgt) = (s_(&b, "Text"), s_(&b, "SourceLanguageCode"), s_(&b, "TargetLanguageCode"));
    if text.is_empty() || src.is_empty() || tgt.is_empty() {
        return jerr(TRANSLATE, "InvalidRequestException", "Text, SourceLanguageCode and TargetLanguageCode are required");
    }
    if tgt.len() > 5 || tgt == "xx" {
        return jerr(TRANSLATE, "UnsupportedLanguagePairException", &format!("Unsupported language pair: {} to {}", src, tgt));
    }
    let src = if src == "auto" { detect_language(&text).to_string() } else { src };
    let out = st.0.lock().unwrap().more.translations.get(&(text.clone(), tgt.clone())).cloned().unwrap_or_else(|| format!("[{}] {}", tgt, text));
    ok(obj([("TranslatedText", Value::from(out)), ("SourceLanguageCode", Value::from(src)), ("TargetLanguageCode", Value::from(tgt))]))
}

const COMPREHEND: &str = "com.amazonaws.comprehend";

fn comprehend(req: &ServerRequest) -> Response {
    let b = body(req);
    let action = target(req);
    let text = if action == "DetectToxicContent" {
        b.get("TextSegments").and_then(|t| t.as_array()).map(|a| a.iter().map(|x| s_(x, "Text")).collect::<Vec<_>>().join(" ")).unwrap_or_default()
    } else {
        s_(&b, "Text")
    };
    if text.is_empty() {
        return jerr(COMPREHEND, "InvalidRequestException", "Text is required");
    }
    if action != "DetectDominantLanguage" && action != "ClassifyDocument" && s_(&b, "LanguageCode").is_empty() {
        return jerr(COMPREHEND, "InvalidRequestException", "LanguageCode is required");
    }
    let lower = text.to_ascii_lowercase();
    let ws = words(&text);
    match action.as_str() {
        "DetectDominantLanguage" => ok(obj([("Languages", Value::Array(vec![obj([("LanguageCode", Value::from(detect_language(&text))), ("Score", Value::Float(0.99))])]))])),
        "DetectSentiment" => {
            let pos = ["love", "great", "good", "excellent", "happy"].iter().filter(|w| lower.contains(*w)).count();
            let neg = ["hate", "bad", "terrible", "awful", "sad"].iter().filter(|w| lower.contains(*w)).count();
            let s = match pos.cmp(&neg) {
                std::cmp::Ordering::Greater => "POSITIVE",
                std::cmp::Ordering::Less => "NEGATIVE",
                _ if pos > 0 => "MIXED",
                _ => "NEUTRAL",
            };
            let score = |k: &str| Value::Float(if k == s { 0.95 } else { 0.0166 });
            ok(obj([("Sentiment", Value::from(s)), ("SentimentScore", obj([("Positive", score("POSITIVE")), ("Negative", score("NEGATIVE")), ("Neutral", score("NEUTRAL")), ("Mixed", score("MIXED"))]))]))
        }
        "DetectEntities" => {
            let ents: Vec<Value> = ws
                .iter()
                .filter(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
                .map(|w| obj([("Text", Value::from(w.clone())), ("Type", Value::from(if w.chars().all(|c| c.is_uppercase()) { "ORGANIZATION" } else { "PERSON" })), ("Score", Value::Float(0.9))]))
                .collect();
            ok(obj([("Entities", Value::Array(ents))]))
        }
        "DetectKeyPhrases" => ok(obj([("KeyPhrases", Value::Array(ws.iter().filter(|w| w.len() > 4).map(|w| obj([("Text", Value::from(w.clone())), ("Score", Value::Float(0.9))])).collect()))])),
        "DetectSyntax" => ok(obj([("SyntaxTokens", Value::Array(ws.iter().enumerate().map(|(i, w)| obj([("TokenId", Value::Int(i as i64 + 1)), ("Text", Value::from(w.clone())), ("PartOfSpeech", obj([("Tag", Value::from("NOUN"))]))])).collect()))])),
        "DetectPiiEntities" => ok(obj([("Entities", Value::Array(ws.iter().filter(|w| w.contains('@')).map(|w| obj([("Type", Value::from("EMAIL")), ("BeginOffset", Value::Int(text.find(w.as_str()).unwrap_or(0) as i64)), ("Score", Value::Float(0.99))])).collect()))])),
        "ContainsPiiEntities" => ok(obj([("Labels", Value::Array(if ws.iter().any(|w| w.contains('@')) { vec![obj([("Name", Value::from("EMAIL")), ("Score", Value::Float(0.99))])] } else { vec![] }))])),
        "DetectToxicContent" => ok(obj([("ResultList", Value::Array(vec![obj([("Toxicity", Value::Float(if lower.contains("hate") { 0.9 } else { 0.01 })), ("Labels", Value::Array(vec![]))])]))])),
        "ClassifyDocument" => ok(obj([("Classes", Value::Array(vec![obj([("Name", Value::from(if lower.contains("invoice") { "INVOICE" } else { "OTHER" })), ("Score", Value::Float(0.9))])]))])),
        other => jerr(COMPREHEND, "InvalidRequestException", &format!("Unsupported action {}", other)),
    }
}

fn polly(st: &Shared, req: &ServerRequest) -> Response {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let b = body(req);
    let voices = [("Joanna", "en-US", "Female"), ("Matthew", "en-US", "Male"), ("Bianca", "it-IT", "Female")];
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["v1", "speech"]) => {
            let (text, voice, fmt) = (s_(&b, "Text"), s_(&b, "VoiceId"), s_(&b, "OutputFormat"));
            if !voices.iter().any(|(v, _, _)| *v == voice) {
                return rest_err(400, "ValidationException", &format!("1 validation error detected: Value '{}' at 'voiceId' failed to satisfy constraint", voice));
            }
            if s_(&b, "TextType") == "ssml" && !text.trim_start().starts_with("<speak") {
                return rest_err(400, "InvalidSsmlException", "Invalid SSML request");
            }
            for l in b.get("LexiconNames").and_then(|l| l.as_array()).cloned().unwrap_or_default() {
                if !m.lexicons.contains_key(&l.to_string()) {
                    return rest_err(404, "LexiconNotFoundException", &format!("Lexicon not found: {}", l));
                }
            }
            let ct = match fmt.as_str() {
                "ogg_vorbis" => "audio/ogg",
                "pcm" => "audio/pcm",
                "json" => "application/x-json-stream",
                _ => "audio/mpeg",
            };
            Response::new(200)
                .with_header("Content-Type", ct)
                .with_header("x-amzn-RequestCharacters", text.chars().count().to_string())
                .with_body(format!("MOCKAUDIO:{}:{}:{}", voice, fmt, text).into_bytes())
        }
        ("GET", ["v1", "voices"]) => {
            let lang = req.query_param("LanguageCode");
            let vs: Vec<Value> = voices
                .iter()
                .filter(|(_, l, _)| lang.as_ref().is_none_or(|x| x == l))
                .map(|(v, l, gnd)| obj([("Id", Value::from(*v)), ("Name", Value::from(*v)), ("LanguageCode", Value::from(*l)), ("Gender", Value::from(*gnd)), ("SupportedEngines", Value::from(vec!["standard", "neural"]))]))
                .collect();
            ok(obj([("Voices", Value::Array(vs))]))
        }
        ("GET", ["v1", "lexicons"]) => ok(obj([("Lexicons", Value::Array(m.lexicons.keys().map(|n| obj([("Name", Value::from(n.clone()))])).collect()))])),
        ("PUT", ["v1", "lexicons", name]) => {
            m.lexicons.insert(name.to_string(), s_(&b, "Content"));
            ok(obj([]))
        }
        ("GET", ["v1", "lexicons", name]) => match m.lexicons.get(*name) {
            Some(c) => ok(obj([("Lexicon", obj([("Name", Value::from(name.to_string())), ("Content", Value::from(c.clone()))]))])),
            None => rest_err(404, "LexiconNotFoundException", "Lexicon not found"),
        },
        ("DELETE", ["v1", "lexicons", name]) => match m.lexicons.remove(*name) {
            Some(_) => ok(obj([])),
            None => rest_err(404, "LexiconNotFoundException", "Lexicon not found"),
        },
        ("POST", ["v1", "synthesisTasks"]) => {
            let id = uuid();
            let bucket = s_(&b, "OutputS3BucketName");
            let task = obj([
                ("TaskId", Value::from(id.clone())),
                ("TaskStatus", Value::from("completed")),
                ("OutputUri", Value::from(format!("https://s3.us-east-1.amazonaws.com/{}/{}{}.mp3", bucket, s_(&b, "OutputS3KeyPrefix"), id))),
                ("VoiceId", b.get("VoiceId").cloned().unwrap_or_default()),
            ]);
            m.synthesis_tasks.insert(id, task.clone());
            ok(obj([("SynthesisTask", task)]))
        }
        ("GET", ["v1", "synthesisTasks", id]) => match m.synthesis_tasks.get(*id) {
            Some(t) => ok(obj([("SynthesisTask", t.clone())])),
            None => rest_err(400, "SynthesisTaskNotFoundException", "Task not found"),
        },
        _ => rest_err(400, "ValidationException", "Unsupported Polly request"),
    }
}

const TRANSCRIBE: &str = "com.amazonaws.transcribe";

fn transcribe(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let name = s_(&b, "TranscriptionJobName");
    let job_value = |name: &str, j: &TranscribeJob| {
        let mut o = ObjectMap::new();
        o.insert("TranscriptionJobName".into(), Value::from(name));
        o.insert("TranscriptionJobStatus".into(), Value::from(j.status.clone()));
        o.insert("LanguageCode".into(), Value::from(j.language.clone()));
        o.insert("Media".into(), obj([("MediaFileUri", Value::from(j.media.clone()))]));
        if j.status == "COMPLETED" {
            o.insert("Transcript".into(), obj([("TranscriptFileUri", Value::from(format!("https://s3.amazonaws.com/transcripts/{}.json", name)))]));
        }
        Value::Object(o)
    };
    match target(req).as_str() {
        "StartTranscriptionJob" => {
            if m.transcribe.contains_key(&name) {
                return jerr(TRANSCRIBE, "ConflictException", "The requested job name already exists. Use a different job name.");
            }
            let media = b.get("Media").map(|x| s_(x, "MediaFileUri")).unwrap_or_default();
            if name.is_empty() || media.is_empty() {
                return jerr(TRANSCRIBE, "BadRequestException", "TranscriptionJobName and Media are required");
            }
            let lang = if b.get("IdentifyLanguage").is_some_and(|v| v.is_truthy()) { "en-US".to_string() } else { s_(&b, "LanguageCode") };
            let j = TranscribeJob { status: "IN_PROGRESS".into(), language: lang, media, gets: 0 };
            let v = job_value(&name, &j);
            m.transcribe.insert(name, j);
            ok(obj([("TranscriptionJob", v)]))
        }
        "GetTranscriptionJob" => match m.transcribe.get_mut(&name) {
            Some(j) => {
                j.gets += 1;
                if j.gets > 1 {
                    j.status = "COMPLETED".into();
                }
                ok(obj([("TranscriptionJob", job_value(&name, j))]))
            }
            None => jerr(TRANSCRIBE, "BadRequestException", "The requested job couldn't be found. Check the job name and try your request again."),
        },
        "DeleteTranscriptionJob" => {
            m.transcribe.remove(&name);
            ok(obj([]))
        }
        "ListTranscriptionJobs" => {
            let status = b.get("Status").map(|x| x.to_string());
            let contains = b.get("JobNameContains").map(|x| x.to_string()).unwrap_or_default();
            let items: Vec<Value> = m
                .transcribe
                .iter()
                .filter(|(n, j)| n.contains(&contains) && status.as_ref().is_none_or(|s| *s == j.status))
                .map(|(n, j)| obj([("TranscriptionJobName", Value::from(n.clone())), ("TranscriptionJobStatus", Value::from(j.status.clone())), ("LanguageCode", Value::from(j.language.clone()))]))
                .collect();
            let (items, next) = page(items, b.get("NextToken").map(|t| t.to_string()).as_deref(), b.get("MaxResults").and_then(|x| x.as_i64()), 2);
            let mut out = ObjectMap::new();
            out.insert("TranscriptionJobSummaries".into(), Value::Array(items));
            ok(with_token(out, "NextToken", next))
        }
        other => jerr(TRANSCRIBE, "BadRequestException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Bedrock
// ---------------------------------------------------------------------------

fn last_user_text(messages: &Value) -> String {
    messages
        .as_array()
        .and_then(|a| a.iter().rev().find(|m| s_(m, "role") == "user"))
        .map(|m| match m.get("content") {
            Some(Value::Array(blocks)) => blocks.iter().filter_map(|b| b.get("text").map(|t| t.to_string()).or_else(|| (!matches!(b, Value::Object(_))).then(|| b.to_string()))).collect::<Vec<_>>().join(""),
            Some(other) => other.to_string(),
            None => String::new(),
        })
        .unwrap_or_default()
}

fn overlap(query: &str, doc: &str) -> usize {
    let q: BTreeSet<String> = words(&query.to_ascii_lowercase()).into_iter().collect();
    words(&doc.to_ascii_lowercase()).into_iter().filter(|w| q.contains(w)).count()
}

fn bedrock(st: &Shared, req: &ServerRequest) -> Response {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["model", model, op]) => {
            if model.contains("missing") {
                return rest_err(404, "ResourceNotFoundException", &format!("Could not resolve the foundation model from the provided model identifier: {}", model));
            }
            m.bedrock_requests.push((model.to_string(), b.clone()));
            match *op {
                "converse" => {
                    let msgs = b.get("messages").cloned().unwrap_or_default();
                    if msgs.as_array().is_none_or(|a| a.is_empty()) {
                        return rest_err(400, "ValidationException", "A conversation must start with a user message.");
                    }
                    let text = last_user_text(&msgs);
                    let system = b.get("system").and_then(|s| s.as_array()).and_then(|a| a.first().cloned()).map(|x| s_(&x, "text")).filter(|x| !x.is_empty());
                    let reply = match system {
                        Some(sys) => format!("[{}] Echo: {}", sys, text),
                        None => format!("Echo: {}", text),
                    };
                    let max = b.get("inferenceConfig").and_then(|c| c.get("maxTokens")).and_then(|x| x.as_i64());
                    let (reply, stop) = match max {
                        Some(n) if (n as usize) < reply.split_whitespace().count() => (reply.split_whitespace().take(n as usize).collect::<Vec<_>>().join(" "), "max_tokens"),
                        _ => (reply, "end_turn"),
                    };
                    let (inp, out) = (text.split_whitespace().count() as i64, reply.split_whitespace().count() as i64);
                    ok(obj([
                        ("output", obj([("message", obj([("role", Value::from("assistant")), ("content", Value::Array(vec![obj([("text", Value::from(reply))])]))]))])),
                        ("stopReason", Value::from(stop)),
                        ("usage", obj([("inputTokens", Value::Int(inp)), ("outputTokens", Value::Int(out)), ("totalTokens", Value::Int(inp + out))])),
                        ("metrics", obj([("latencyMs", Value::Int(1))])),
                    ]))
                }
                "invoke" => {
                    let resp = if model.contains("embed") {
                        let text = s_(&b, "inputText");
                        let v: Vec<Value> = (0..8).map(|i| Value::Float(((text.len() + i * 7) % 13) as f64 / 13.0)).collect();
                        obj([("embedding", Value::Array(v)), ("inputTextTokenCount", Value::Int(text.split_whitespace().count() as i64))])
                    } else if model.contains("stability") || model.contains("image") {
                        obj([("artifacts", Value::Array(vec![obj([("base64", Value::from(base64_encode(b"MOCKPNG"))), ("finishReason", Value::from("SUCCESS"))])]))])
                    } else if model.starts_with("anthropic") {
                        obj([("type", Value::from("message")), ("role", Value::from("assistant")), ("content", Value::Array(vec![obj([("type", Value::from("text")), ("text", Value::from(format!("Echo: {}", last_user_text(&b.get("messages").cloned().unwrap_or_default()))))])])), ("stop_reason", Value::from("end_turn"))])
                    } else {
                        obj([("results", Value::Array(vec![obj([("outputText", Value::from(format!("Echo: {}", s_(&b, "inputText")))), ("completionReason", Value::from("FINISH"))])]))])
                    };
                    ok(resp).with_header("Content-Type", "application/json")
                }
                _ => rest_err(400, "ValidationException", "Unsupported Bedrock operation"),
            }
        }
        ("POST", ["knowledgebases", kb, "retrieve"]) => {
            let Some(docs) = m.knowledge_bases.get(*kb) else { return rest_err(404, "ResourceNotFoundException", &format!("Knowledge base {} not found", kb)) };
            let q = b.get("retrievalQuery").map(|x| s_(x, "text")).unwrap_or_default();
            let n = b.get("retrievalConfiguration").and_then(|c| c.get("vectorSearchConfiguration")).and_then(|v| v.get("numberOfResults")).and_then(|x| x.as_i64()).unwrap_or(5) as usize;
            let mut scored: Vec<(usize, usize, &String)> = docs.iter().enumerate().map(|(i, d)| (overlap(&q, d), i, d)).filter(|(s, _, _)| *s > 0).collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let results: Vec<Value> = scored
                .iter()
                .take(n)
                .map(|(sc, i, d)| obj([("content", obj([("text", Value::from((*d).clone()))])), ("location", obj([("type", Value::from("S3")), ("s3Location", obj([("uri", Value::from(format!("s3://kb/{}/doc{}.txt", kb, i)))]))])), ("score", Value::Float(*sc as f64 / 10.0))]))
                .collect();
            ok(obj([("retrievalResults", Value::Array(results))]))
        }
        ("POST", ["retrieveAndGenerate"]) => {
            let kb = b.get("retrieveAndGenerateConfiguration").and_then(|c| c.get("knowledgeBaseConfiguration")).map(|k| s_(k, "knowledgeBaseId")).unwrap_or_default();
            let Some(docs) = m.knowledge_bases.get(&kb) else { return rest_err(404, "ResourceNotFoundException", &format!("Knowledge base {} not found", kb)) };
            let q = b.get("input").map(|x| s_(x, "text")).unwrap_or_default();
            let best = docs.iter().max_by_key(|d| overlap(&q, d)).cloned().unwrap_or_default();
            let session = b.get("sessionId").map(|x| x.to_string()).unwrap_or_else(uuid);
            ok(obj([
                ("output", obj([("text", Value::from(format!("Based on the knowledge base: {}", best)))])),
                ("sessionId", Value::from(session)),
                ("citations", Value::Array(vec![obj([("retrievedReferences", Value::Array(vec![obj([("content", obj([("text", Value::from(best))]))])]))])])),
            ]))
        }
        _ => rest_err(400, "ValidationException", "Unsupported Bedrock request"),
    }
}

// ---------------------------------------------------------------------------
// MQ / ECS / EKS
// ---------------------------------------------------------------------------

fn mq(st: &Shared, req: &ServerRequest) -> Response {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["v1", "brokers"]) => {
            let name = s_(&b, "brokerName");
            if name.is_empty() || b.get("engineType").is_none() || b.get("hostInstanceType").is_none() {
                return rest_err(400, "BadRequestException", "brokerName, engineType and hostInstanceType are required");
            }
            if m.mq.values().any(|x| s_(x, "brokerName") == name) {
                return rest_err(409, "ConflictException", &format!("Broker {} already exists", name));
            }
            let id = format!("b-{}", uuid());
            let arn = format!("arn:aws:mq:us-east-1:{}:broker:{}:{}", ACCOUNT, name, id);
            let users: Vec<Value> = b.get("users").and_then(|u| u.as_array()).map(|a| a.iter().map(|u| obj([("username", Value::from(s_(u, "username")))])).collect()).unwrap_or_default();
            m.mq.insert(
                id.clone(),
                obj([
                    ("brokerId", Value::from(id.clone())),
                    ("brokerArn", Value::from(arn.clone())),
                    ("brokerName", Value::from(name)),
                    ("brokerState", Value::from("RUNNING")),
                    ("engineType", b.get("engineType").cloned().unwrap_or_default()),
                    ("engineVersion", b.get("engineVersion").cloned().unwrap_or_default()),
                    ("hostInstanceType", b.get("hostInstanceType").cloned().unwrap_or_default()),
                    ("deploymentMode", b.get("deploymentMode").cloned().unwrap_or_default()),
                    ("publiclyAccessible", b.get("publiclyAccessible").cloned().unwrap_or(Value::Bool(false))),
                    ("users", Value::Array(users)),
                ]),
            );
            ok(obj([("brokerArn", Value::from(arn)), ("brokerId", Value::from(id))]))
        }
        ("GET", ["v1", "brokers"]) => {
            let items: Vec<Value> = m.mq.values().map(|x| obj([("brokerId", x.get("brokerId").cloned().unwrap_or_default()), ("brokerName", x.get("brokerName").cloned().unwrap_or_default()), ("brokerState", x.get("brokerState").cloned().unwrap_or_default())])).collect();
            let (items, next) = page(items, req.query_param("nextToken").as_deref(), req.query_param("maxResults").and_then(|x| x.parse().ok()), 100);
            let mut out = ObjectMap::new();
            out.insert("brokerSummaries".into(), Value::Array(items));
            ok(with_token(out, "nextToken", next))
        }
        ("GET", ["v1", "brokers", id]) => match m.mq.get(*id) {
            Some(x) => ok(x.clone()),
            None => rest_err(404, "NotFoundException", &format!("Can't find requested broker [{}]", id)),
        },
        ("DELETE", ["v1", "brokers", id]) => match m.mq.remove(*id) {
            Some(_) => ok(obj([("brokerId", Value::from(id.to_string()))])),
            None => rest_err(404, "NotFoundException", &format!("Can't find requested broker [{}]", id)),
        },
        ("POST", ["v1", "brokers", id, "reboot"]) => match m.mq.contains_key(*id) {
            true => ok(obj([])),
            false => rest_err(404, "NotFoundException", &format!("Can't find requested broker [{}]", id)),
        },
        _ => rest_err(400, "BadRequestException", "Unsupported MQ request"),
    }
}

const ECS: &str = "com.amazonaws.ecs";

fn ecs(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let cluster = |name: &str, status: &str| obj([("clusterName", Value::from(name)), ("clusterArn", Value::from(format!("arn:aws:ecs:us-east-1:{}:cluster/{}", ACCOUNT, name))), ("status", Value::from(status)), ("runningTasksCount", Value::Int(0))]);
    match target(req).as_str() {
        "CreateCluster" => {
            let name = b.get("clusterName").map(|x| x.to_string()).unwrap_or_else(|| "default".into());
            let c = cluster(&name, "ACTIVE");
            m.ecs.insert(name, c.clone());
            ok(obj([("cluster", c)]))
        }
        "ListClusters" => {
            let arns: Vec<Value> = m.ecs.values().map(|c| c.get("clusterArn").cloned().unwrap_or_default()).collect();
            let (arns, next) = page(arns, b.get("nextToken").map(|t| t.to_string()).as_deref(), b.get("maxResults").and_then(|x| x.as_i64()), 2);
            let mut out = ObjectMap::new();
            out.insert("clusterArns".into(), Value::Array(arns));
            ok(with_token(out, "nextToken", next))
        }
        "DescribeClusters" => {
            let mut found = Vec::new();
            let mut failures = Vec::new();
            for n in b.get("clusters").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
                let n = n.to_string().rsplit('/').next().unwrap_or("").to_string();
                match m.ecs.get(&n) {
                    Some(c) => found.push(c.clone()),
                    None => failures.push(obj([("arn", Value::from(n)), ("reason", Value::from("MISSING"))])),
                }
            }
            ok(obj([("clusters", Value::Array(found)), ("failures", Value::Array(failures))]))
        }
        "DeleteCluster" => {
            let n = s_(&b, "cluster").rsplit('/').next().unwrap_or("").to_string();
            match m.ecs.remove(&n) {
                Some(_) => ok(obj([("cluster", cluster(&n, "INACTIVE"))])),
                None => jerr(ECS, "ClusterNotFoundException", "Cluster not found."),
            }
        }
        other => jerr(ECS, "InvalidParameterException", &format!("Unsupported action {}", other)),
    }
}

fn eks(st: &Shared, req: &ServerRequest) -> Response {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["clusters"]) => {
            let name = s_(&b, "name");
            if name.is_empty() || s_(&b, "roleArn").is_empty() {
                return rest_err(400, "InvalidParameterException", "name and roleArn are required");
            }
            if m.eks.contains_key(&name) {
                return rest_err(409, "ResourceInUseException", &format!("Cluster already exists with name: {}", name));
            }
            let c = obj([
                ("name", Value::from(name.clone())),
                ("arn", Value::from(format!("arn:aws:eks:us-east-1:{}:cluster/{}", ACCOUNT, name))),
                ("roleArn", b.get("roleArn").cloned().unwrap_or_default()),
                ("resourcesVpcConfig", b.get("resourcesVpcConfig").cloned().unwrap_or_default()),
                ("status", Value::from("ACTIVE")),
                ("version", Value::from("1.30")),
            ]);
            m.eks.insert(name, c.clone());
            ok(obj([("cluster", c)]))
        }
        ("GET", ["clusters"]) => {
            let names: Vec<Value> = m.eks.keys().map(|n| Value::from(n.clone())).collect();
            let (names, next) = page(names, req.query_param("nextToken").as_deref(), req.query_param("maxResults").and_then(|x| x.parse().ok()), 100);
            let mut out = ObjectMap::new();
            out.insert("clusters".into(), Value::Array(names));
            ok(with_token(out, "nextToken", next))
        }
        ("GET", ["clusters", name]) => match m.eks.get(*name) {
            Some(c) => ok(obj([("cluster", c.clone())])),
            None => rest_err(404, "ResourceNotFoundException", &format!("No cluster found for name: {}.", name)),
        },
        ("DELETE", ["clusters", name]) => match m.eks.remove(*name) {
            Some(mut c) => {
                if let Value::Object(o) = &mut c {
                    o.insert("status".into(), Value::from("DELETING"));
                }
                ok(obj([("cluster", c)]))
            }
            None => rest_err(404, "ResourceNotFoundException", &format!("No cluster found for name: {}.", name)),
        },
        _ => rest_err(400, "InvalidParameterException", "Unsupported EKS request"),
    }
}

// ---------------------------------------------------------------------------
// KMS
// ---------------------------------------------------------------------------

const KMS: &str = "com.amazonaws.kms";

fn kms_meta(id: &str, k: &KmsKey) -> Value {
    obj([
        ("KeyId", Value::from(id)),
        ("Arn", Value::from(k.arn.clone())),
        ("Description", Value::from(k.description.clone())),
        ("Enabled", Value::Bool(k.state == "Enabled")),
        ("KeyState", Value::from(k.state.clone())),
        ("KeyUsage", Value::from("ENCRYPT_DECRYPT")),
        ("CreationDate", Value::Float(k.created as f64)),
    ])
}

fn xor(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter().enumerate().map(|(i, b)| b ^ key[i % key.len()]).collect()
}

fn kms(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let key_id = s_(&b, "KeyId").rsplit('/').next().unwrap_or("").to_string();
    let not_found = || jerr(KMS, "NotFoundException", &format!("Key 'arn:aws:kms:us-east-1:{}:key/{}' does not exist", ACCOUNT, key_id));
    match target(req).as_str() {
        "CreateKey" => {
            let id = uuid();
            let k = KmsKey { arn: format!("arn:aws:kms:us-east-1:{}:key/{}", ACCOUNT, id), description: s_(&b, "Description"), state: "Enabled".into(), material: camel_tls::crypto::rng::random_bytes(32), created: now_secs() };
            let meta = kms_meta(&id, &k);
            m.kms.insert(id, k);
            ok(obj([("KeyMetadata", meta)]))
        }
        "ListKeys" => {
            let keys: Vec<Value> = m.kms.iter().map(|(id, k)| obj([("KeyId", Value::from(id.clone())), ("KeyArn", Value::from(k.arn.clone()))])).collect();
            let (keys, next) = page(keys, b.get("Marker").map(|t| t.to_string()).as_deref(), b.get("Limit").and_then(|x| x.as_i64()), 100);
            let mut out = ObjectMap::new();
            out.insert("Keys".into(), Value::Array(keys));
            out.insert("Truncated".into(), Value::Bool(next.is_some()));
            ok(with_token(out, "NextMarker", next))
        }
        "DescribeKey" => match m.kms.get(&key_id) {
            Some(k) => ok(obj([("KeyMetadata", kms_meta(&key_id, k))])),
            None => not_found(),
        },
        "EnableKey" | "DisableKey" => {
            let enable = target(req) == "EnableKey";
            match m.kms.get_mut(&key_id) {
                Some(k) if k.state == "PendingDeletion" => jerr(KMS, "KMSInvalidStateException", "Key is pending deletion"),
                Some(k) => {
                    k.state = if enable { "Enabled" } else { "Disabled" }.into();
                    ok(obj([]))
                }
                None => not_found(),
            }
        }
        "ScheduleKeyDeletion" => {
            let days = b.get("PendingWindowInDays").and_then(|d| d.as_i64()).unwrap_or(30);
            if !(7..=30).contains(&days) {
                return jerr(KMS, "ValidationException", "PendingWindowInDays must be between 7 and 30");
            }
            match m.kms.get_mut(&key_id) {
                Some(k) => {
                    k.state = "PendingDeletion".into();
                    ok(obj([("KeyId", Value::from(k.arn.clone())), ("DeletionDate", Value::Float((now_secs() + days * 86400) as f64)), ("KeyState", Value::from("PendingDeletion")), ("PendingWindowInDays", Value::Int(days))]))
                }
                None => not_found(),
            }
        }
        "Encrypt" => match m.kms.get(&key_id) {
            Some(k) if k.state != "Enabled" => jerr(KMS, "DisabledException", &format!("{} is disabled.", k.arn)),
            Some(k) => {
                let plain = base64_decode(&s_(&b, "Plaintext")).unwrap_or_default();
                let mut blob = format!("KMS1{}", key_id).into_bytes();
                blob.extend(xor(&plain, &k.material));
                ok(obj([("CiphertextBlob", Value::from(base64_encode(&blob))), ("KeyId", Value::from(k.arn.clone())), ("EncryptionAlgorithm", Value::from("SYMMETRIC_DEFAULT"))]))
            }
            None => not_found(),
        },
        "Decrypt" => {
            let blob = base64_decode(&s_(&b, "CiphertextBlob")).unwrap_or_default();
            if blob.len() < 40 || &blob[..4] != b"KMS1" {
                return jerr(KMS, "InvalidCiphertextException", "The ciphertext is invalid");
            }
            let id = String::from_utf8_lossy(&blob[4..40]).into_owned();
            match m.kms.get(&id) {
                Some(k) if k.state != "Enabled" => jerr(KMS, "DisabledException", &format!("{} is disabled.", k.arn)),
                Some(k) => ok(obj([("Plaintext", Value::from(base64_encode(&xor(&blob[40..], &k.material)))), ("KeyId", Value::from(k.arn.clone()))])),
                None => jerr(KMS, "InvalidCiphertextException", "The ciphertext refers to an unknown key"),
            }
        }
        other => jerr(KMS, "UnsupportedOperationException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Timestream
// ---------------------------------------------------------------------------

const TIMESTREAM: &str = "com.amazonaws.timestream";

fn timestream(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let host = req.header("Host").unwrap_or("localhost").to_string();
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let (db, table) = (s_(&b, "DatabaseName"), s_(&b, "TableName"));
    let db_value = |n: &str, t: i64| obj([("DatabaseName", Value::from(n)), ("Arn", Value::from(format!("arn:aws:timestream:us-east-1:{}:database/{}", ACCOUNT, n))), ("CreationTime", Value::Float(t as f64))]);
    let table_value = |d: &str, t: &str| obj([("DatabaseName", Value::from(d)), ("TableName", Value::from(t)), ("TableStatus", Value::from("ACTIVE")), ("Arn", Value::from(format!("arn:aws:timestream:us-east-1:{}:database/{}/table/{}", ACCOUNT, d, t)))]);
    match target(req).as_str() {
        "DescribeEndpoints" => ok(obj([("Endpoints", Value::Array(vec![obj([("Address", Value::from(host)), ("CachePeriodInMinutes", Value::Int(1440))])]))])),
        "CreateDatabase" => {
            if m.ts_dbs.contains_key(&db) {
                return jerr(TIMESTREAM, "ConflictException", &format!("Database {} already exists", db));
            }
            m.ts_dbs.insert(db.clone(), now_secs());
            ok(obj([("Database", db_value(&db, now_secs()))]))
        }
        "DescribeDatabase" => match m.ts_dbs.get(&db) {
            Some(t) => ok(obj([("Database", db_value(&db, *t))])),
            None => jerr(TIMESTREAM, "ResourceNotFoundException", &format!("The database {} does not exist.", db)),
        },
        "DeleteDatabase" => {
            m.ts_dbs.remove(&db);
            m.ts_tables.retain(|(d, _), _| *d != db);
            ok(obj([]))
        }
        "ListDatabases" => ok(obj([("Databases", Value::Array(m.ts_dbs.iter().map(|(n, t)| db_value(n, *t)).collect()))])),
        "CreateTable" => {
            if !m.ts_dbs.contains_key(&db) {
                return jerr(TIMESTREAM, "ResourceNotFoundException", &format!("The database {} does not exist.", db));
            }
            m.ts_tables.entry((db.clone(), table.clone())).or_default();
            ok(obj([("Table", table_value(&db, &table))]))
        }
        "DescribeTable" => match m.ts_tables.contains_key(&(db.clone(), table.clone())) {
            true => ok(obj([("Table", table_value(&db, &table))])),
            false => jerr(TIMESTREAM, "ResourceNotFoundException", &format!("The table {} does not exist.", table)),
        },
        "DeleteTable" => {
            m.ts_tables.remove(&(db.clone(), table.clone()));
            ok(obj([]))
        }
        "ListTables" => ok(obj([("Tables", Value::Array(m.ts_tables.keys().filter(|(d, _)| db.is_empty() || *d == db).map(|(d, t)| table_value(d, t)).collect()))])),
        "WriteRecords" => {
            let Some(rows) = m.ts_tables.get_mut(&(db.clone(), table.clone())) else { return jerr(TIMESTREAM, "ResourceNotFoundException", &format!("The table {}.{} does not exist.", db, table)) };
            let common = b.get("CommonAttributes").and_then(|c| c.as_object()).cloned().unwrap_or_default();
            let records = b.get("Records").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            let mut rejected = Vec::new();
            let mut accepted = Vec::new();
            for (i, r) in records.iter().enumerate() {
                let mut merged = common.clone();
                for (k, v) in r.as_object().cloned().unwrap_or_default() {
                    merged.insert(k, v);
                }
                if merged.get("MeasureName").is_none() || merged.get("MeasureValue").is_none() {
                    rejected.push(obj([("RecordIndex", Value::Int(i as i64)), ("Reason", Value::from("Missing MeasureName or MeasureValue"))]));
                } else {
                    accepted.push(Value::Object(merged));
                }
            }
            if !rejected.is_empty() {
                return Response::json(400, obj([("__type", Value::from(format!("{}#RejectedRecordsException", TIMESTREAM))), ("message", Value::from("One or more records have been rejected.")), ("RejectedRecords", Value::Array(rejected))]).to_json());
            }
            let n = accepted.len() as i64;
            rows.extend(accepted);
            ok(obj([("RecordsIngested", obj([("Total", Value::Int(n)), ("MemoryStore", Value::Int(n)), ("MagneticStore", Value::Int(0))]))]))
        }
        "Query" => {
            let q = s_(&b, "QueryString");
            let target_table = m.ts_tables.keys().find(|(d, t)| q.contains(&format!("\"{}\".\"{}\"", d, t)) || q.contains(&format!("{}.{}", d, t))).cloned();
            let Some(key) = target_table else { return jerr(TIMESTREAM, "ValidationException", &format!("The query references an unknown table: {}", q)) };
            let records = m.ts_tables.get(&key).cloned().unwrap_or_default();
            let mut cols: Vec<String> = Vec::new();
            for r in &records {
                for d in r.get("Dimensions").and_then(|d| d.as_array()).cloned().unwrap_or_default() {
                    let n = s_(&d, "Name");
                    if !cols.contains(&n) {
                        cols.push(n);
                    }
                }
            }
            cols.extend(["measure_name".to_string(), "time".to_string(), "measure_value".to_string()]);
            let rows: Vec<Value> = records
                .iter()
                .map(|r| {
                    let dims = r.get("Dimensions").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                    let cell = |c: &str| -> Value {
                        let v = match c {
                            "measure_name" => Some(s_(r, "MeasureName")),
                            "time" => Some(s_(r, "Time")),
                            "measure_value" => Some(s_(r, "MeasureValue")),
                            _ => dims.iter().find(|d| s_(d, "Name") == c).map(|d| s_(d, "Value")),
                        };
                        match v {
                            Some(x) => obj([("ScalarValue", Value::from(x))]),
                            None => obj([("NullValue", Value::Bool(true))]),
                        }
                    };
                    obj([("Data", Value::Array(cols.iter().map(|c| cell(c)).collect()))])
                })
                .collect();
            let size = if m.ts_page_size == 0 { 1000 } else { m.ts_page_size };
            let (rows, next) = page(rows, b.get("NextToken").map(|t| t.to_string()).as_deref(), None, size);
            let mut out = ObjectMap::new();
            out.insert("QueryId".into(), Value::from(format!("q-{}", random_hex(6))));
            out.insert("Rows".into(), Value::Array(rows));
            out.insert("ColumnInfo".into(), Value::Array(cols.iter().map(|c| obj([("Name", Value::from(c.clone())), ("Type", obj([("ScalarType", Value::from("VARCHAR"))]))])).collect()));
            ok(with_token(out, "NextToken", next))
        }
        "CancelQuery" => ok(obj([("CancellationMessage", Value::from(format!("Query {} cancelled", s_(&b, "QueryId"))))])),
        other => jerr(TIMESTREAM, "ValidationException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// CloudTrail / Config
// ---------------------------------------------------------------------------

fn cloudtrail(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    if target(req) != "LookupEvents" {
        return jerr("com.amazonaws.cloudtrail.v20131101", "OperationNotPermittedException", "Unsupported action");
    }
    let g = st.0.lock().unwrap();
    let source = b.get("LookupAttributes").and_then(|a| a.as_array()).and_then(|a| a.iter().find(|x| s_(x, "AttributeKey") == "EventSource").cloned()).map(|x| s_(&x, "AttributeValue"));
    let start = b.get("StartTime").and_then(|t| t.as_f64());
    let mut evs: Vec<Value> = g
        .more
        .trail
        .iter()
        .filter(|e| source.as_ref().is_none_or(|s| *s == s_(e, "EventSource")))
        .filter(|e| start.is_none_or(|t| e.get("EventTime").and_then(|x| x.as_f64()).unwrap_or(0.0) + 1e-6 >= t))
        .cloned()
        .collect();
    evs.reverse(); // newest first
    let (evs, next) = page(evs, b.get("NextToken").map(|t| t.to_string()).as_deref(), b.get("MaxResults").and_then(|x| x.as_i64()), 50);
    let mut out = ObjectMap::new();
    out.insert("Events".into(), Value::Array(evs));
    ok(with_token(out, "NextToken", next))
}

const CONFIG: &str = "com.amazonaws.starlingdoveservice";

fn config(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let names: Option<Vec<String>> = b.get("ConfigRuleNames").and_then(|n| n.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect());
    match target(req).as_str() {
        "PutConfigRule" => {
            let rule = b.get("ConfigRule").cloned().unwrap_or_default();
            let name = s_(&rule, "ConfigRuleName");
            let src = rule.get("Source").cloned().unwrap_or_default();
            if name.is_empty() || s_(&src, "SourceIdentifier").is_empty() || s_(&src, "Owner").is_empty() {
                return jerr(CONFIG, "InvalidParameterValueException", "ConfigRuleName and Source (Owner, SourceIdentifier) are required");
            }
            let mut r = rule.as_object().cloned().unwrap_or_default();
            r.insert("ConfigRuleArn".into(), Value::from(format!("arn:aws:config:us-east-1:{}:config-rule/config-rule-{}", ACCOUNT, random_hex(3))));
            r.insert("ConfigRuleState".into(), Value::from("ACTIVE"));
            m.config_rules.insert(name, Value::Object(r));
            ok(obj([]))
        }
        "DeleteConfigRule" => match m.config_rules.remove(&s_(&b, "ConfigRuleName")) {
            Some(_) => ok(obj([])),
            None => jerr(CONFIG, "NoSuchConfigRuleException", &format!("The ConfigRule '{}' provided in the request is invalid.", s_(&b, "ConfigRuleName"))),
        },
        "DescribeConfigRules" => {
            if let Some(ns) = &names {
                if let Some(missing) = ns.iter().find(|n| !m.config_rules.contains_key(*n)) {
                    return jerr(CONFIG, "NoSuchConfigRuleException", &format!("The ConfigRule '{}' provided in the request is invalid.", missing));
                }
            }
            let rules: Vec<Value> = m.config_rules.iter().filter(|(n, _)| names.as_ref().is_none_or(|ns| ns.contains(n))).map(|(_, r)| r.clone()).collect();
            ok(obj([("ConfigRules", Value::Array(rules))]))
        }
        "DescribeComplianceByConfigRule" => {
            let list: Vec<Value> = m
                .config_rules
                .keys()
                .filter(|n| names.as_ref().is_none_or(|ns| ns.contains(n)))
                .map(|n| obj([("ConfigRuleName", Value::from(n.clone())), ("Compliance", obj([("ComplianceType", Value::from(m.compliance.get(n).cloned().unwrap_or_else(|| "COMPLIANT".into())))]))]))
                .collect();
            ok(obj([("ComplianceByConfigRules", Value::Array(list))]))
        }
        "PutConformancePack" => {
            let name = s_(&b, "ConformancePackName");
            if name.is_empty() || s_(&b, "TemplateBody").is_empty() {
                return jerr(CONFIG, "InvalidParameterValueException", "ConformancePackName and TemplateBody are required");
            }
            m.packs.insert(name.clone(), s_(&b, "TemplateBody"));
            ok(obj([("ConformancePackArn", Value::from(format!("arn:aws:config:us-east-1:{}:conformance-pack/{}/conformance-pack-{}", ACCOUNT, name, random_hex(3))))]))
        }
        "DeleteConformancePack" => match m.packs.remove(&s_(&b, "ConformancePackName")) {
            Some(_) => ok(obj([])),
            None => jerr(CONFIG, "NoSuchConformancePackException", "Conformance pack not found"),
        },
        "DescribeConformancePackCompliance" => {
            if !m.packs.contains_key(&s_(&b, "ConformancePackName")) {
                return jerr(CONFIG, "NoSuchConformancePackException", "Conformance pack not found");
            }
            let list: Vec<Value> = m.config_rules.keys().map(|n| obj([("ConfigRuleName", Value::from(n.clone())), ("ComplianceType", Value::from(m.compliance.get(n).cloned().unwrap_or_else(|| "COMPLIANT".into())))])).collect();
            ok(obj([("ConformancePackName", b.get("ConformancePackName").cloned().unwrap_or_default()), ("ConformancePackRuleComplianceList", Value::Array(list))]))
        }
        other => jerr(CONFIG, "InvalidParameterValueException", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Redshift Data
// ---------------------------------------------------------------------------

const REDSHIFT: &str = "com.amazonaws.redshiftdata";

fn field(v: &Value) -> Value {
    match v {
        Value::Null => obj([("isNull", Value::Bool(true))]),
        Value::Int(i) => obj([("longValue", Value::Int(*i))]),
        Value::Float(f) => obj([("doubleValue", Value::Float(*f))]),
        Value::Bool(b) => obj([("booleanValue", Value::Bool(*b))]),
        other => obj([("stringValue", Value::from(other.to_string()))]),
    }
}

fn redshift(st: &Shared, req: &ServerRequest) -> Response {
    let b = body(req);
    let mut g = st.0.lock().unwrap();
    let m = &mut g.more;
    let connection_ok = |b: &Value| !s_(b, "Database").is_empty() && (!s_(b, "ClusterIdentifier").is_empty() || !s_(b, "WorkgroupName").is_empty());
    let id = s_(&b, "Id");
    match target(req).as_str() {
        "ExecuteStatement" | "BatchExecuteStatement" => {
            if !connection_ok(&b) {
                return jerr(REDSHIFT, "ValidationException", "Database and either ClusterIdentifier or WorkgroupName are required");
            }
            let sqls: Vec<String> = match b.get("Sqls").and_then(|s| s.as_array()) {
                Some(a) => a.iter().map(|x| x.to_string()).collect(),
                None => vec![s_(&b, "Sql")],
            };
            let last = sqls.last().cloned().unwrap_or_default();
            let lower = last.to_ascii_lowercase();
            let mut stmt = RsStatement { sql: sqls.join("; "), database: s_(&b, "Database"), status: "SUBMITTED".into(), error: String::new(), columns: vec![], rows: vec![], has_result: false, created: now_secs() };
            if sqls.iter().any(|s| s.to_ascii_lowercase().contains("error")) {
                stmt.error = "ERROR: syntax error at or near \"error\"".into();
            } else if let Some(rest) = lower.strip_prefix("select * from ") {
                let t = rest.trim().trim_end_matches(';').to_string();
                match m.rs_tables.get(&t) {
                    Some((cols, rows)) => {
                        stmt.columns = cols.clone();
                        stmt.rows = rows.clone();
                        stmt.has_result = true;
                    }
                    None => stmt.error = format!("ERROR: relation \"{}\" does not exist", t),
                }
            } else if lower.starts_with("select") {
                stmt.columns = vec!["?column?".into()];
                stmt.rows = vec![vec![Value::Int(1)]];
                stmt.has_result = true;
            }
            let id = uuid();
            m.rs_statements.insert(id.clone(), stmt);
            ok(obj([("Id", Value::from(id)), ("Database", b.get("Database").cloned().unwrap_or_default())]))
        }
        "DescribeStatement" => match m.rs_statements.get_mut(&id) {
            Some(s) => {
                s.status = match s.status.as_str() {
                    "SUBMITTED" => "STARTED".into(),
                    _ if !s.error.is_empty() => "FAILED".into(),
                    "STARTED" => "FINISHED".into(),
                    other => other.into(),
                };
                let mut o = ObjectMap::new();
                o.insert("Id".into(), Value::from(id.clone()));
                o.insert("QueryString".into(), Value::from(s.sql.clone()));
                o.insert("Database".into(), Value::from(s.database.clone()));
                o.insert("Status".into(), Value::from(s.status.clone()));
                o.insert("HasResultSet".into(), Value::Bool(s.has_result));
                o.insert("ResultRows".into(), Value::Int(if s.status == "FINISHED" { s.rows.len() as i64 } else { -1 }));
                o.insert("CreatedAt".into(), Value::Float(s.created as f64));
                if s.status == "FAILED" {
                    o.insert("Error".into(), Value::from(s.error.clone()));
                }
                ok(Value::Object(o))
            }
            None => jerr(REDSHIFT, "ResourceNotFoundException", &format!("Query does not exist: {}", id)),
        },
        "GetStatementResult" => match m.rs_statements.get(&id) {
            Some(s) if s.status != "FINISHED" => jerr(REDSHIFT, "ValidationException", &format!("Query has not finished: {}", s.status)),
            Some(s) if !s.has_result => jerr(REDSHIFT, "ResourceNotFoundException", "Query does not have result. Please check query status with DescribeStatement."),
            Some(s) => {
                let records: Vec<Value> = s.rows.iter().map(|r| Value::Array(r.iter().map(field).collect())).collect();
                let total = records.len() as i64;
                let (records, next) = page(records, b.get("NextToken").map(|t| t.to_string()).as_deref(), None, 2);
                let mut out = ObjectMap::new();
                out.insert("Records".into(), Value::Array(records));
                out.insert("ColumnMetadata".into(), Value::Array(s.columns.iter().map(|c| obj([("name", Value::from(c.clone())), ("typeName", Value::from("varchar"))])).collect()));
                out.insert("TotalNumRows".into(), Value::Int(total));
                ok(with_token(out, "NextToken", next))
            }
            None => jerr(REDSHIFT, "ResourceNotFoundException", &format!("Query does not exist: {}", id)),
        },
        "CancelStatement" => match m.rs_statements.get_mut(&id) {
            Some(s) => {
                s.status = "ABORTED".into();
                ok(obj([("Status", Value::Bool(true))]))
            }
            None => jerr(REDSHIFT, "ResourceNotFoundException", &format!("Query does not exist: {}", id)),
        },
        "ListStatements" => ok(obj([("Statements", Value::Array(m.rs_statements.iter().map(|(id, s)| obj([("Id", Value::from(id.clone())), ("QueryString", Value::from(s.sql.clone())), ("Status", Value::from(s.status.clone()))])).collect()))])),
        "ListDatabases" => {
            if !connection_ok(&b) {
                return jerr(REDSHIFT, "ValidationException", "Database and either ClusterIdentifier or WorkgroupName are required");
            }
            ok(obj([("Databases", Value::from(vec!["dev", "sample_data_dev"]))]))
        }
        "ListSchemas" => ok(obj([("Schemas", Value::from(vec!["public", "pg_catalog"]))])),
        "ListTables" => ok(obj([("Tables", Value::Array(m.rs_tables.keys().map(|t| obj([("name", Value::from(t.clone())), ("schema", Value::from("public")), ("type", Value::from("TABLE"))])).collect()))])),
        "DescribeTable" => match m.rs_tables.get(&s_(&b, "Table").to_ascii_lowercase()) {
            Some((cols, _)) => ok(obj([("TableName", b.get("Table").cloned().unwrap_or_default()), ("ColumnList", Value::Array(cols.iter().map(|c| obj([("name", Value::from(c.clone())), ("typeName", Value::from("varchar"))])).collect()))])),
            None => jerr(REDSHIFT, "ResourceNotFoundException", "Table not found"),
        },
        other => jerr(REDSHIFT, "ValidationException", &format!("Unsupported action {}", other)),
    }
}
