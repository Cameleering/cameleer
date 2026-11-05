//! Azure Key Vault **secret refresh** (Camel's `EventhubsReloadTriggerTask`): with
//! `camel.vault.azure.refreshEnabled=true` a context service consumes the Key Vault events that
//! Event Grid forwards to an Event Hub (`camel.vault.azure.eventhubConnectionString`, with
//! `EntityPath`; checkpoints in Azure Blob when `camel.vault.azure.blobAccountName`,
//! `blobContainerName` and `blobAccessKey` (+ Cameleer `blobEndpoint`) are set, in memory
//! otherwise) through cameleer-amqp10's `azure-eventhubs` consumer in an internal context. Event
//! Grid events (`eventType` `Microsoft.KeyVault.SecretNewVersionCreated`, `subject` /
//! `data.ObjectName` = the secret name; one event or an array per message) of the secrets named
//! by `camel.vault.azure.secrets` (comma separated names or `*` patterns; when empty, the
//! secrets resolved through `{{azure:...}}`) are collected, and every
//! `camel.vault.azure.refreshPeriod` ms (default 30000) pending updates trigger a context
//! reload: reload listeners ([`AzureSecretsRefreshTask::add_reload_listener`]) get the secret
//! names and every started route is restarted, so `{{azure:...}}` placeholders resolve again.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Service;
use cameleer_core::value::Value;

/// Registry bean name of the [`AzureSecretsRefreshTask`].
pub const REFRESH_TASK_BEAN: &str = "cameleer-cloud-azure-secrets-refresh-task";
/// Registry bean holding the names of the secrets resolved through `{{azure:...}}`.
pub(crate) const USED_SECRETS_BEAN: &str = "cameleer-cloud-azure-vault-used-secrets";
/// The Event Grid event type of a new secret version.
pub const SECRET_NEW_VERSION: &str = "Microsoft.KeyVault.SecretNewVersionCreated";

/// The secrets resolved through `{{azure:...}}` in a context.
pub(crate) fn used_secrets(ctx: &CamelContext) -> Arc<Mutex<BTreeSet<String>>> {
    if let Some(s) = ctx.registry().lookup::<Mutex<BTreeSet<String>>>(USED_SECRETS_BEAN) {
        return s;
    }
    let s = Arc::new(Mutex::new(BTreeSet::new()));
    ctx.registry().bind_arc(USED_SECRETS_BEAN, s.clone());
    s
}

type Listener = Arc<dyn Fn(&[String]) + Send + Sync>;

/// The Event Hubs driven secret refresh of a context (see the module docs).
pub struct AzureSecretsRefreshTask {
    ctx: WeakContext,
    running: Arc<(Mutex<bool>, Condvar)>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    consumer: Mutex<Option<CamelContext>>,
    pending: Arc<Mutex<Vec<String>>>,
    listeners: Mutex<Vec<Listener>>,
    reloads: AtomicUsize,
    enabled: AtomicBool,
}

/// The refresh task of a context (registered by [`crate::register`]).
pub fn refresh_task(ctx: &CamelContext) -> Option<Arc<AzureSecretsRefreshTask>> {
    ctx.registry().lookup::<AzureSecretsRefreshTask>(REFRESH_TASK_BEAN)
}

/// Secret names of the Key Vault "new version" events of an Event Grid message body.
pub fn secret_events(body: &Value) -> Vec<String> {
    let v = match body {
        Value::Bytes(b) => crate::util::parse_json(b).unwrap_or_default(),
        Value::String(s) => crate::util::parse_json(s.as_bytes()).unwrap_or_default(),
        other => other.clone(),
    };
    let events = match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    events
        .iter()
        .filter(|e| e.get("eventType").or_else(|| e.get("type")).map(|t| t.to_string()).as_deref() == Some(SECRET_NEW_VERSION))
        .filter_map(|e| e.get("data").and_then(|d| d.get("ObjectName")).or_else(|| e.get("subject")).map(|n| n.to_string()))
        .filter(|n| !n.is_empty())
        .collect()
}

impl AzureSecretsRefreshTask {
    /// Create the task of a context and register it as bean and service.
    pub fn install(ctx: &CamelContext) -> Arc<AzureSecretsRefreshTask> {
        if let Some(t) = refresh_task(ctx) {
            return t;
        }
        let t = Arc::new(AzureSecretsRefreshTask {
            ctx: ctx.downgrade(),
            running: Arc::new((Mutex::new(false), Condvar::new())),
            thread: Mutex::new(None),
            consumer: Mutex::new(None),
            pending: Arc::new(Mutex::new(Vec::new())),
            listeners: Mutex::new(Vec::new()),
            reloads: AtomicUsize::new(0),
            enabled: AtomicBool::new(false),
        });
        ctx.registry().bind_arc(REFRESH_TASK_BEAN, t.clone());
        let _ = ctx.add_service(Arc::new(RefreshService(t.clone())));
        t
    }

