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
//!
//! **Start positions**: the checkpoint wins, else `eventPosition` (a map per partition:
//! `eventPosition=0=earliest,1=offset:42,2=sequenceNumber:7:inclusive,3=enqueuedTime:<millis>`
//! or `#bean` with a `HashMap<String, EventPosition>` / object value), else
//! `initialPosition`.
//!
//! **Load balancing**: without `partitionId`, consumer instances sharing a checkpoint
//! store that keeps [`PartitionOwnership`] records (in-memory, file, Azure Blob through
//! camel-cloud when it implements ownership) balance the partitions like the Azure SDK's
//! event processor (balanced strategy): every `loadBalancingUpdateInterval` (rust-camel,
//! default 10 s) an instance (`ownerId`, default random) renews its claims with optimistic
//! etags, gives back partitions above its share, claims unowned or expired ones
//! (`partitionOwnershipExpirationInterval`, default 60 s) and steals from instances above
//! their share. Receivers of owned partitions attach with epoch (`com.microsoft:epoch`)
//! `ownerLevel` (default 0), so a new owner disconnects the previous receiver; stopping
//! relinquishes the claims. Stores without ownership (or `loadBalancing=false`) consume
//! every partition, with the `ownerLevel` epoch when set.

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::component::support::{BackgroundThreads, StopFlag};

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
use crate::util::{amqp_failure, now_millis, to_amqp_simple, to_camel, Opts, COMMON_IGNORED};

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

/// A partition ownership record of the load balancing protocol (Azure SDK
/// `PartitionOwnership`): who owns a partition, when the claim was last renewed and the
/// etag that makes claims optimistic (a claim succeeds only with the current etag; a new
/// record is claimed with `etag: None`). An empty `owner_id` means relinquished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionOwnership {
    pub namespace: String,
    pub hub: String,
    pub consumer_group: String,
    pub partition_id: String,
    pub owner_id: String,
    /// Epoch millis of the last claim.
    pub last_modified: i64,
    pub etag: Option<String>,
}

/// Where consumers store their position (Azure `CheckpointStore`). Bind an
/// `Arc<dyn CheckpointStore>` in the registry and reference it with `checkpointStore=#name`.
/// Stores that also keep [`PartitionOwnership`] records enable load balancing between
/// consumer instances; the default methods report that ownership is not supported.
pub trait CheckpointStore: Send + Sync {
    fn get(&self, namespace: &str, hub: &str, consumer_group: &str, partition: &str) -> Option<Checkpoint>;
    fn update(&self, namespace: &str, hub: &str, consumer_group: &str, partition: &str, checkpoint: Checkpoint) -> Result<()>;
    /// All ownership records of a consumer group.
    fn list_ownership(&self, _namespace: &str, _hub: &str, _consumer_group: &str) -> Result<Vec<PartitionOwnership>> {
        Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "this checkpoint store keeps no partition ownership"))
    }
    /// Claim ownerships: each succeeds when its etag matches the stored one (or it is new
    /// and `etag` is `None`). Returns the successful claims with their new etag and time.
    fn claim_ownership(&self, _claims: &[PartitionOwnership]) -> Result<Vec<PartitionOwnership>> {
        Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "this checkpoint store keeps no partition ownership"))
    }
}

static ETAGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn new_etag() -> String {
    format!("\"0x{:X}{:04X}\"", now_millis(), ETAGS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) & 0xffff)
}

fn ownership_key(ns: &str, hub: &str, cg: &str, p: &str) -> String {
    format!("{}/{}/{}/ownership/{}", ns.to_ascii_lowercase(), hub.to_ascii_lowercase(), cg.to_ascii_lowercase(), p)
}

/// Where a partition without checkpoint starts (Azure `EventPosition`), per partition with
/// the `eventPosition` option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventPosition {
    Earliest,
    Latest,
    Offset { offset: i64, inclusive: bool },
    SequenceNumber { sequence_number: i64, inclusive: bool },
    /// Events enqueued after this time (epoch millis).
    EnqueuedTime(i64),
}

