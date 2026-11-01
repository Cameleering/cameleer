//! The Camel `mongodb-gridfs` component: files stored in GridFS buckets (`<bucket>.files`
//! + `<bucket>.chunks`, the GridFS specification layout shared by all drivers).
//!
//! `mongodb-gridfs:connectionBean?database=db&bucket=fs&operation=create`
//!
//! The connection is a [`MongoClient`] bean (path) or `connectionUriString`.
//!
//! **Producer operations** (`operation`, header `gridfs.operation` overrides):
//! * `create` (default): stores the body (bytes / text) as file `CamelFileName` with
//!   `gridfs.chunksize` (default 261120 bytes), `Content-Type` and `gridfs.metadata` (JSON
//!   object); sets `gridfs.fileid` to the new file id.
//! * `remove`: deletes the file `gridfs.objectid` (hex id) or all files named
//!   `CamelFileName`, with their chunks.
//! * `findOne`: reads the file `gridfs.objectid` or the newest file named `CamelFileName`;
//!   body = content bytes, headers `CamelFileName`, `CamelFileLength`,
//!   `CamelFileLastModified`, `Content-Type`, `gridfs.metadata`, `gridfs.fileid`.
//! * `listAll`: body = `filename<TAB>id` lines (all files, or those named `CamelFileName`).
//! * `count`: number of files (all, or named `CamelFileName`).
//!
//! **Consumer**: polls the bucket (`delay`, `initialDelay`; extra filter `query` JSON) and
//! routes each new file like `findOne`. `queryStrategy`: `TimeStamp` (default; files
//! uploaded after the consumer started, by `uploadDate`), `FileAttribute` (files without
//! the `metadata.<fileAttributeName>` marker, default `camel-processed`, which is set to
//! `processing` while routing and `done` afterwards) or `TimeStampAndFileAttribute`.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use super::bson::{Bson, Document, ObjectId};
use super::client::{FindOptions, MongoClient, MongoClientOptions};
use crate::kinds;
use crate::util::{header_i64, header_str, now_millis};

/// Header names (Camel `GridFsConstants`).
pub mod headers {
    pub const METADATA: &str = "gridfs.metadata";
    pub const CHUNK_SIZE: &str = "gridfs.chunksize";
    pub const FILE_ID_PRODUCED: &str = "gridfs.fileid";
    pub const OBJECT_ID: &str = "gridfs.objectid";
    pub const OPERATION: &str = "gridfs.operation";
    pub const FILE_NAME: &str = "CamelFileName";
    pub const FILE_LENGTH: &str = "CamelFileLength";
    pub const FILE_LAST_MODIFIED: &str = "CamelFileLastModified";
    pub const CONTENT_TYPE: &str = "Content-Type";
}

/// Default chunk size (255 KiB, as the drivers).
pub const DEFAULT_CHUNK_SIZE: i64 = 261_120;

/// A GridFS bucket.
#[derive(Clone)]
pub struct GridFsBucket {
    pub client: Arc<MongoClient>,
    pub database: String,
    pub bucket: String,
}

/// A stored file's descriptor (`<bucket>.files` document).
#[derive(Clone, Debug)]
pub struct GridFsFile {
    pub id: Bson,
    pub filename: String,
    pub length: i64,
    pub chunk_size: i64,
    pub upload_date: i64,
    pub content_type: Option<String>,
    pub metadata: Option<Document>,
}

impl GridFsFile {
    fn from_doc(d: &Document) -> GridFsFile {
        GridFsFile {
            id: d.get("_id").cloned().unwrap_or(Bson::Null),
            filename: d.get_str("filename").unwrap_or("").to_string(),
            length: d.get_i64("length").unwrap_or(0),
            chunk_size: d.get_i64("chunkSize").unwrap_or(DEFAULT_CHUNK_SIZE),
            upload_date: match d.get("uploadDate") {
                Some(Bson::DateTime(t)) => *t,
                _ => 0,
            },
            content_type: d.get_str("contentType").map(String::from).or_else(|| d.get_doc("metadata").and_then(|m| m.get_str("contentType")).map(String::from)),
            metadata: d.get_doc("metadata").cloned(),
        }
    }

