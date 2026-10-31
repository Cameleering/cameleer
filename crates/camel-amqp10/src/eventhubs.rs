//! The `azure-eventhubs` component (Camel `camel-azure-eventhubs`) over AMQP 1.0 with SAS
//! tokens put on the `$cbs` node.
//!
//! ```text
//! azure-eventhubs:namespace/eventHubName?sharedAccessName=..&sharedAccessKey=..
//!     | ?connectionString=Endpoint=sb://ns.servicebus.windows.net/;SharedAccessKeyName=..;SharedAccessKey=..;EntityPath=hub
//!     &consumerGroupName=$Default&partitionId=&partitionKey=&prefetchCount=500
//!     &checkpointStore=#bean&checkpointBatchSize=500&checkpointBatchTimeout=5000
//!     &initialPosition=latest|earliest
//! ```
//!
//! **Producer**: sends the body (a list body sends a batch, one event per element) as a
//! data section to the event hub (round robin), to a partition (`partitionId` /
//! `CamelAzureEventHubsPartitionId`, address `hub/Partitions/N`) or with a partition key
//! (`partitionKey` / `CamelAzureEventHubsPartitionKey`, annotation `x-opt-partition-key`).
//! Non-`Camel` headers become application properties.
//!
//! **Consumer**: discovers the partitions with the `$management` `READ`
//! (`com.microsoft:eventhub`) operation (or uses `partitionId`), and runs one receiver per
//! partition on `hub/ConsumerGroups/<group>/Partitions/<id>` with an
//! `amqp.annotation.x-opt-offset > '<offset>'` filter: the last checkpoint of the
//! partition, else `initialPosition` (rust-camel option; Azure's default `latest`).
//! Checkpoints (offset + sequence number) are written to the [`CheckpointStore`] every
//! `checkpointBatchSize` events or `checkpointBatchTimeout` ms and when the consumer stops.
//! The default store is an [`InMemoryCheckpointStore`] shared by the context
//! (`checkpointStore=#bean` selects another one, e.g. a [`FileCheckpointStore`] with the blob
//! checkpoint store layout). Events carry `CamelAzureEventHubsPartitionId`, `...Offset`,
//! `...SequenceNumber`, `...EnqueuedTime` (epoch millis), `CamelMessageTimestamp`, the
//! partition key and the application properties as headers; the body is the event's bytes.

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::component::support::BackgroundThreads;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::azure::AzureNamespace;
use crate::cbs::RequestResponse;
use crate::client::{Connection, LinkOptions, Sender, Session};
use crate::codec::AmqpValue;
use crate::message::{Body, Message};
use crate::types::SELECTOR_FILTER;
use crate::util::{amqp_failure, to_amqp_simple, to_camel, Opts, COMMON_IGNORED};

/// Header names (Camel `EventHubsConstants`).
pub mod headers {
    pub const PARTITION_ID: &str = "CamelAzureEventHubsPartitionId";
    pub const PARTITION_KEY: &str = "CamelAzureEventHubsPartitionKey";
    pub const OFFSET: &str = "CamelAzureEventHubsOffset";
    pub const ENQUEUED_TIME: &str = "CamelAzureEventHubsEnqueuedTime";
    pub const SEQUENCE_NUMBER: &str = "CamelAzureEventHubsSequenceNumber";
    pub const MESSAGE_TIMESTAMP: &str = "CamelMessageTimestamp";
}

/// A partition checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub offset: i64,
    pub sequence_number: i64,
}

/// Where consumers store their position (Azure `CheckpointStore`). Bind an
/// `Arc<dyn CheckpointStore>` in the registry and reference it with `checkpointStore=#name`.
pub trait CheckpointStore: Send + Sync {
    fn get(&self, namespace: &str, hub: &str, consumer_group: &str, partition: &str) -> Option<Checkpoint>;
    fn update(&self, namespace: &str, hub: &str, consumer_group: &str, partition: &str, checkpoint: Checkpoint) -> Result<()>;
}

/// An in-memory checkpoint store.
#[derive(Default)]
pub struct InMemoryCheckpointStore {
    map: Mutex<HashMap<String, Checkpoint>>,
}

fn store_key(ns: &str, hub: &str, cg: &str, p: &str) -> String {
    format!("{}/{}/{}/checkpoint/{}", ns.to_ascii_lowercase(), hub.to_ascii_lowercase(), cg.to_ascii_lowercase(), p)
}