impl EventPosition {
    /// Parse `earliest`, `latest`, `offset:N[:inclusive]`, `sequenceNumber:N[:inclusive]`,
    /// `enqueuedTime:<epoch millis or ISO-8601>`.
    pub fn parse(s: &str) -> Result<EventPosition> {
        let s = s.trim();
        let bad = || CamelError::illegal_argument(format!("Invalid event position: {}", s));
        let mut parts = s.splitn(3, ':');
        let kind = parts.next().unwrap_or("").to_ascii_lowercase();
        let value = parts.next();
        let inclusive = parts.next().is_some_and(|f| f.eq_ignore_ascii_case("inclusive"));
        let num = |v: Option<&str>| v.and_then(|v| v.trim().parse::<i64>().ok()).ok_or_else(bad);
        Ok(match kind.as_str() {
            "earliest" => EventPosition::Earliest,
            "latest" => EventPosition::Latest,
            "offset" => EventPosition::Offset { offset: num(value)?, inclusive },
            "sequencenumber" | "sequence" => EventPosition::SequenceNumber { sequence_number: num(value)?, inclusive },
            "enqueuedtime" | "time" => {
                let rest = s.split_once(':').map(|x| x.1).unwrap_or("");
                EventPosition::EnqueuedTime(crate::servicebus::parse_time(&Value::String(rest.to_string())).ok_or_else(bad)?)
            }
            _ => return Err(bad()),
        })
    }

    /// The Event Hubs receiver filter expression.
    pub fn filter(&self) -> String {
        let op = |inclusive: bool| if inclusive { ">=" } else { ">" };
        match self {
            EventPosition::Earliest => "amqp.annotation.x-opt-offset > '-1'".to_string(),
            EventPosition::Latest => "amqp.annotation.x-opt-offset > '@latest'".to_string(),
            EventPosition::Offset { offset, inclusive } => format!("amqp.annotation.x-opt-offset {} '{}'", op(*inclusive), offset),
            EventPosition::SequenceNumber { sequence_number, inclusive } => format!("amqp.annotation.x-opt-sequence-number {} '{}'", op(*inclusive), sequence_number),
            EventPosition::EnqueuedTime(t) => format!("amqp.annotation.x-opt-enqueued-time > '{}'", t),
        }
    }
}

/// Parse the `eventPosition` option: `0=earliest,1=offset:42,2=sequenceNumber:7:inclusive`
/// (`;` also separates entries).
pub fn parse_event_positions(s: &str) -> Result<HashMap<String, EventPosition>> {
    let mut m = HashMap::new();
    for entry in s.split([',', ';']).map(str::trim).filter(|e| !e.is_empty()) {
        let (p, pos) = entry.split_once('=').ok_or_else(|| CamelError::illegal_argument(format!("Invalid eventPosition entry (partition=position): {}", entry)))?;
        m.insert(p.trim().to_string(), EventPosition::parse(pos)?);
    }
    Ok(m)
}

/// Options of an Azure Blob checkpoint store (`blobAccountName`, `blobContainerName`,
/// `blobAccessKey`, and the rust-camel `blobEndpoint` override, e.g. an Azurite URL).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlobCheckpointOptions {
    pub account_name: String,
    pub container_name: String,
    pub access_key: Option<String>,
    pub endpoint: Option<String>,
}

/// Creates a blob checkpoint store from the endpoint's `blob*` options. Another crate
/// (camel-cloud) binds one in the registry under [`BLOB_CHECKPOINT_STORE_FACTORY`].
pub type BlobCheckpointStoreFactory = Arc<dyn Fn(&CamelContext, &BlobCheckpointOptions) -> Result<Arc<dyn CheckpointStore>> + Send + Sync>;

/// Registry name of the [`BlobCheckpointStoreFactory`].
pub const BLOB_CHECKPOINT_STORE_FACTORY: &str = "camel-amqp10-blob-checkpoint-store-factory";

