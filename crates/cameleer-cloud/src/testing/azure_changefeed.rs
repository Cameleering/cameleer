//! Blob change feed emulation for [`AzuriteMock`]: once enabled
//! ([`AzuriteMock::enable_change_feed`]), every blob write / delete is recorded as a
//! `BlobCreated` / `BlobDeleted` event and the `$blobchangefeed` container is rebuilt with
//! `meta/segments.json`, hourly `idx/segments/.../meta.json` manifests and Avro object container
//! chunk files (`log/00/YYYY/MM/DD/hh00/00000.avro`), the layout the Azure SDK reads.

use std::collections::BTreeMap;

use cameleer_core::value::{ObjectMap, Value};
use cameleer_dataformats::avro::{write_container, AvroSchema, Codec};

use super::{new_blob, AzuriteMock, Container, State, ACCOUNT};
use crate::util::{iso8601_millis, now_millis, now_secs, obj, uuid};

const CONTAINER: &str = "$blobchangefeed";

const SCHEMA: &str = r#"{"type":"record","name":"BlobChangeEvent","namespace":"com.microsoft.storage.blobchangefeed","fields":[
 {"name":"schemaVersion","type":"int"},{"name":"topic","type":"string"},{"name":"subject","type":"string"},
 {"name":"eventType","type":"string"},{"name":"eventTime","type":"string"},{"name":"id","type":"string"},
 {"name":"data","type":{"type":"record","name":"BlobChangeEventData","fields":[
  {"name":"api","type":"string"},{"name":"clientRequestId","type":"string"},{"name":"requestId","type":"string"},
  {"name":"etag","type":"string"},{"name":"contentType","type":"string"},{"name":"contentLength","type":"long"},
  {"name":"blobType","type":"string"},{"name":"url","type":"string"},{"name":"sequencer","type":"string"}]}}]}"#;

impl AzuriteMock {
    /// Enable the blob change feed of the account (records changes from now on).
    pub fn enable_change_feed(&self) {
        let mut s = self.state.lock().unwrap();
        s.change_feed = true;
        rebuild(&mut s);
    }
    /// Number of change feed events recorded.
    pub fn change_feed_events(&self) -> usize {
        self.state.lock().unwrap().feed_events.len()
    }
}

/// Record a change of `container/blob` (`etag` None = deleted).
pub(super) fn record(s: &mut State, container: &str, blob: &str, api: &str, etag: Option<String>, content_type: &str, len: usize, blob_type: &str) {
    if !s.change_feed || container == CONTAINER {
        return;
    }
    let t = now_millis();
    let seq = s.feed_events.len() + 1;
    let ev = obj([
        ("schemaVersion", Value::Int(3)),
        ("topic", Value::from(format!("/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg/providers/Microsoft.Storage/storageAccounts/{}", ACCOUNT))),
        ("subject", Value::from(format!("/blobServices/default/containers/{}/blobs/{}", container, blob))),
        ("eventType", Value::from(if etag.is_some() { "BlobCreated" } else { "BlobDeleted" })),
        ("eventTime", Value::from(iso8601_millis(t))),
        ("id", Value::from(uuid())),
        (
            "data",
            obj([
                ("api", Value::from(api)),
                ("clientRequestId", Value::from(uuid())),
                ("requestId", Value::from(uuid())),
                ("etag", Value::from(etag.unwrap_or_default())),
                ("contentType", Value::from(content_type)),
                ("contentLength", Value::Int(len as i64)),
                ("blobType", Value::from(blob_type)),
                ("url", Value::from(format!("https://{}.blob.core.windows.net/{}/{}", ACCOUNT, container, blob))),
                ("sequencer", Value::from(format!("{:032x}", seq))),
            ]),
        ),
    ]);
    s.feed_events.push((t, ev));
    rebuild(s);
}

fn rebuild(s: &mut State) {
    let schema = AvroSchema::parse(SCHEMA).expect("change feed schema");
    let mut c = Container { created: now_secs(), ..Default::default() };
    let mut hours: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
    for (t, e) in &s.feed_events {
        hours.entry(t.div_euclid(3_600_000)).or_default().push(e.clone());
    }
    let put = |c: &mut Container, name: String, data: Vec<u8>, ct: &str| {
        c.blobs.insert(name, new_blob(data, ct.to_string(), vec![]));
    };
    for (h, events) in hours {
        let (y, mo, d, hh, _, _) = cameleer_core::util_time::civil_from_epoch(h * 3600);
        let dir = format!("{:04}/{:02}/{:02}/{:02}00", y, mo, d, hh);
        let chunk_dir = format!("log/00/{}/", dir);
        let data = write_container(&schema, &events, Codec::Deflate, 4096).expect("avro chunk");
        put(&mut c, format!("{}00000.avro", chunk_dir), data, "avro/binary");
        let mut meta = ObjectMap::new();
        meta.insert("version".into(), Value::Int(0));
        meta.insert("begin".into(), Value::from(iso8601_millis(h * 3_600_000)));
        meta.insert("intervalSecs".into(), Value::Int(3600));
        meta.insert("status".into(), Value::from("Finalized"));
        meta.insert("chunkFilePaths".into(), Value::Array(vec![Value::from(format!("{}/{}", CONTAINER, chunk_dir))]));
        put(&mut c, format!("idx/segments/{}/meta.json", dir), Value::Object(meta).to_json().into_bytes(), "application/json");
    }
    let segments = obj([("version", Value::Int(0)), ("lastConsumable", Value::from(iso8601_millis(now_millis() + 1000)))]);
    put(&mut c, "meta/segments.json".to_string(), segments.to_json().into_bytes(), "application/json");
    s.containers.insert(CONTAINER.to_string(), c);
}