    fn id_text(&self) -> String {
        match &self.id {
            Bson::ObjectId(o) => o.to_hex(),
            other => other.to_value().to_string(),
        }
    }
}

impl GridFsBucket {
    fn files(&self) -> String {
        format!("{}.files", self.bucket)
    }
    fn chunks(&self) -> String {
        format!("{}.chunks", self.bucket)
    }

    /// Stores a file; returns its id.
    pub fn upload(&self, filename: &str, data: &[u8], chunk_size: i64, content_type: Option<&str>, metadata: Option<Document>) -> Result<ObjectId> {
        let id = ObjectId::new();
        let cs = chunk_size.max(1) as usize;
        let chunks: Vec<Document> = data.chunks(cs).enumerate().map(|(n, c)| Document::new().with("_id", ObjectId::new()).with("files_id", id).with("n", n as i32).with("data", Bson::Binary(0, c.to_vec()))).collect();
        if !chunks.is_empty() {
            self.client.insert_many(&self.database, &self.chunks(), chunks, true)?;
        }
        let mut f = Document::new().with("_id", id).with("length", Bson::Int64(data.len() as i64)).with("chunkSize", Bson::Int32(cs as i32)).with("uploadDate", Bson::DateTime(now_millis())).with("filename", filename);
        if let Some(ct) = content_type {
            f.insert("contentType", ct);
        }
        if let Some(m) = metadata {
            f.insert("metadata", m);
        }
        self.client.insert_many(&self.database, &self.files(), vec![f], true)?;
        Ok(id)
    }

    /// File descriptors matching `filter`, oldest first.
    pub fn find(&self, filter: Document) -> Result<Vec<GridFsFile>> {
        let o = FindOptions { sort: Some(Document::new().with("uploadDate", 1).with("_id", 1)), ..Default::default() };
        Ok(self.client.find(&self.database, &self.files(), filter, &o)?.iter().map(GridFsFile::from_doc).collect())
    }

    /// Reads the content of a file (chunks in order, length checked).
    pub fn download(&self, f: &GridFsFile) -> Result<Vec<u8>> {
        let o = FindOptions { sort: Some(Document::new().with("n", 1)), ..Default::default() };
        let chunks = self.client.find(&self.database, &self.chunks(), Document::new().with("files_id", f.id.clone()), &o)?;
        let mut out = Vec::with_capacity(f.length.max(0) as usize);
        for (i, c) in chunks.iter().enumerate() {
            if c.get_i64("n") != Some(i as i64) {
                return Err(CamelError::new(kinds::MONGO, format!("GridFS: chunk {} of file {} is missing", i, f.id_text())));
            }
            match c.get("data") {
                Some(Bson::Binary(_, b)) => out.extend_from_slice(b),
                _ => return Err(CamelError::new(kinds::MONGO, "GridFS: chunk without binary data")),
            }
        }
        if out.len() as i64 != f.length {
            return Err(CamelError::new(kinds::MONGO, format!("GridFS: file {} has {} bytes instead of {}", f.id_text(), out.len(), f.length)));
        }
        Ok(out)
    }

    /// Deletes a file and its chunks.
    pub fn delete(&self, id: &Bson) -> Result<()> {
        self.client.delete(&self.database, &self.files(), Document::new().with("_id", id.clone()), 1)?;
        self.client.delete(&self.database, &self.chunks(), Document::new().with("files_id", id.clone()), 0)?;
        Ok(())
    }

    fn set_marker(&self, id: &Bson, attr: &str, value: &str) -> Result<()> {
        let upd = Document::new().with("$set", Document::new().with(format!("metadata.{}", attr), value));
        self.client.update(&self.database, &self.files(), Document::new().with("_id", id.clone()), upd, false, false).map(|_| ())
    }
}

/// The `mongodb-gridfs` component.
#[derive(Default)]
pub struct GridFsComponent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    TimeStamp,
    FileAttribute,
    Both,
}

#[derive(Clone)]
struct Config {
    bucket: GridFsBucket,
    operation: String,
    query: Option<Document>,
    strategy: Strategy,
    file_attribute: String,
}