/// An in-memory checkpoint store (checkpoints and partition ownership; share one
/// instance between consumers to balance them within a process).
#[derive(Default)]
pub struct InMemoryCheckpointStore {
    map: Mutex<HashMap<String, Checkpoint>>,
    ownership: Mutex<HashMap<String, PartitionOwnership>>,
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
    fn list_ownership(&self, ns: &str, hub: &str, cg: &str) -> Result<Vec<PartitionOwnership>> {
        let prefix = ownership_key(ns, hub, cg, "");
        let mut v: Vec<PartitionOwnership> = self.ownership.lock().unwrap().iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(_, o)| o.clone()).collect();
        v.sort_by(|a, b| a.partition_id.cmp(&b.partition_id));
        Ok(v)
    }
    fn claim_ownership(&self, claims: &[PartitionOwnership]) -> Result<Vec<PartitionOwnership>> {
        let mut map = self.ownership.lock().unwrap();
        let mut out = Vec::new();
        for c in claims {
            let key = ownership_key(&c.namespace, &c.hub, &c.consumer_group, &c.partition_id);
            let current = map.get(&key).and_then(|o| o.etag.clone());
            if current == c.etag {
                let o = PartitionOwnership { last_modified: now_millis(), etag: Some(new_etag()), ..c.clone() };
                map.insert(key, o.clone());
                out.push(o);
            }
        }
        Ok(out)
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
    /// Ownership records are files `<dir>/<ns>/<hub>/<group>/ownership/<partition>` with
    /// `ownerid`, `etag` and `modified` lines (the blob store layout with the metadata as
    /// lines). Claims are atomic within a process and use write-then-rename on disk.
    fn list_ownership(&self, ns: &str, hub: &str, cg: &str) -> Result<Vec<PartitionOwnership>> {
        let dir = self.dir.join(ownership_key(ns, hub, cg, ""));
        let mut v = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(v) };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if let Some(o) = self.read_ownership(ns, hub, cg, &name) {
                v.push(o);
            }
        }
        v.sort_by(|a, b| a.partition_id.cmp(&b.partition_id));
        Ok(v)
    }
    fn claim_ownership(&self, claims: &[PartitionOwnership]) -> Result<Vec<PartitionOwnership>> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let mut out = Vec::new();
        for c in claims {
            let current = self.read_ownership(&c.namespace, &c.hub, &c.consumer_group, &c.partition_id).and_then(|o| o.etag);
            if current != c.etag {
                continue;
            }
            let o = PartitionOwnership { last_modified: now_millis(), etag: Some(new_etag()), ..c.clone() };
            let path = self.dir.join(ownership_key(&c.namespace, &c.hub, &c.consumer_group, &c.partition_id));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CamelError::io(e.to_string()))?;
            }
            let tmp = path.with_file_name(format!(".{}.tmp", c.partition_id));
            std::fs::write(&tmp, format!("ownerid={}\netag={}\nmodified={}\n", o.owner_id, o.etag.as_deref().unwrap_or(""), o.last_modified)).map_err(|e| CamelError::io(e.to_string()))?;
            std::fs::rename(&tmp, &path).map_err(|e| CamelError::io(e.to_string()))?;
            out.push(o);
        }
        Ok(out)
    }
}

impl FileCheckpointStore {
    fn read_ownership(&self, ns: &str, hub: &str, cg: &str, p: &str) -> Option<PartitionOwnership> {
        let text = std::fs::read_to_string(self.dir.join(ownership_key(ns, hub, cg, p))).ok()?;
        let mut o = PartitionOwnership { namespace: ns.to_string(), hub: hub.to_string(), consumer_group: cg.to_string(), partition_id: p.to_string(), owner_id: String::new(), last_modified: 0, etag: None };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("ownerid", v)) => o.owner_id = v.trim().to_string(),
                Some(("etag", v)) => o.etag = Some(v.trim().to_string()).filter(|e| !e.is_empty()),
                Some(("modified", v)) => o.last_modified = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
        Some(o)
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
    /// Start positions per partition (`eventPosition`), used without a checkpoint.
    pub event_positions: HashMap<String, EventPosition>,
    /// Balance partitions between consumers sharing the checkpoint store (`loadBalancing`,
    /// default true without `partitionId`).
    pub load_balancing: bool,
    /// Interval of load balancing rounds (`loadBalancingUpdateInterval`, default 10 s).
    pub load_balancing_interval: Duration,
    /// Claims not renewed for this long are expired (`partitionOwnershipExpirationInterval`,
    /// default 60 s).
    pub ownership_expiration: Duration,
    /// Our owner id in ownership records (`ownerId`, default a random id).
    pub owner_id: String,
    /// Receiver epoch (`ownerLevel`); load balanced receivers use 0 unless set.
    pub owner_level: Option<i64>,
}