    /// Register a listener called on every reload with the updated secret names.
    pub fn add_reload_listener(&self, l: impl Fn(&[String]) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Arc::new(l));
    }

    /// Number of reloads triggered so far.
    pub fn reload_count(&self) -> usize {
        self.reloads.load(Ordering::SeqCst)
    }

    /// Whether the refresh is running.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Secret updates received and not reloaded yet.
    pub fn pending_updates(&self) -> Vec<String> {
        self.pending.lock().unwrap().clone()
    }

    fn prop(ctx: &CamelContext, k: &str) -> Option<String> {
        ctx.properties().get(&format!("camel.vault.azure.{}", k)).filter(|v| !v.trim().is_empty())
    }

    fn matches(ctx: &CamelContext, name: &str) -> bool {
        match Self::prop(ctx, "secrets") {
            Some(p) => p.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| cameleer_core::util::matches_pattern(name, p)),
            None => {
                let used = used_secrets(ctx);
                let used = used.lock().unwrap();
                used.is_empty() || used.contains(name)
            }
        }
    }

    /// Reload now when secret updates are pending; returns the reloaded secret names.
    pub fn check_now(&self) -> Result<Vec<String>> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let updated: Vec<String> = std::mem::take(&mut *self.pending.lock().unwrap());
        if updated.is_empty() {
            return Ok(updated);
        }
        cameleer_core::camel_info!("azure-key-vault", "Secrets {:?} updated: reloading the CamelContext routes", updated);
        let listeners = self.listeners.lock().unwrap().clone();
        for l in listeners {
            l(&updated);
        }
        for id in ctx.route_ids() {
            if ctx.route_status(&id).is_some_and(|s| s == cameleer_core::context::ServiceStatus::Started) {
                ctx.stop_route(&id)?;
                ctx.start_route(&id)?;
            }
        }
        // counted once the reload is done (listeners called, routes restarted)
        self.reloads.fetch_add(1, Ordering::SeqCst);
        Ok(updated)
    }

    fn start(self: &Arc<Self>) -> Result<()> {
        let Some(ctx) = self.ctx.upgrade() else { return Ok(()) };
        if !Self::prop(&ctx, "refreshEnabled").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Ok(());
        }
        let cs = Self::prop(&ctx, "eventhubConnectionString").ok_or_else(|| CamelError::illegal_argument("camel.vault.azure.refreshEnabled requires camel.vault.azure.eventhubConnectionString"))?;
        let period = Self::prop(&ctx, "refreshPeriod").map(|p| p.parse::<u64>().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.vault.azure.refreshPeriod: {}", p)))).transpose()?.unwrap_or(30_000).max(10);
        let mut uri = format!("azure-eventhubs:?connectionString=RAW({})&initialPosition=latest&checkpointBatchSize=1", cs);
        if let (Some(a), Some(c), Some(k)) = (Self::prop(&ctx, "blobAccountName"), Self::prop(&ctx, "blobContainerName"), Self::prop(&ctx, "blobAccessKey")) {
            uri.push_str(&format!("&blobAccountName={}&blobContainerName={}&blobAccessKey=RAW({})", a, c, k));
            if let Some(e) = Self::prop(&ctx, "blobEndpoint") {
                uri.push_str(&format!("&blobEndpoint={}", e));
            }
        }
        // the Event Hubs consumer runs in an internal context (its route is not reloaded)
        let inner = CamelContext::new();
        cameleer_amqp10::register(&inner);
        super::blob_checkpoint::register_factory(&inner);
        let (pending, weak) = (self.pending.clone(), ctx.downgrade());
        inner.add_routes(move |r| {
            r.from(&uri).route_id("azure-vault-refresh").process(move |ex: &mut Exchange| {
                let Some(ctx) = weak.upgrade() else { return Ok(()) };
                for name in secret_events(ex.body()) {
                    if Self::matches(&ctx, &name) {
                        let mut p = pending.lock().unwrap();
                        if !p.contains(&name) {
                            p.push(name);
                        }
                    }
                }
                Ok(())
            });
        })?;
        inner.start()?;
        *self.consumer.lock().unwrap() = Some(inner);
        *self.running.0.lock().unwrap() = true;
        self.enabled.store(true, Ordering::SeqCst);
        let me = self.clone();
        let h = std::thread::Builder::new().name("azure-secrets-refresh".into()).spawn(move || loop {
            {
                let (lock, cv) = &*me.running;
                let guard = lock.lock().unwrap();
                let (guard, _) = cv.wait_timeout_while(guard, Duration::from_millis(period), |r| *r).unwrap();
                if !*guard {
                    return;
                }
            }
            if let Err(e) = me.check_now() {
                cameleer_core::camel_warn!("azure-key-vault", "Secret refresh failed: {}", e);
            }
        });
        *self.thread.lock().unwrap() = h.ok();
        Ok(())
    }

    fn stop(&self) {
        *self.running.0.lock().unwrap() = false;
        self.running.1.notify_all();
        self.enabled.store(false, Ordering::SeqCst);
        if let Some(inner) = self.consumer.lock().unwrap().take() {
            let _ = inner.stop();
        }
        if let Some(h) = self.thread.lock().unwrap().take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

struct RefreshService(Arc<AzureSecretsRefreshTask>);

impl Service for RefreshService {
    fn start(&self) -> Result<()> {
        self.0.start()
    }
    fn stop(&self) -> Result<()> {
        self.0.stop();
        Ok(())
    }
}