impl CheckpointStore for InMemoryCheckpointStore {
    fn get(&self, ns: &str, hub: &str, cg: &str, p: &str) -> Option<Checkpoint> {
        self.map.lock().unwrap().get(&store_key(ns, hub, cg, p)).cloned()
    }
    fn update(&self, ns: &str, hub: &str, cg: &str, p: &str, c: Checkpoint) -> Result<()> {
        self.map.lock().unwrap().insert(store_key(ns, hub, cg, p), c);
        Ok(())
    }
}

/// A file based checkpoint store using the blob checkpoint store layout
/// (`<dir>/<namespace>/<hub>/<group>/checkpoint/<partition>` containing `offset` and
/// `sequencenumber` lines, like the blob metadata).
pub struct FileCheckpointStore {
    dir: PathBuf,
}

impl FileCheckpointStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileCheckpointStore { dir: dir.into() }
    }
}

impl CheckpointStore for FileCheckpointStore {
    fn get(&self, ns: &str, hub: &str, cg: &str, p: &str) -> Option<Checkpoint> {
        let text = std::fs::read_to_string(self.dir.join(store_key(ns, hub, cg, p))).ok()?;
        let mut c = Checkpoint { offset: -1, sequence_number: -1 };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("offset", v)) => c.offset = v.trim().parse().ok()?,
                Some(("sequencenumber", v)) => c.sequence_number = v.trim().parse().ok()?,
                _ => {}
            }
        }
        Some(c)
    }
    fn update(&self, ns: &str, hub: &str, cg: &str, p: &str, c: Checkpoint) -> Result<()> {
        let path = self.dir.join(store_key(ns, hub, cg, p));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CamelError::io(e.to_string()))?;
        }
        std::fs::write(&path, format!("offset={}\nsequencenumber={}\n", c.offset, c.sequence_number)).map_err(|e| CamelError::io(e.to_string()))
    }
}

/// Initial position for partitions without a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialPosition {
    Earliest,
    Latest,
}

/// Resolved endpoint configuration.
#[derive(Clone)]
pub struct EventHubsConfig {
    pub ns: AzureNamespace,
    pub hub: String,
    pub consumer_group: String,
    pub partition_id: Option<String>,
    pub partition_key: Option<String>,
    pub prefetch: u32,
    pub store: Arc<dyn CheckpointStore>,
    pub batch_size: u32,
    pub batch_timeout: Duration,
    pub initial: InitialPosition,
    pub recovery_interval: Duration,
}

impl EventHubsConfig {
    fn from_uri(scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<EventHubsConfig> {
        let mut o = Opts::new(ctx, scheme, params);
        let (ns_part, hub_part) = match remaining.trim().trim_matches('/').split_once('/') {
            Some((n, h)) => (Some(n.to_string()), Some(h.to_string())),
            None if remaining.trim().is_empty() => (None, None),
            None => (Some(remaining.trim().to_string()), None),
        };
        let (ns, entity) = AzureNamespace::resolve(ctx, scheme, &mut o, ns_part.as_deref())?;
        let hub = hub_part.filter(|h| !h.is_empty()).or(entity).ok_or_else(|| CamelError::illegal_argument("azure-eventhubs: the event hub name must be specified (namespace/eventHubName or EntityPath)"))?;
        let consumer_group = o.string("consumerGroupName").unwrap_or_else(|| "$Default".into());
        let partition_id = o.string("partitionId");
        let partition_key = o.string("partitionKey");
        let prefetch = o.num("prefetchCount", 500u32)?.max(1);
        let store: Arc<dyn CheckpointStore> = match o.string("checkpointStore") {
            Some(r) => {
                let name = r.trim_start_matches('#');
                ctx.registry()
                    .lookup::<Arc<dyn CheckpointStore>>(name)
                    .map(|a| (*a).clone())
                    .ok_or_else(|| CamelError::new(camel_core::error::kinds::NO_SUCH_BEAN, format!("No CheckpointStore bean named {}", name)))?
            }
            None => default_store(ctx),
        };
        if o.string("blobContainerName").is_some() || o.string("blobAccountName").is_some() || o.string("blobAccessKey").is_some() {
            return Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "Blob checkpoint options are not supported: bind a CheckpointStore (e.g. FileCheckpointStore) and use checkpointStore=#bean"));
        }
        let batch_size = o.num("checkpointBatchSize", 500u32)?.max(1);
        let batch_timeout = o.millis("checkpointBatchTimeout", Duration::from_millis(5000))?;
        let initial = match o.get("initialPosition").map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("latest") => InitialPosition::Latest,
            Some("earliest") => InitialPosition::Earliest,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid initialPosition (earliest or latest): {}", other))),
        };
        let recovery_interval = o.millis("recoveryInterval", Duration::from_millis(5000))?;
        o.ignore(COMMON_IGNORED);
        o.ignore(&["eventPosition", "producerAsyncClient", "checkpointUpdater"]);
        Ok(EventHubsConfig { ns, hub, consumer_group, partition_id, partition_key, prefetch, store, batch_size, batch_timeout, initial, recovery_interval })
    }
}