impl EventHubsConfig {
    fn from_uri(scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<EventHubsConfig> {
        let mut o = Opts::new(ctx, scheme, params);
        let (ns_part, hub_part) = match remaining.trim().trim_matches('/').split_once('/') {
            Some((n, h)) => (Some(n.to_string()), Some(h.to_string())),
            None if remaining.trim().is_empty() => (None, None),
            None => (Some(remaining.trim().to_string()), None),
        };
        let (ns, entity) = AzureNamespace::resolve(ctx, scheme, &mut o, ns_part.as_deref(), crate::identity::EVENT_HUBS_SCOPE)?;
        let hub = hub_part.filter(|h| !h.is_empty()).or(entity).ok_or_else(|| CamelError::illegal_argument("azure-eventhubs: the event hub name must be specified (namespace/eventHubName or EntityPath)"))?;
        let consumer_group = o.string("consumerGroupName").unwrap_or_else(|| "$Default".into());
        let partition_id = o.string("partitionId");
        let partition_key = o.string("partitionKey");
        let prefetch = o.num("prefetchCount", 500u32)?.max(1);
        let blob = BlobCheckpointOptions {
            account_name: o.string("blobAccountName").unwrap_or_default(),
            container_name: o.string("blobContainerName").unwrap_or_default(),
            access_key: o.string("blobAccessKey"),
            endpoint: o.string("blobEndpoint"),
        };
        o.ignore(&["blobStorageSharedKeyCredential"]);
        let store: Arc<dyn CheckpointStore> = match o.string("checkpointStore") {
            Some(r) => {
                let name = r.trim_start_matches('#');
                ctx.registry()
                    .lookup::<Arc<dyn CheckpointStore>>(name)
                    .map(|a| (*a).clone())
                    .ok_or_else(|| CamelError::new(camel_core::error::kinds::NO_SUCH_BEAN, format!("No CheckpointStore bean named {}", name)))?
            }
            None if blob != BlobCheckpointOptions::default() => {
                if blob.account_name.is_empty() || blob.container_name.is_empty() {
                    return Err(CamelError::illegal_argument("The blob checkpoint store needs both blobAccountName and blobContainerName"));
                }
                let factory = ctx.registry().lookup::<BlobCheckpointStoreFactory>(BLOB_CHECKPOINT_STORE_FACTORY).ok_or_else(|| {
                    CamelError::new(
                        camel_core::error::kinds::UNSUPPORTED_OPERATION,
                        "Blob checkpoint options need the Azure Blob checkpoint store of camel-cloud (camel_cloud::register), or bind a CheckpointStore and use checkpointStore=#bean",
                    )
                })?;
                factory(ctx, &blob)?
            }
            None => default_store(ctx),
        };
        let batch_size = o.num("checkpointBatchSize", 500u32)?.max(1);
        let batch_timeout = o.millis("checkpointBatchTimeout", Duration::from_millis(5000))?;
        let initial = match o.get("initialPosition").map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("latest") => InitialPosition::Latest,
            Some("earliest") => InitialPosition::Earliest,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid initialPosition (earliest or latest): {}", other))),
        };
        let recovery_interval = o.millis("recoveryInterval", Duration::from_millis(5000))?;
        let event_positions = match o.string("eventPosition") {
            None => HashMap::new(),
            Some(r) if r.starts_with('#') => {
                let name = r.trim_start_matches('#');
                if let Some(m) = ctx.registry().lookup::<HashMap<String, EventPosition>>(name) {
                    (*m).clone()
                } else if let Some(Value::Object(m)) = ctx.registry().lookup::<Value>(name).map(|v| (*v).clone()) {
                    m.iter().map(|(k, v)| EventPosition::parse(&v.to_string()).map(|p| (k.clone(), p))).collect::<Result<HashMap<_, _>>>()?
                } else {
                    return Err(CamelError::new(camel_core::error::kinds::NO_SUCH_BEAN, format!("No eventPosition map bean named {}", name)));
                }
            }
            Some(s) => parse_event_positions(&s)?,
        };
        let load_balancing = o.bool("loadBalancing", true)?;
        let load_balancing_interval = o.millis("loadBalancingUpdateInterval", Duration::from_secs(10))?.max(Duration::from_millis(10));
        let ownership_expiration = o.millis("partitionOwnershipExpirationInterval", Duration::from_secs(60))?;
        let owner_id = o.string("ownerId").unwrap_or_else(|| crate::message::format_uuid(&crate::message::random_uuid()));
        let owner_level = match o.string("ownerLevel") {
            Some(s) => Some(s.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid ownerLevel: {}", s)))?),
            None => None,
        };
        o.ignore(COMMON_IGNORED);
        o.ignore(&["producerAsyncClient", "checkpointUpdater"]);
        Ok(EventHubsConfig {
            ns,
            hub,
            consumer_group,
            partition_id,
            partition_key,
            prefetch,
            store,
            batch_size,
            batch_timeout,
            initial,
            recovery_interval,
            event_positions,
            load_balancing,
            load_balancing_interval,
            ownership_expiration,
            owner_id,
            owner_level,
        })
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

/// What partition workers share.
struct Shared {
    cfg: Arc<EventHubsConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    progress: Mutex<HashMap<String, PartitionProgress>>,
}

/// A running partition receiver.
struct PartitionWorker {
    stop: Arc<StopFlag>,
    handle: std::thread::JoinHandle<()>,
}

impl Shared {
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

    /// The receiver filter of a partition: last processed position, else the checkpoint,
    /// else `eventPosition[partition]`, else `initialPosition`.
    fn start_filter(&self, partition: &str) -> String {
        let cfg = &self.cfg;
        let resume = self.progress.lock().unwrap().get(partition).and_then(|x| x.last.clone()).or_else(|| cfg.store.get(&cfg.ns.host, &cfg.hub, &cfg.consumer_group, partition));
        let position = match resume {
            Some(c) => EventPosition::Offset { offset: c.offset, inclusive: false },
            None => cfg.event_positions.get(partition).cloned().unwrap_or(match cfg.initial {
                InitialPosition::Earliest => EventPosition::Earliest,
                InitialPosition::Latest => EventPosition::Latest,
            }),
        };
        position.filter()
    }

    fn process_event(&self, pid: &str, d: crate::client::Delivery) {
        let m = &d.message;
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.set_body(m.body_bytes().map(Value::Bytes).unwrap_or(Value::Null));
        for (k, v) in &m.application_properties {
            ex.set_header(k.clone(), to_camel(v));
        }
        let offset = m.annotation("x-opt-offset").and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()).or(v.as_i64()));
        let seq = m.annotation("x-opt-sequence-number").and_then(|v| v.as_i64());
        ex.set_header(headers::PARTITION_ID, pid.to_string());
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
        let mut g = self.progress.lock().unwrap();
        let p = g.entry(pid.to_string()).or_insert_with(|| PartitionProgress { last: None, since_checkpoint: 0, last_checkpoint: Instant::now() });
        if let (Some(o), Some(s)) = (offset, seq) {
            p.last = Some(Checkpoint { offset: o, sequence_number: s });
            p.since_checkpoint += 1;
        }
        self.checkpoint(pid, p, false);
    }

    /// Receive one partition until stopped or the link is lost (e.g. stolen by a
    /// receiver with a higher epoch); checkpoints when it ends.
    fn run_partition(self: &Arc<Self>, session: &Session, pid: &str, epoch: Option<i64>, stop: &StopFlag) {
        let cfg = &self.cfg;
        let address = format!("{}/ConsumerGroups/{}/Partitions/{}", cfg.hub, cfg.consumer_group, pid);
        let mut o = LinkOptions::new(&address).credit(cfg.prefetch).at_most_once().filter("apache.org:selector-filter:string", SELECTOR_FILTER, AmqpValue::str(self.start_filter(pid)));
        if let Some(e) = epoch {
            o = o.property(crate::azure::EPOCH_PROPERTY, AmqpValue::Long(e));
        }
        let rx = match session.receiver(o) {
            Ok(rx) => rx,
            Err(e) => {
                camel_core::camel_warn!(&self.uri, "Cannot receive from partition {}: {}", pid, e.message);
                return;
            }
        };
        loop {
            if stop.is_stopped() {
                break;
            }
            match rx.receive(Duration::from_millis(50)) {
                Ok(Some(d)) => self.process_event(pid, d),
                Ok(None) => {
                    let mut g = self.progress.lock().unwrap();
                    if let Some(p) = g.get_mut(pid) {
                        self.checkpoint(pid, p, false);
                    }
                }
                Err(e) => {
                    if !stop.is_stopped() && session.is_open() {
                        camel_core::camel_info!(&self.uri, "Partition {} receiver closed: {}", pid, e.message);
                    }
                    break;
                }
            }
        }
        if stop.is_stopped() {
            rx.close();
        }
        let mut g = self.progress.lock().unwrap();
        if let Some(p) = g.get_mut(pid) {
            self.checkpoint(pid, p, true);
        }
        // a new owner resumes from the checkpoint store, not from our memory
        g.remove(pid);
    }

    fn spawn_partition(self: &Arc<Self>, session: &Session, pid: &str, epoch: Option<i64>) -> Result<PartitionWorker> {
        let stop = StopFlag::new();
        let (me, session, pid2, st) = (self.clone(), session.clone(), pid.to_string(), stop.clone());
        let handle = std::thread::Builder::new()
            .name(format!("Camel {} partition {}", self.uri, pid))
            .spawn(move || me.run_partition(&session, &pid2, epoch, &st))
            .map_err(|e| CamelError::io(e.to_string()))?;
        Ok(PartitionWorker { stop, handle })
    }

    /// One load balancing round (balanced strategy over the checkpoint store's ownership
    /// records): renew our claims, give back partitions above our share, claim unowned
    /// or expired ones and, if still below our share, steal from owners above theirs.
    /// Returns the partitions we own afterwards.
    fn balance(&self, partitions: &[String]) -> Result<Vec<String>> {
        let cfg = &self.cfg;
        let now = now_millis();
        let all = cfg.store.list_ownership(&cfg.ns.host, &cfg.hub, &cfg.consumer_group)?;
        let expiry = cfg.ownership_expiration.as_millis() as i64;
        let active: Vec<&PartitionOwnership> = all.iter().filter(|o| !o.owner_id.is_empty() && now - o.last_modified < expiry && partitions.contains(&o.partition_id)).collect();
        let mut owners: Vec<String> = active.iter().map(|o| o.owner_id.clone()).collect();
        owners.push(cfg.owner_id.clone());
        owners.sort();
        owners.dedup();
        let n = partitions.len();
        let k = owners.len().max(1);
        let target_of = |owner: &str| {
            let idx = owners.iter().position(|o| o == owner).unwrap_or(0);
            n / k + usize::from(idx < n % k)
        };
        let target = target_of(&cfg.owner_id);
        let mut mine: Vec<PartitionOwnership> = active.iter().filter(|o| o.owner_id == cfg.owner_id).map(|o| (*o).clone()).collect();
        mine.sort_by(|a, b| a.partition_id.cmp(&b.partition_id));
        let mut claims: Vec<PartitionOwnership> = Vec::new();
        for (i, o) in mine.iter().enumerate() {
            let mut c = o.clone();
            if i >= target {
                c.owner_id = String::new();
            }
            claims.push(c);
        }
        let mut need = target.saturating_sub(mine.len());
        let record = |p: &str| {
            all.iter().find(|o| o.partition_id == p).cloned().unwrap_or(PartitionOwnership {
                namespace: cfg.ns.host.clone(),
                hub: cfg.hub.clone(),
                consumer_group: cfg.consumer_group.clone(),
                partition_id: p.to_string(),
                owner_id: String::new(),
                last_modified: 0,
                etag: None,
            })
        };
        for p in partitions {
            if need == 0 {
                break;
            }
            if !active.iter().any(|o| o.partition_id == *p) {
                let mut c = record(p);
                c.owner_id = cfg.owner_id.clone();
                claims.push(c);
                need -= 1;
            }
        }
        if need > 0 {
            for owner in owners.iter().filter(|o| **o != cfg.owner_id) {
                let theirs: Vec<&&PartitionOwnership> = active.iter().filter(|o| o.owner_id == *owner).collect();
                let surplus = theirs.len().saturating_sub(target_of(owner));
                for o in theirs.into_iter().take(surplus.min(need)) {
                    let mut c = (**o).clone();
                    c.owner_id = cfg.owner_id.clone();
                    claims.push(c);
                    need -= 1;
                }
                if need == 0 {
                    break;
                }
            }
        }
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        let claimed = cfg.store.claim_ownership(&claims)?;
        Ok(claimed.into_iter().filter(|o| o.owner_id == cfg.owner_id).map(|o| o.partition_id).collect())
    }

    /// Give our partitions back when stopping, so other processors take over at once.
    fn relinquish(&self) {
        let cfg = &self.cfg;
        let Ok(all) = cfg.store.list_ownership(&cfg.ns.host, &cfg.hub, &cfg.consumer_group) else { return };
        let mine: Vec<PartitionOwnership> = all
            .into_iter()
            .filter(|o| o.owner_id == cfg.owner_id)
            .map(|mut o| {
                o.owner_id = String::new();
                o
            })
            .collect();
        if !mine.is_empty() {
            let _ = cfg.store.claim_ownership(&mine);
        }
    }
}

fn stop_workers(workers: &mut HashMap<String, PartitionWorker>) {
    for w in workers.values() {
        w.stop.stop();
    }
    for (_, w) in workers.drain() {
        let _ = w.handle.join();
    }
}

impl EventHubsConsumer {
    fn run(&self, stop: &Arc<StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
        let cfg = &self.cfg;
        let shared = Arc::new(Shared { cfg: cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor: self.processor.clone(), progress: Mutex::new(HashMap::new()) });
        let mut balancing = cfg.load_balancing && cfg.partition_id.is_none();
        while !stop.is_stopped() {
            let setup = (|| -> Result<(Connection, Session, Vec<String>)> {
                let (conn, session) = cfg.ns.connect(&[&cfg.hub])?;
                let partitions = match &cfg.partition_id {
                    Some(p) => vec![p.clone()],
                    None => partition_ids(&cfg.ns, &session, &cfg.hub)?,
                };
                Ok((conn, session, partitions))
            })();
            let (conn, session, partitions) = match setup {
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
            let mut workers: HashMap<String, PartitionWorker> = HashMap::new();
            if !balancing {
                for p in &partitions {
                    match shared.spawn_partition(&session, p, cfg.owner_level) {
                        Ok(w) => {
                            workers.insert(p.clone(), w);
                        }
                        Err(e) => camel_core::camel_warn!(&self.uri, "Cannot start partition {}: {}", p, e.message),
                    }
                }
            }
            let mut next_round = Instant::now();
            while !stop.is_stopped() && session.is_open() {
                workers.retain(|_, w| !w.handle.is_finished());
                if balancing && Instant::now() >= next_round {
                    next_round = Instant::now() + cfg.load_balancing_interval;
                    match shared.balance(&partitions) {
                        Ok(owned) => {
                            let lost: Vec<String> = workers.keys().filter(|p| !owned.contains(p)).cloned().collect();
                            for p in lost {
                                if let Some(w) = workers.remove(&p) {
                                    w.stop.stop();
                                    let _ = w.handle.join();
                                }
                            }
                            for p in owned {
                                if !workers.contains_key(&p) {
                                    // owner level 0: a new owner disconnects the previous one
                                    match shared.spawn_partition(&session, &p, Some(cfg.owner_level.unwrap_or(0))) {
                                        Ok(w) => {
                                            workers.insert(p, w);
                                        }
                                        Err(e) => camel_core::camel_warn!(&self.uri, "Cannot start partition {}: {}", p, e.message),
                                    }
                                }
                            }
                        }
                        Err(e) if e.kind == camel_core::error::kinds::UNSUPPORTED_OPERATION => {
                            camel_core::camel_warn!(&self.uri, "The checkpoint store does not support partition ownership ({}) - consuming every partition without load balancing", e.message);
                            balancing = false;
                            for p in &partitions {
                                if let Ok(w) = shared.spawn_partition(&session, p, cfg.owner_level) {
                                    workers.insert(p.clone(), w);
                                }
                            }
                        }
                        Err(e) => camel_core::camel_warn!(&self.uri, "Load balancing round failed: {}", e.message),
                    }
                } else if !balancing && workers.is_empty() {
                    // every receiver ended (e.g. stolen): reconnect after the recovery interval
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            stop_workers(&mut workers);
            if balancing {
                shared.relinquish();
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
