//! MongoDB: BSON codec, `OP_MSG` wire protocol, client (SCRAM-SHA-256, TLS), the Camel
//! `mongodb` component and an embedded test server with a query engine.
//!
//! `mongodb:connectionBean?database=db&collection=coll&operation=findAll`
//!
//! The connection is the registry bean named by the path (a [`MongoClient`]), or
//! `connectionUriString=mongodb://...`, or `hosts=h1:27017,h2` with `username`/`password`/`authSource`.
//! TLS: `tls=true` in the connection string (trust from `sslContextParameters=#bean`, the
//! global `camel.ssl.*` parameters, else the system trust store; `tlsInsecure=true` skips
//! verification).
//!
//! **Producer operations** (`operation`, overridden by header `CamelMongoDbOperation`):
//! `findById`, `findOneByQuery`, `findAll`, `findDistinct`, `count`, `insert`, `save`,
//! `update`, `remove`, `aggregate`, `getDbStats`, `getColStats`, `command`.
//! Bodies are documents (objects, JSON text with Extended JSON/shell syntax, or arrays of
//! them); results are objects/arrays with Extended JSON for BSON-only types
//! (`{"$oid": ..}`, `{"$date": ..}`, ...). Headers: `CamelMongoDbCriteria`,
//! `CamelMongoDbFieldsProjection`, `CamelMongoDbSortBy`, `CamelMongoDbLimit`,
//! `CamelMongoDbNumToSkip`, `CamelMongoDbBatchSize`, `CamelMongoDbMultiUpdate`,
//! `CamelMongoDbUpsert`, `CamelMongoDbDistinctQueryField`, `CamelMongoDbAllowDiskUse`,
//! `CamelMongoDbDatabase`/`CamelMongoDbCollection` (with `dynamicity=true`); results set
//! `CamelMongoDbRecordsAffected`, `CamelMongoDbRecordsMatched`, `CamelMongoDbResultTotalSize`,
//! `CamelMongoDbResultPageSize`, `CamelMongoOid` and, with `writeResultAsHeader=true`,
//! `CamelMongoWriteResult`.
//!
//! **Consumers**: `consumerType=tailable` (capped collection, `tailTrackIncreasingField`,
//! `cursorRegenerationDelay`; tail tracking is kept in memory) and
//! `consumerType=changeStreams` (`streamFilter` = `$match` JSON, `fullDocument=updateLookup`);
//! change events set `CamelMongoDbStreamOperationType` and `_id` (the document key).

pub mod bson;
pub mod client;
pub mod query;
pub mod server;
pub mod wire;

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::{process_consumer_exchange, BackgroundThreads, StopFlag};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

pub use bson::{Bson, Decimal128, Document, ObjectId};
pub use client::{FindOptions, MongoClient, MongoClientOptions, MongoConnection, UpdateResult};

use crate::kinds;
use crate::util::{header, header_bool, header_i64, header_str};

/// Header names (Camel `MongoDbConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelMongoDbOperation";
    pub const DATABASE: &str = "CamelMongoDbDatabase";
    pub const COLLECTION: &str = "CamelMongoDbCollection";
    pub const CRITERIA: &str = "CamelMongoDbCriteria";
    pub const FIELDS_PROJECTION: &str = "CamelMongoDbFieldsProjection";
    pub const SORT_BY: &str = "CamelMongoDbSortBy";
    pub const LIMIT: &str = "CamelMongoDbLimit";
    pub const NUM_TO_SKIP: &str = "CamelMongoDbNumToSkip";
    pub const BATCH_SIZE: &str = "CamelMongoDbBatchSize";
    pub const MULTI_UPDATE: &str = "CamelMongoDbMultiUpdate";
    pub const UPSERT: &str = "CamelMongoDbUpsert";
    pub const RECORDS_AFFECTED: &str = "CamelMongoDbRecordsAffected";
    pub const RECORDS_MATCHED: &str = "CamelMongoDbRecordsMatched";
    pub const RESULT_TOTAL_SIZE: &str = "CamelMongoDbResultTotalSize";
    pub const RESULT_PAGE_SIZE: &str = "CamelMongoDbResultPageSize";
    pub const OID: &str = "CamelMongoOid";
    pub const WRITE_RESULT: &str = "CamelMongoWriteResult";
    pub const DISTINCT_QUERY_FIELD: &str = "CamelMongoDbDistinctQueryField";
    pub const ALLOW_DISK_USE: &str = "CamelMongoDbAllowDiskUse";
    pub const STREAM_OPERATION_TYPE: &str = "CamelMongoDbStreamOperationType";
    pub const MONGO_ID: &str = "_id";
}