fn default_store(ctx: &CamelContext) -> Arc<dyn CheckpointStore> {
    const NAME: &str = "camel-amqp10-default-checkpoint-store";
    // created once per context (race-free enough: the first lookup wins on bind)
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();
    if let Some(s) = ctx.registry().lookup::<Arc<dyn CheckpointStore>>(NAME) {
        return (*s).clone();
    }
    let s: Arc<dyn CheckpointStore> = Arc::new(InMemoryCheckpointStore::default());
    ctx.registry().bind(NAME, s.clone());
    s
}

/// The `azure-eventhubs` component.
#[derive(Default)]
pub struct EventHubsComponent;

impl Component for EventHubsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let cfg = EventHubsConfig::from_uri("azure-eventhubs", ctx, remaining, params)?;
        Ok(Arc::new(EventHubsEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `azure-eventhubs` endpoint.
pub struct EventHubsEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<EventHubsConfig>,
}

impl Endpoint for EventHubsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(EventHubsProducer { cfg: self.cfg.clone(), state: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(EventHubsConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct ProducerState {
    conn: Connection,
    session: Session,
    senders: HashMap<String, Sender>,
}

struct EventHubsProducer {
    cfg: Arc<EventHubsConfig>,
    state: Mutex<Option<ProducerState>>,
}

fn hstr(ex: &Exchange, k: &str) -> Option<String> {
    ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

fn event(ex: &Exchange, body: &Value, partition_key: Option<&str>) -> Message {
    let bytes = match body {
        Value::Bytes(b) => b.clone(),
        Value::Null => Vec::new(),
        Value::Object(_) | Value::Array(_) => body.to_json().into_bytes(),
        other => other.to_string().into_bytes(),
    };
    let mut m = Message::data(bytes);
    if let Some(k) = partition_key {
        m.set_annotation("x-opt-partition-key", AmqpValue::str(k));
    }
    for (k, v) in ex.message.headers.iter() {
        if k.to_ascii_lowercase().starts_with("camel") || v.is_null() {
            continue;
        }
        m.set_app_property(k, to_amqp_simple(v));
    }
    m
}

impl Service for EventHubsProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.state.lock().unwrap().take() {
            s.conn.close();
        }
        Ok(())
    }
}

impl Processor for EventHubsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let partition = hstr(ex, headers::PARTITION_ID).or_else(|| cfg.partition_id.clone());
        let key = hstr(ex, headers::PARTITION_KEY).or_else(|| cfg.partition_key.clone());
        let address = match &partition {
            Some(p) => format!("{}/Partitions/{}", cfg.hub, p),
            None => cfg.hub.clone(),
        };
        let events: Vec<Message> = match ex.body() {
            Value::Array(items) => items.iter().map(|b| event(ex, b, key.as_deref())).collect(),
            b => vec![event(ex, b, key.as_deref())],
        };
        let mut g = self.state.lock().unwrap();
        if g.as_ref().is_some_and(|s| !s.conn.is_open()) {
            if let Some(s) = g.take() {
                s.conn.close();
            }
        }
        if g.is_none() {
            let (conn, session) = cfg.ns.connect(&[&cfg.hub])?;
            *g = Some(ProducerState { conn, session, senders: HashMap::new() });
        }
        let s = g.as_mut().unwrap();
        if !s.senders.contains_key(&address) {
            let snd = s.session.sender(LinkOptions::new(&address))?;
            s.senders.insert(address.clone(), snd);
        }
        let snd = &s.senders[&address];
        for e in &events {
            snd.send_accepted(e, cfg.ns.request_timeout)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

/// Read the partition ids of an event hub through `$management`.
pub fn partition_ids(ns: &AzureNamespace, session: &Session, hub: &str) -> Result<Vec<String>> {
    let mgmt = ns.management(session, "$management")?;
    let mut req = Message::default();
    req.set_app_property("operation", AmqpValue::str("READ"));
    req.set_app_property("name", AmqpValue::str(hub));
    req.set_app_property("type", AmqpValue::str("com.microsoft:eventhub"));
    let r = mgmt.request(req);
    mgmt.close();
    let resp = r?;
    let (code, desc) = RequestResponse::status(&resp);
    if code != 200 {
        return Err(amqp_failure(format!("Event hub {} metadata request failed: {} {}", hub, code, desc)));
    }
    Ok(match &resp.body {
        Body::Value(v) => v.map_get("partition_ids").and_then(|p| p.as_list()).unwrap_or(&[]).iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    })
}

struct EventHubsConsumer {
    cfg: Arc<EventHubsConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

struct PartitionProgress {
    last: Option<Checkpoint>,
    since_checkpoint: u32,
    last_checkpoint: Instant,
}

impl EventHubsConsumer {
    fn checkpoint(&self, partition: &str, p: &mut PartitionProgress, force: bool) {
        let cfg = &self.cfg;
        let Some(c) = p.last.clone() else { return };
        if p.since_checkpoint == 0 {
            return;
        }
        if force || p.since_checkpoint >= cfg.batch_size || p.last_checkpoint.elapsed() >= cfg.batch_timeout {
            if let Err(e) = cfg.store.update(&cfg.ns.host, &cfg.hub, &cfg.consumer_group, partition, c) {
                camel_core::camel_warn!(&self.uri, "Cannot update checkpoint of partition {}: {}", partition, e.message);
            }
            p.since_checkpoint = 0;
            p.last_checkpoint = Instant::now();
        }
    }

    fn run(&self, stop: &Arc<camel_core::component::support::StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
        let cfg = &self.cfg;
        let progress: Mutex<HashMap<String, PartitionProgress>> = Mutex::new(HashMap::new());
        while !stop.is_stopped() {
            let setup = (|| -> Result<(Connection, Vec<(String, crate::client::Receiver)>)> {
                let (conn, session) = cfg.ns.connect(&[&cfg.hub])?;
                let partitions = match &cfg.partition_id {
                    Some(p) => vec![p.clone()],
                    None => partition_ids(&cfg.ns, &session, &cfg.hub)?,
                };
                let mut rxs = Vec::new();
                for p in partitions {
                    let resume = progress.lock().unwrap().get(&p).and_then(|x| x.last.clone()).or_else(|| cfg.store.get(&cfg.ns.host, &cfg.hub, &cfg.consumer_group, &p));
                    let filter = match (resume, cfg.initial) {
                        (Some(c), _) => format!("amqp.annotation.x-opt-offset > '{}'", c.offset),
                        (None, InitialPosition::Earliest) => "amqp.annotation.x-opt-offset > '-1'".to_string(),
                        (None, InitialPosition::Latest) => "amqp.annotation.x-opt-offset > '@latest'".to_string(),
                    };
                    let address = format!("{}/ConsumerGroups/{}/Partitions/{}", cfg.hub, cfg.consumer_group, p);
                    let o = LinkOptions::new(&address).credit(cfg.prefetch).at_most_once().filter("apache.org:selector-filter:string", SELECTOR_FILTER, AmqpValue::str(filter));
                    rxs.push((p, session.receiver(o)?));
                }
                Ok((conn, rxs))
            })();
            let (conn, rxs) = match setup {
                Ok(x) => x,
                Err(e) => {
                    camel_core::camel_warn!(&self.uri, "Cannot start consumer on {}: {} - retrying in {} ms", self.uri, e.message, cfg.recovery_interval.as_millis());
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Err(e));
                    }
                    if stop.sleep(cfg.recovery_interval) {
                        return;
                    }
                    continue;
                }
            };
            if let Some(tx) = ready.take() {
                let _ = tx.send(Ok(()));
            }
            std::thread::scope(|scope| {
                for (pid, rx) in &rxs {
                    let st = stop.clone();
                    let progress = &progress;
                    scope.spawn(move || {
                        loop {
                            if st.is_stopped() {
                                break;
                            }
                            match rx.receive(Duration::from_millis(50)) {
                                Ok(Some(d)) => {
                                    let m = &d.message;
                                    let mut ex = Exchange::new(self.ctx.upgrade());
                                    ex.set_body(m.body_bytes().map(Value::Bytes).unwrap_or(Value::Null));
                                    for (k, v) in &m.application_properties {
                                        ex.set_header(k.clone(), to_camel(v));
                                    }
                                    let offset = m.annotation("x-opt-offset").and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()).or(v.as_i64()));
                                    let seq = m.annotation("x-opt-sequence-number").and_then(|v| v.as_i64());
                                    ex.set_header(headers::PARTITION_ID, pid.clone());
                                    if let Some(o) = offset {
                                        ex.set_header(headers::OFFSET, o.to_string());
                                    }
                                    if let Some(s) = seq {
                                        ex.set_header(headers::SEQUENCE_NUMBER, s);
                                    }
                                    if let Some(t) = m.annotation("x-opt-enqueued-time").and_then(|v| v.as_i64()) {
                                        ex.set_header(headers::ENQUEUED_TIME, t);
                                        ex.set_header(headers::MESSAGE_TIMESTAMP, t);
                                    }
                                    if let Some(k) = m.annotation("x-opt-partition-key").and_then(|v| v.as_str()) {
                                        ex.set_header(headers::PARTITION_KEY, k.to_string());
                                    }
                                    if let Err(e) = self.processor.process(&mut ex) {
                                        ex.set_exception(e);
                                    }
                                    if let Some(e) = &ex.exception {
                                        camel_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
                                    }
                                    let mut g = progress.lock().unwrap();
                                    let p = g.entry(pid.clone()).or_insert_with(|| PartitionProgress { last: None, since_checkpoint: 0, last_checkpoint: Instant::now() });
                                    if let (Some(o), Some(s)) = (offset, seq) {
                                        p.last = Some(Checkpoint { offset: o, sequence_number: s });
                                        p.since_checkpoint += 1;
                                    }
                                    self.checkpoint(pid, p, false);
                                }
                                Ok(None) => {
                                    let mut g = progress.lock().unwrap();
                                    if let Some(p) = g.get_mut(pid) {
                                        self.checkpoint(pid, p, false);
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    });
                }
            });
            {
                let mut g = progress.lock().unwrap();
                for (pid, p) in g.iter_mut() {
                    self.checkpoint(pid, p, true);
                }
            }
            let lost = !stop.is_stopped();
            conn.close();
            if lost {
                camel_core::camel_warn!(&self.uri, "Connection lost on {} - reconnecting in {} ms", self.uri, cfg.recovery_interval.as_millis());
                if stop.sleep(cfg.recovery_interval) {
                    return;
                }
            }
        }
    }
}

impl Service for EventHubsConsumer {
    fn start(&self) -> Result<()> {
        if self.threads.is_running() {
            return Ok(());
        }
        let me = EventHubsConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor: self.processor.clone(), threads: BackgroundThreads::new() };
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        self.threads.spawn(&format!("Camel {} consumer", self.uri), move |stop| {
            let mut ready = Some(tx);
            me.run(&stop, &mut ready);
        })?;
        if let Ok(Err(e)) = rx.recv_timeout(self.cfg.ns.request_timeout + Duration::from_secs(10)) {
            camel_core::camel_warn!(&self.uri, "Consumer {} not connected yet: {}", self.uri, e.message);
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_stores() {
        let m = InMemoryCheckpointStore::default();
        assert!(m.get("ns", "hub", "$Default", "0").is_none());
        m.update("ns", "hub", "$Default", "0", Checkpoint { offset: 10, sequence_number: 2 }).unwrap();
        assert_eq!(m.get("NS", "hub", "$default", "0"), Some(Checkpoint { offset: 10, sequence_number: 2 }));
        let dir = std::env::temp_dir().join(format!("camel-amqp10-ckpt-{}-{}", std::process::id(), crate::util::unique_id()));
        let f = FileCheckpointStore::new(&dir);
        f.update("ns", "hub", "$Default", "1", Checkpoint { offset: 5, sequence_number: 1 }).unwrap();
        assert_eq!(f.get("ns", "hub", "$Default", "1"), Some(Checkpoint { offset: 5, sequence_number: 1 }));
        assert!(dir.join("ns/hub/$default/checkpoint/1").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