const OPERATIONS: &[&str] = &["create", "remove", "findOne", "listAll", "count"];

impl Component for GridFsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let bean = remaining.trim_start_matches('/').trim_start_matches('#').to_string();
        let client = match params.take("connectionUriString") {
            Some(cs) => Arc::new(MongoClient::new(MongoClientOptions::parse(&cs)?)),
            None => {
                if bean.is_empty() {
                    return Err(CamelError::illegal_argument("mongodb-gridfs endpoint needs a connection bean name or connectionUriString"));
                }
                ctx.registry().lookup::<MongoClient>(&bean).ok_or_else(|| CamelError::no_such_bean(&bean))?
            }
        };
        let database = params.take("database").or_else(|| client.options().default_database.clone()).ok_or_else(|| CamelError::illegal_argument("mongodb-gridfs: the database option is required"))?;
        let bucket = params.take("bucket").filter(|b| !b.is_empty()).unwrap_or_else(|| "fs".into());
        let operation = params.take("operation").unwrap_or_else(|| "create".into());
        if !OPERATIONS.contains(&operation.as_str()) {
            return Err(CamelError::illegal_argument(format!("mongodb-gridfs: unknown operation '{}' ({})", operation, OPERATIONS.join(", "))));
        }
        let query = match params.take("query").filter(|q| !q.trim().is_empty()) {
            Some(q) => Some(Document::parse_json(&q)?),
            None => None,
        };
        let strategy = match params.take("queryStrategy").unwrap_or_else(|| "TimeStamp".into()).as_str() {
            "TimeStamp" | "PersistentTimestamp" => Strategy::TimeStamp,
            "FileAttribute" => Strategy::FileAttribute,
            "TimeStampAndFileAttribute" | "PersistentTimestampAndFileAttribute" => Strategy::Both,
            other => return Err(CamelError::illegal_argument(format!("mongodb-gridfs: unknown queryStrategy '{}'", other))),
        };
        let file_attribute = params.take("fileAttributeName").unwrap_or_else(|| "camel-processed".into());
        let poll = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        for k in ["readPreference", "writeConcern", "persistentTSCollection", "persistentTSObject", "lazyStartProducer"] {
            params.take(k);
        }
        let cfg = Config { bucket: GridFsBucket { client, database, bucket }, operation, query, strategy, file_attribute };
        Ok(Arc::new(GridFsEndpoint { uri: uri.to_string(), cfg, poll, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `mongodb-gridfs` endpoint.
pub struct GridFsEndpoint {
    uri: String,
    cfg: Config,
    poll: ScheduledPollConfig,
    ctx: WeakContext,
}

impl GridFsEndpoint {
    /// The bucket of this endpoint.
    pub fn bucket(&self) -> &GridFsBucket {
        &self.cfg.bucket
    }
}

fn file_headers(ex: &mut Exchange, f: &GridFsFile) {
    ex.set_header(headers::FILE_NAME, f.filename.clone());
    ex.set_header(headers::FILE_LENGTH, Value::Int(f.length));
    ex.set_header(headers::FILE_LAST_MODIFIED, Value::Int(f.upload_date));
    ex.set_header(headers::FILE_ID_PRODUCED, f.id_text());
    if let Some(ct) = &f.content_type {
        ex.set_header(headers::CONTENT_TYPE, ct.clone());
    }
    if let Some(m) = &f.metadata {
        ex.set_header(headers::METADATA, m.to_value().to_json());
    }
}

impl Endpoint for GridFsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(GridFsProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let cfg = self.cfg.clone();
        let ctx = self.ctx.clone();
        let since: Arc<Mutex<i64>> = Arc::new(Mutex::new(now_millis()));
        let logger = self.uri.clone();
        Ok(Arc::new(ScheduledPollConsumer::new("mongodb-gridfs", self.poll.clone(), move || {
            let b = &cfg.bucket;
            let mut filter = cfg.query.clone().unwrap_or_default();
            if cfg.strategy != Strategy::FileAttribute {
                filter.insert("uploadDate", Document::new().with("$gt", Bson::DateTime(*since.lock().unwrap())));
            }
            if cfg.strategy != Strategy::TimeStamp {
                filter.insert(format!("metadata.{}", cfg.file_attribute), Document::new().with("$exists", false));
            }
            let files = b.find(filter)?;
            let mut n = 0;
            for f in files {
                if cfg.strategy != Strategy::TimeStamp {
                    b.set_marker(&f.id, &cfg.file_attribute, "processing")?;
                }
                let data = b.download(&f)?;
                let mut ex = Exchange::new(ctx.upgrade());
                file_headers(&mut ex, &f);
                ex.set_body(Value::Bytes(data));
                process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
                if cfg.strategy != Strategy::TimeStamp {
                    b.set_marker(&f.id, &cfg.file_attribute, "done")?;
                }
                let mut s = since.lock().unwrap();
                *s = (*s).max(f.upload_date);
                n += 1;
            }
            Ok(n)
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct GridFsProducer {
    cfg: Config,
}

impl Service for GridFsProducer {}

impl GridFsProducer {
    fn selector(&self, ex: &Exchange) -> Result<Document> {
        if let Some(id) = header_str(ex, headers::OBJECT_ID) {
            let oid = ObjectId::parse(id.trim()).ok_or_else(|| CamelError::illegal_argument(format!("mongodb-gridfs: invalid {} '{}'", headers::OBJECT_ID, id)))?;
            return Ok(Document::new().with("_id", oid));
        }
        let name = header_str(ex, headers::FILE_NAME).ok_or_else(|| CamelError::illegal_argument(format!("mongodb-gridfs: {} or {} header required", headers::FILE_NAME, headers::OBJECT_ID)))?;
        Ok(Document::new().with("filename", name))
    }
}

impl Processor for GridFsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let b = &self.cfg.bucket;
        let op = header_str(ex, headers::OPERATION).unwrap_or_else(|| self.cfg.operation.clone());
        match op.as_str() {
            "create" => {
                let name = header_str(ex, headers::FILE_NAME).ok_or_else(|| CamelError::illegal_argument("mongodb-gridfs create requires the CamelFileName header"))?;
                let data = match ex.body() {
                    Value::Bytes(b) => b.clone(),
                    Value::Null => Vec::new(),
                    other => other.to_string().into_bytes(),
                };
                let chunk = header_i64(ex, headers::CHUNK_SIZE)?.unwrap_or(DEFAULT_CHUNK_SIZE);
                let metadata = match header_str(ex, headers::METADATA) {
                    Some(m) => Some(Document::parse_json(&m)?),
                    None => None,
                };
                let ct = header_str(ex, headers::CONTENT_TYPE);
                let id = b.upload(&name, &data, chunk, ct.as_deref(), metadata)?;
                ex.set_header(headers::FILE_ID_PRODUCED, id.to_hex());
            }
            "remove" => {
                let files = b.find(self.selector(ex)?)?;
                for f in &files {
                    b.delete(&f.id)?;
                }
            }
            "findOne" => {
                let files = b.find(self.selector(ex)?)?;
                let f = files.last().ok_or_else(|| CamelError::new(kinds::MONGO, "mongodb-gridfs: no such file"))?;
                let data = b.download(f)?;
                file_headers(ex, f);
                ex.set_body(Value::Bytes(data));
            }
            "listAll" => {
                let filter = match header_str(ex, headers::FILE_NAME) {
                    Some(n) => Document::new().with("filename", n),
                    None => Document::new(),
                };
                let text: String = b.find(filter)?.iter().map(|f| format!("{}\t{}\n", f.filename, f.id_text())).collect();
                ex.set_body(text);
            }
            "count" => {
                let filter = match header_str(ex, headers::FILE_NAME) {
                    Some(n) => Document::new().with("filename", n),
                    None => Document::new(),
                };
                ex.set_body(Value::Int(b.client.count(&b.database, &b.files(), filter)?));
            }
            other => return Err(CamelError::illegal_argument(format!("mongodb-gridfs: unknown operation '{}'", other))),
        }
        Ok(())
    }
}