/// Producer operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    FindById,
    FindOneByQuery,
    FindAll,
    FindDistinct,
    Count,
    Insert,
    Save,
    Update,
    Remove,
    Aggregate,
    GetDbStats,
    GetColStats,
    Command,
}

impl Operation {
    pub fn parse(s: &str) -> Result<Operation> {
        Ok(match s {
            "findById" => Operation::FindById,
            "findOneByQuery" => Operation::FindOneByQuery,
            "findAll" => Operation::FindAll,
            "findDistinct" => Operation::FindDistinct,
            "count" => Operation::Count,
            "insert" => Operation::Insert,
            "save" => Operation::Save,
            "update" => Operation::Update,
            "remove" => Operation::Remove,
            "aggregate" => Operation::Aggregate,
            "getDbStats" => Operation::GetDbStats,
            "getColStats" => Operation::GetColStats,
            "command" => Operation::Command,
            "bulkWrite" => return Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "The mongodb bulkWrite operation is not supported")),
            other => return Err(CamelError::illegal_argument(format!("Unknown mongodb operation: {}", other))),
        })
    }
}

/// The `mongodb` component.
#[derive(Default)]
pub struct MongoDbComponent;

/// Endpoint configuration.
#[derive(Clone)]
pub struct MongoEndpointConfig {
    pub client: Arc<MongoClient>,
    pub database: Option<String>,
    pub collection: Option<String>,
    pub operation: Option<Operation>,
    pub create_collection: bool,
    pub dynamicity: bool,
    pub write_result_as_header: bool,
    pub consumer_type: Option<String>,
    pub tail_track_field: Option<String>,
    pub cursor_regeneration_delay: Duration,
    pub stream_filter: Option<Document>,
    pub full_document: Option<String>,
}

const IGNORED: &[&str] = &[
    "lazyStartProducer",
    "bridgeErrorHandler",
    "exceptionHandler",
    "exchangePattern",
    "collectionIndex",
    "outputType",
    "readPreference",
    "writeConcern",
    "persistentTailTracking",
    "persistentId",
    "tailTrackDb",
    "tailTrackCollection",
    "tailTrackField",
    "mongoConnection",
    "replicaSet",
    "appName",
    "compressors",
    "maxPoolSize",
    "minPoolSize",
    "retryWrites",
    "retryReads",
];

impl Component for MongoDbComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let bean = remaining.trim_start_matches('/').trim_start_matches('#').to_string();
        let explicit_tls = camel_tls::camel_support::take_endpoint_ssl(ctx, params, "mongodb")?;
        let client = if let Some(cs) = params.take("connectionUriString") {
            let mut o = MongoClientOptions::parse(&cs)?;
            resolve_tls(ctx, &mut o, explicit_tls.as_ref())?;
            Arc::new(MongoClient::new(o))
        } else if let Some(hosts) = params.take("hosts") {
            let mut s = String::from("mongodb://");
            let user = params.take("username");
            let pass = params.take("password");
            if let Some(u) = &user {
                s.push_str(&camel_core::util::url_encode(u));
                if let Some(p) = &pass {
                    s.push(':');
                    s.push_str(&camel_core::util::url_encode(p));
                }
                s.push('@');
            }
            s.push_str(&hosts);
            s.push('/');
            if let Some(a) = params.take("authSource") {
                s.push_str(&format!("?authSource={}", a));
            }
            let mut o = MongoClientOptions::parse(&s)?;
            resolve_tls(ctx, &mut o, explicit_tls.as_ref())?;
            Arc::new(MongoClient::new(o))
        } else {
            if bean.is_empty() {
                return Err(CamelError::illegal_argument("mongodb endpoint needs a connection bean name, connectionUriString or hosts"));
            }
            ctx.registry().lookup::<MongoClient>(&bean).ok_or_else(|| CamelError::no_such_bean(&bean))?
        };
        let operation = match params.take("operation") {
            Some(o) => Some(Operation::parse(&o)?),
            None => None,
        };
        let stream_filter = match params.take("streamFilter").filter(|s| !s.trim().is_empty()) {
            Some(f) => Some(Document::parse_json(&f)?),
            None => None,
        };
        let cfg = MongoEndpointConfig {
            client,
            database: params.take("database"),
            collection: params.take("collection"),
            operation,
            create_collection: params.take_bool("createCollection", true)?,
            dynamicity: params.take_bool("dynamicity", false)?,
            write_result_as_header: params.take_bool("writeResultAsHeader", false)?,
            consumer_type: params.take("consumerType"),
            tail_track_field: params.take("tailTrackIncreasingField"),
            cursor_regeneration_delay: params.take_duration("cursorRegenerationDelay", Duration::from_millis(1000))?,
            stream_filter,
            full_document: params.take("fullDocument"),
        };
        if let Some(t) = &cfg.consumer_type {
            if t != "tailable" && t != "changeStreams" {
                return Err(CamelError::illegal_argument(format!("Unknown consumerType {} (tailable or changeStreams)", t)));
            }
        }
        for k in IGNORED {
            params.take(k);
        }
        Ok(Arc::new(MongoDbEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn resolve_tls(ctx: &CamelContext, o: &mut MongoClientOptions, explicit: Option<&Arc<camel_tls::SslContextParameters>>) -> Result<()> {
    if !o.tls_requested && explicit.is_none() {
        return Ok(());
    }
    let mut p = (*camel_tls::camel_support::effective_client_params(ctx, explicit)?).clone();
    if o.tls_insecure {
        p.insecure_skip_verify = true;
    }
    o.tls = Some(camel_tls::camel_support::EndpointTls::new(Arc::new(p)));
    Ok(())
}

/// A `mongodb` endpoint.
pub struct MongoDbEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: MongoEndpointConfig,
}

impl MongoDbEndpoint {
    pub fn config(&self) -> &MongoEndpointConfig {
        &self.cfg
    }
}

impl Endpoint for MongoDbEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(MongoProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let kind = self.cfg.consumer_type.clone().unwrap_or_else(|| "tailable".into());
        if self.cfg.database.is_none() || self.cfg.collection.is_none() {
            return Err(CamelError::illegal_argument("A mongodb consumer needs database and collection"));
        }
        if kind == "tailable" && self.cfg.tail_track_field.is_none() {
            return Err(CamelError::illegal_argument("A tailable mongodb consumer needs tailTrackIncreasingField"));
        }
        Ok(Arc::new(MongoConsumer { cfg: self.cfg.clone(), kind, ctx: self.ctx.clone(), processor, logger: self.uri.clone(), threads: BackgroundThreads::new(), closer: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn to_doc(v: &Value, what: &str) -> Result<Document> {
    bson::to_document(v).map_err(|e| CamelError::new(kinds::CAMEL_MONGODB, format!("Invalid {}: {}", what, e.message)))
}

fn header_doc(ex: &Exchange, name: &str) -> Result<Option<Document>> {
    match header(ex, name) {
        Some(v) => Ok(Some(to_doc(v, name)?)),
        None => Ok(None),
    }
}

/// Documents from a body (one document, an array of them, or JSON text of either).
fn body_docs(v: &Value) -> Result<Vec<Document>> {
    let parsed = match v {
        Value::String(s) if s.trim_start().starts_with('[') => bson::parse_json(s)?,
        Value::Bytes(b) if String::from_utf8_lossy(b).trim_start().starts_with('[') => bson::parse_json(&String::from_utf8_lossy(b))?,
        Value::Array(a) => Bson::Array(a.iter().map(Bson::from_value).collect()),
        other => Bson::Document(to_doc(other, "document")?),
    };
    match parsed {
        Bson::Array(a) => a.into_iter().map(|d| d.as_document().cloned().ok_or_else(|| CamelError::new(kinds::CAMEL_MONGODB, "array element is not a document"))).collect(),
        Bson::Document(d) => Ok(vec![d]),
        _ => Err(CamelError::new(kinds::CAMEL_MONGODB, "body is not a document")),
    }
}

fn update_result_value(r: &UpdateResult) -> Value {
    let mut m = Value::object([("matchedCount", Value::Int(r.matched)), ("modifiedCount", Value::Int(r.modified))]);
    if let (Some(id), Some(o)) = (&r.upserted_id, m.as_object_mut()) {
        o.insert("upsertedId".into(), id.to_value());
    }
    m
}

struct MongoProducer {
    cfg: MongoEndpointConfig,
}

impl MongoProducer {
    fn target(&self, ex: &Exchange) -> Result<(String, String)> {
        let mut db = self.cfg.database.clone();
        let mut coll = self.cfg.collection.clone();
        if self.cfg.dynamicity {
            if let Some(d) = header_str(ex, headers::DATABASE) {
                db = Some(d);
            }
            if let Some(c) = header_str(ex, headers::COLLECTION) {
                coll = Some(c);
            }
        }
        let db = db.or_else(|| self.cfg.client.options().default_database.clone()).ok_or_else(|| CamelError::illegal_argument("mongodb: database is not set"))?;
        Ok((db, coll.unwrap_or_default()))
    }

    fn criteria(&self, ex: &Exchange) -> Result<Document> {
        match header_doc(ex, headers::CRITERIA)? {
            Some(c) => Ok(c),
            None => match ex.body() {
                Value::Null => Ok(Document::new()),
                b => to_doc(b, "query"),
            },
        }
    }

    fn write_result(&self, ex: &mut Exchange, v: Value) {
        if self.cfg.write_result_as_header {
            ex.set_header(headers::WRITE_RESULT, v);
        } else {
            ex.set_body(v);
        }
    }
}

impl Service for MongoProducer {
    fn start(&self) -> Result<()> {
        if !self.cfg.create_collection {
            if let (Some(db), Some(coll)) = (&self.cfg.database, &self.cfg.collection) {
                let names = self.cfg.client.list_collection_names(db)?;
                if !names.contains(coll) {
                    return Err(CamelError::new(
                        kinds::CAMEL_MONGODB,
                        format!("Could not initialise MongoDbComponent. Collection {} does not exist on the database and createCollection is false.", coll),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Processor for MongoProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match header_str(ex, headers::OPERATION) {
            Some(o) => Operation::parse(&o)?,
            None => self.cfg.operation.ok_or_else(|| CamelError::illegal_argument("mongodb: operation is not set"))?,
        };
        let (db, coll) = self.target(ex)?;
        let client = &self.cfg.client;
        let need_coll = || if coll.is_empty() { Err(CamelError::illegal_argument("mongodb: collection is not set")) } else { Ok(()) };
        let projection = header_doc(ex, headers::FIELDS_PROJECTION)?;
        let sort = header_doc(ex, headers::SORT_BY)?;
        match op {
            Operation::FindById => {
                need_coll()?;
                let id = Bson::from_value(ex.body());
                let o = FindOptions { projection, limit: Some(-1), ..Default::default() };
                let found = client.find(&db, &coll, Document::new().with("_id", id), &o)?;
                ex.set_header(headers::RESULT_TOTAL_SIZE, found.len() as i64);
                ex.set_body(found.into_iter().next().map(|d| d.to_value()).unwrap_or(Value::Null));
            }
            Operation::FindOneByQuery => {
                need_coll()?;
                let o = FindOptions { projection, sort, limit: Some(-1), ..Default::default() };
                let found = client.find(&db, &coll, self.criteria(ex)?, &o)?;
                ex.set_header(headers::RESULT_TOTAL_SIZE, found.len() as i64);
                ex.set_body(found.into_iter().next().map(|d| d.to_value()).unwrap_or(Value::Null));
            }
            Operation::FindAll => {
                need_coll()?;
                let limit = header_i64(ex, headers::LIMIT)?;
                let o = FindOptions { projection, sort, limit, skip: header_i64(ex, headers::NUM_TO_SKIP)?, batch_size: header_i64(ex, headers::BATCH_SIZE)?, ..Default::default() };
                let found = client.find(&db, &coll, self.criteria(ex)?, &o)?;
                if limit.is_some() {
                    ex.set_header(headers::RESULT_PAGE_SIZE, found.len() as i64);
                }
                ex.set_header(headers::RESULT_TOTAL_SIZE, found.len() as i64);
                ex.set_body(Value::Array(found.iter().map(Document::to_value).collect()));
            }
            Operation::FindDistinct => {
                need_coll()?;
                let field = header_str(ex, headers::DISTINCT_QUERY_FIELD).ok_or_else(|| CamelError::new(camel_core::error::kinds::NO_SUCH_HEADER, format!("No '{}' header available", headers::DISTINCT_QUERY_FIELD)))?;
                let values = client.distinct(&db, &coll, &field, self.criteria(ex)?)?;
                ex.set_body(Value::Array(values.iter().map(Bson::to_value).collect()));
            }
            Operation::Count => {
                need_coll()?;
                let n = client.count(&db, &coll, self.criteria(ex)?)?;
                ex.set_body(n);
            }
            Operation::Insert => {
                need_coll()?;
                let single = !matches!(ex.body(), Value::Array(_)) && !ex.body().to_string().trim_start().starts_with('[');
                let docs = client.insert_many(&db, &coll, body_docs(ex.body())?, true)?;
                let ids: Vec<Value> = docs.iter().map(|d| d.get("_id").map(Bson::to_value).unwrap_or(Value::Null)).collect();
                if single {
                    ex.set_header(headers::OID, ids.into_iter().next().unwrap_or(Value::Null));
                    ex.set_body(docs[0].to_value());
                } else {
                    ex.set_header(headers::OID, Value::Array(ids));
                    ex.set_body(Value::Array(docs.iter().map(Document::to_value).collect()));
                }
            }
            Operation::Save => {
                need_coll()?;
                let mut doc = to_doc(ex.body(), "document")?;
                if !doc.contains_key("_id") {
                    doc.insert_first("_id", ObjectId::new());
                }
                let id = doc.get("_id").cloned().unwrap();
                let r = client.update(&db, &coll, Document::new().with("_id", id.clone()), doc, false, true)?;
                ex.set_header(headers::OID, id.to_value());
                ex.set_header(headers::RECORDS_AFFECTED, r.modified + r.upserted_id.is_some() as i64);
                self.write_result(ex, update_result_value(&r));
            }
            Operation::Update => {
                need_coll()?;
                let (filter, update) = match header_doc(ex, headers::CRITERIA)? {
                    Some(c) => (c, to_doc(ex.body(), "update")?),
                    None => {
                        let docs = body_docs(ex.body())?;
                        if docs.len() != 2 {
                            return Err(CamelError::new(kinds::CAMEL_MONGODB, "MongoDB update needs a body with [criteria, update] or the CamelMongoDbCriteria header"));
                        }
                        let mut it = docs.into_iter();
                        (it.next().unwrap(), it.next().unwrap())
                    }
                };
                let multi = header_bool(ex, headers::MULTI_UPDATE).unwrap_or(false);
                let upsert = header_bool(ex, headers::UPSERT).unwrap_or(false);
                let r = client.update(&db, &coll, filter, update, multi, upsert)?;
                ex.set_header(headers::RECORDS_AFFECTED, r.modified);
                ex.set_header(headers::RECORDS_MATCHED, r.matched);
                if let Some(id) = &r.upserted_id {
                    ex.set_header(headers::OID, id.to_value());
                }
                self.write_result(ex, update_result_value(&r));
            }
            Operation::Remove => {
                need_coll()?;
                let n = client.delete(&db, &coll, self.criteria(ex)?, 0)?;
                ex.set_header(headers::RECORDS_AFFECTED, n);
                self.write_result(ex, Value::object([("deletedCount", n)]));
            }
            Operation::Aggregate => {
                need_coll()?;
                let pipeline: Vec<Bson> = body_docs(ex.body())?.into_iter().map(Bson::Document).collect();
                let out = client.aggregate(&db, &coll, pipeline, header_bool(ex, headers::ALLOW_DISK_USE).unwrap_or(false), header_i64(ex, headers::BATCH_SIZE)?)?;
                ex.set_body(Value::Array(out.iter().map(Document::to_value).collect()));
            }
            Operation::GetDbStats => {
                let r = client.run_command(&db, Document::new().with("dbStats", 1))?;
                ex.set_body(r.to_value());
            }
            Operation::GetColStats => {
                need_coll()?;
                let r = client.run_command(&db, Document::new().with("collStats", coll.as_str()))?;
                ex.set_body(r.to_value());
            }
            Operation::Command => {
                let cmd = to_doc(ex.body(), "command")?;
                let r = client.run_command(&db, cmd)?;
                ex.set_body(r.to_value());
            }
        }
        Ok(())
    }
}

struct MongoConsumer {
    cfg: MongoEndpointConfig,
    kind: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    threads: BackgroundThreads,
    closer: Arc<Mutex<Option<camel_tls::net::NetStream>>>,
}

/// State kept between cursor regenerations.
struct Tail {
    last_value: Option<Bson>,
    resume_token: Option<Document>,
}

impl MongoConsumer {
    fn open(&self, conn: &mut MongoConnection, tail: &Tail) -> Result<client::CursorBatch> {
        let db = self.cfg.database.as_deref().unwrap_or_default();
        let coll = self.cfg.collection.as_deref().unwrap_or_default();
        if self.kind == "tailable" {
            let field = self.cfg.tail_track_field.as_deref().unwrap_or("_id");
            let filter = match &tail.last_value {
                Some(v) => Document::new().with(field, Document::new().with("$gt", v.clone())),
                None => Document::new(),
            };
            let o = FindOptions { tailable: true, await_data: true, ..Default::default() };
            client::open_cursor_on(conn, db, client::find_command(coll, filter, &o))
        } else {
            let mut cs = Document::new();
            if let Some(f) = &self.cfg.full_document {
                cs.insert("fullDocument", f.as_str());
            }
            if let Some(t) = &tail.resume_token {
                cs.insert("resumeAfter", t.clone());
            }
            let mut pipeline = vec![Bson::Document(Document::new().with("$changeStream", cs))];
            if let Some(f) = &self.cfg.stream_filter {
                pipeline.push(Bson::Document(Document::new().with("$match", f.clone())));
            }
            client::open_cursor_on(conn, db, Document::new().with("aggregate", coll).with("pipeline", Bson::Array(pipeline)).with("cursor", Document::new()))
        }
    }

    fn deliver(&self, doc: Document, tail: &mut Tail) {
        let mut ex = Exchange::new(self.ctx.upgrade());
        if self.kind == "tailable" {
            if let Some(f) = &self.cfg.tail_track_field {
                if let Some(v) = query::get_path(&doc, f) {
                    tail.last_value = Some(v);
                }
            }
            ex.set_body(doc.to_value());
        } else {
            tail.resume_token = doc.get_doc("_id").cloned();
            ex.set_header(headers::STREAM_OPERATION_TYPE, doc.get_str("operationType").unwrap_or("").to_string());
            if let Some(id) = doc.get_doc("documentKey").and_then(|k| k.get("_id")) {
                ex.set_header(headers::MONGO_ID, id.to_value());
            }
            ex.set_body(doc.get("fullDocument").map(Bson::to_value).unwrap_or(Value::Null));
        }
        process_consumer_exchange(&self.logger, self.processor.as_ref(), &mut ex);
    }

    fn run(self: Arc<Self>, stop: Arc<StopFlag>, first: Option<(MongoConnection, client::CursorBatch)>) {
        let db = self.cfg.database.clone().unwrap_or_default();
        let coll = self.cfg.collection.clone().unwrap_or_default();
        let mut tail = Tail { last_value: None, resume_token: None };
        let mut state = first;
        while !stop.is_stopped() {
            let (mut conn, mut batch) = match state.take() {
                Some(s) => s,
                None => {
                    let opened = self.cfg.client.connect().and_then(|mut c| {
                        let b = self.open(&mut c, &tail)?;
                        Ok((c, b))
                    });
                    match opened {
                        Ok(s) => s,
                        Err(e) => {
                            camel_core::camel_warn!("mongodb", "Cannot open {} cursor on {}.{}: {}", self.kind, db, coll, e);
                            stop.sleep(self.cfg.cursor_regeneration_delay);
                            continue;
                        }
                    }
                }
            };
            *self.closer.lock().unwrap() = conn.closer();
            loop {
                if let Some(t) = &batch.post_batch_resume_token {
                    if self.kind != "tailable" {
                        tail.resume_token = Some(t.clone());
                    }
                }
                for d in std::mem::take(&mut batch.docs) {
                    if stop.is_stopped() {
                        break;
                    }
                    self.deliver(d, &mut tail);
                }
                if stop.is_stopped() || batch.id == 0 {
                    break;
                }
                match client::get_more_on(&mut conn, &db, &coll, batch.id, None, Some(300)) {
                    Ok(b) => batch = b,
                    Err(e) => {
                        if !stop.is_stopped() {
                            camel_core::camel_warn!("mongodb", "{} cursor on {}.{} failed: {}", self.kind, db, coll, e);
                        }
                        break;
                    }
                }
            }
            if !stop.is_stopped() {
                stop.sleep(self.cfg.cursor_regeneration_delay);
            }
        }
    }
}

impl Service for MongoConsumer {
    fn start(&self) -> Result<()> {
        // open the first cursor synchronously so that events after start() are seen
        let tail = Tail { last_value: None, resume_token: None };
        let mut conn = self.cfg.client.connect()?;
        let first = self.open(&mut conn, &tail)?;
        let me = Arc::new(MongoConsumer {
            cfg: self.cfg.clone(),
            kind: self.kind.clone(),
            ctx: self.ctx.clone(),
            processor: self.processor.clone(),
            logger: self.logger.clone(),
            threads: BackgroundThreads::new(),
            closer: self.closer.clone(),
        });
        self.threads.spawn("mongodb-consumer", move |stop| me.run(stop, Some((conn, first))))
    }
    fn stop(&self) -> Result<()> {
        let flag = self.threads.flag();
        flag.stop();
        if let Some(c) = self.closer.lock().unwrap().take() {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        self.threads.stop();
        Ok(())
    }
}
