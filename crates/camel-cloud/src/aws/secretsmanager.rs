//! The `aws-secrets-manager` component (Camel `camel-aws-secrets-manager`, producer) over the
//! Secrets Manager AWS JSON 1.1 protocol, and the `{{aws:...}}` properties function.
//!
//! `aws-secrets-manager://label?operation=...&binaryPayload=false`
//!
//! Operations (`operation` / `CamelAwsSecretsManagerOperation`, default `getSecret`):
//! * `createSecret`: name `CamelAwsSecretsManagerSecretName`, value = body (string, or bytes
//!   as `SecretBinary` with `binaryPayload=true`), `CamelAwsSecretsManagerSecretDescription`;
//!   body = secret ARN, `CamelAwsSecretsManagerSecretVersionId`.
//! * `getSecret`: id `CamelAwsSecretsManagerSecretId` (name or ARN), optional
//!   `CamelAwsSecretsManagerSecretVersionId`; body = secret string (bytes for binary secrets).
//! * `describeSecret`, `listSecrets` (body = list of secret entries), `deleteSecret` (body =
//!   ARN; `CamelAwsSecretsManagerForceDeletion` for `ForceDeleteWithoutRecovery`),
//!   `restoreSecret`, `updateSecret` (body = new value), `rotateSecret` (rotation lambda
//!   `CamelAwsSecretsManagerLambdaRotationFunctionArn`, `CamelAwsSecretsManagerRotateImmediately`),
//!   `replicateSecretToRegions` (`CamelAwsSecretsManagerSecretReplicationRegions`).
//!
//! Properties function (registered by [`crate::register`]): `{{aws:name}}`,
//! `{{aws:name/field}}` (field of a JSON secret), `{{aws:name@version}}` and
//! `{{aws:name/field:default}}`. Configuration from the context properties
//! `camel.vault.aws.accessKey`, `secretKey`, `region`, `defaultCredentialsProvider`,
//! `profileCredentialsProvider`, `profileName`, and (as in recent Camel versions)
//! `uriEndpointOverride` + `overrideEndpoint`.
//!
//! **Secret refresh** ([`SecretsRefreshTask`], Camel's `CloudTrailReloadTriggerTask`): with
//! `camel.vault.aws.refreshEnabled=true` a context service polls CloudTrail `LookupEvents`
//! (event source `secretsmanager.amazonaws.com`) every `camel.vault.aws.refreshPeriod` ms
//! (default 30000) for `PutSecretValue` / `UpdateSecret` / `RotateSecret` /
//! `UpdateSecretVersionStage` / `RestoreSecret` events on the secrets named by
//! `camel.vault.aws.secrets` (comma separated names or patterns, `*` wildcards; when empty, the
//! secrets resolved through `{{aws:...}}`). An update triggers a context reload: the registered
//! reload listeners ([`SecretsRefreshTask::add_reload_listener`]) are called with the updated
//! secret names and every started route is restarted (`RouteStopped` / `RouteStarted` events).
//! The task is a context bean named [`REFRESH_TASK_BEAN`] ([`refresh_task`]).

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, PropertiesFunction, WeakContext};
use camel_core::spi::Service;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{error_code, AwsConfig};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, header_list, header_str, kinds, obj, parse_json, Opts};

/// Header names (Camel `SecretsManagerConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsSecretsManagerOperation";
    pub const SECRET_NAME: &str = "CamelAwsSecretsManagerSecretName";
    pub const SECRET_DESCRIPTION: &str = "CamelAwsSecretsManagerSecretDescription";
    pub const SECRET_ID: &str = "CamelAwsSecretsManagerSecretId";
    pub const SECRET_VERSION_ID: &str = "CamelAwsSecretsManagerSecretVersionId";
    pub const LAMBDA_ROTATION_FUNCTION_ARN: &str = "CamelAwsSecretsManagerLambdaRotationFunctionArn";
    pub const SECRET_REPLICATION_REGIONS: &str = "CamelAwsSecretsManagerSecretReplicationRegions";
    /// rust-camel extensions.
    pub const FORCE_DELETION: &str = "CamelAwsSecretsManagerForceDeletion";
    pub const ROTATE_IMMEDIATELY: &str = "CamelAwsSecretsManagerRotateImmediately";
}

/// A Secrets Manager client.
pub struct SecretsManagerService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub binary_payload: bool,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["secretsManagerClient", "pojoRequest"]);
    Ok(Arc::new(SecretsManagerService { operation: o.get("operation"), binary_payload: o.bool("binaryPayload", false)?, aws }))
}

impl SecretsManagerService {
    /// Invoke a Secrets Manager API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("secretsmanager", "secretsmanager", "secretsmanager", action, &body, "1.1", kinds::SECRETS_MANAGER)
    }

    /// `GetSecretValue`: the secret string, or the decoded binary.
    pub fn get_secret_value(&self, id: &str, version: Option<&str>) -> Result<Value> {
        let mut m = ObjectMap::new();
        m.insert("SecretId".to_string(), Value::from(id));
        if let Some(v) = version {
            m.insert("VersionId".to_string(), Value::from(v));
        }
        let r = self.call("GetSecretValue", Value::Object(m))?;
        if let Some(s) = r.get("SecretString").filter(|s| !s.is_null()) {
            return Ok(Value::from(s.to_string()));
        }
        Ok(Value::Bytes(r.get("SecretBinary").and_then(|b| base64_decode(&b.to_string())).unwrap_or_default()))
    }

    fn secret_id(ex: &Exchange) -> Result<String> {
        header_str(ex, headers::SECRET_ID)
            .or_else(|| header_str(ex, headers::SECRET_NAME))
            .ok_or_else(|| CamelError::illegal_argument("Secret id must be specified (CamelAwsSecretsManagerSecretId)"))
    }

    fn payload(&self, ex: &Exchange, m: &mut ObjectMap) {
        if self.binary_payload {
            m.insert("SecretBinary".into(), Value::from(base64_encode(&ex.body().to_bytes())));
        } else {
            m.insert("SecretString".into(), Value::from(ex.body().to_string()));
        }
    }
}

impl CloudService for SecretsManagerService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "getSecret");
        match op.as_str() {
            "createSecret" => {
                let name = header_str(ex, headers::SECRET_NAME).ok_or_else(|| CamelError::illegal_argument("createSecret requires the CamelAwsSecretsManagerSecretName header"))?;
                let mut m = ObjectMap::new();
                m.insert("Name".to_string(), Value::from(name));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                if let Some(d) = header_str(ex, headers::SECRET_DESCRIPTION) {
                    m.insert("Description".into(), Value::from(d));
                }
                self.payload(ex, &mut m);
                let r = self.call("CreateSecret", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "getSecret" => {
                let id = Self::secret_id(ex)?;
                let version = header_str(ex, headers::SECRET_VERSION_ID);
                let v = self.get_secret_value(&id, version.as_deref())?;
                ex.set_body(v);
            }
            "describeSecret" => {
                let r = self.call("DescribeSecret", obj([("SecretId", Value::from(Self::secret_id(ex)?))]))?;
                ex.set_body(r);
            }
            "listSecrets" => {
                let mut all = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let req = match &token {
                        Some(t) => obj([("NextToken", Value::from(t.clone()))]),
                        None => obj([]),
                    };
                    let r = self.call("ListSecrets", req)?;
                    all.extend(r.get("SecretList").and_then(|l| l.as_array()).cloned().unwrap_or_default());
                    token = r.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
                    if token.is_none() {
                        break;
                    }
                }
                ex.set_body(Value::Array(all));
            }
            "deleteSecret" => {
                let mut m = ObjectMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                if ex.header(headers::FORCE_DELETION).is_some_and(|v| v.is_truthy()) {
                    m.insert("ForceDeleteWithoutRecovery".into(), Value::Bool(true));
                }
                let r = self.call("DeleteSecret", Value::Object(m))?;
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "restoreSecret" => {
                let r = self.call("RestoreSecret", obj([("SecretId", Value::from(Self::secret_id(ex)?))]))?;
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "updateSecret" => {
                let mut m = ObjectMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                self.payload(ex, &mut m);
                let r = self.call("PutSecretValue", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "rotateSecret" => {
                let mut m = ObjectMap::new();
                m.insert("SecretId".to_string(), Value::from(Self::secret_id(ex)?));
                m.insert("ClientRequestToken".to_string(), Value::from(crate::util::uuid()));
                if let Some(arn) = header_str(ex, headers::LAMBDA_ROTATION_FUNCTION_ARN) {
                    m.insert("RotationLambdaARN".into(), Value::from(arn));
                }
                if let Some(v) = ex.header(headers::ROTATE_IMMEDIATELY) {
                    m.insert("RotateImmediately".into(), Value::Bool(v.is_truthy()));
                }
                let r = self.call("RotateSecret", Value::Object(m))?;
                ex.set_header(headers::SECRET_VERSION_ID, r.get("VersionId").cloned().unwrap_or_default());
                ex.set_body(r.get("ARN").cloned().unwrap_or_default());
            }
            "replicateSecretToRegions" => {
                let regions: Vec<Value> = header_list(ex, headers::SECRET_REPLICATION_REGIONS).into_iter().map(|r| obj([("Region", Value::from(r))])).collect();
                let r = self.call("ReplicateSecretToRegions", obj([("SecretId", Value::from(Self::secret_id(ex)?)), ("AddReplicaRegions", Value::Array(regions))]))?;
                ex.set_body(r);
            }
            other => return Err(unsupported("aws-secrets-manager", other)),
        }
        Ok(())
    }
}

/// Split a vault reference `name[/field][@version]` into its parts.
pub fn parse_secret_ref(r: &str) -> (String, Option<String>, Option<String>) {
    let (rest, version) = match r.split_once('@') {
        Some((a, v)) => (a, Some(v.to_string()).filter(|v| !v.is_empty())),
        None => (r, None),
    };
    match rest.split_once('/') {
        Some((n, f)) => (n.to_string(), Some(f.to_string()).filter(|f| !f.is_empty()), version),
        None => (rest.to_string(), None, version),
    }
}

/// Select `field` of a JSON secret (the whole secret when `field` is None).
pub fn secret_field(secret: &str, field: Option<&str>) -> Option<String> {
    match field {
        None => Some(secret.to_string()),
        Some(f) => {
            let v = parse_json(secret.as_bytes()).ok()?;
            let x = v.get(f)?;
            Some(if matches!(x, Value::Object(_) | Value::Array(_)) { x.to_json() } else { x.to_string() })
        }
    }
}

fn vault_config(ctx: &CamelContext) -> Result<AwsConfig> {
    let p = ctx.properties();
    let get = |k: &str| p.get(&format!("camel.vault.aws.{}", k)).filter(|v| !v.is_empty());
    let flag = |k: &str| get(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let mut cfg = AwsConfig {
        access_key: get("accessKey"),
        secret_key: get("secretKey"),
        session_token: get("sessionToken"),
        region: get("region").or_else(|| std::env::var("AWS_REGION").ok().filter(|r| !r.is_empty())).unwrap_or_else(|| "us-east-1".into()),
        use_default_credentials_provider: flag("defaultCredentialsProvider"),
        use_profile_credentials_provider: flag("profileCredentialsProvider"),
        profile_credentials_name: get("profileName"),
        uri_endpoint_override: get("uriEndpointOverride"),
        override_endpoint: flag("overrideEndpoint"),
        ..Default::default()
    };
    if cfg.uri_endpoint_override.is_some() && get("overrideEndpoint").is_none() {
        cfg.override_endpoint = true;
    }
    Ok(cfg)
}

/// Registry bean holding the names of the secrets resolved through `{{aws:...}}`.
const USED_SECRETS_BEAN: &str = "camel-cloud-aws-vault-used-secrets";
/// Registry bean name of the [`SecretsRefreshTask`].
pub const REFRESH_TASK_BEAN: &str = "camel-cloud-aws-secrets-refresh-task";

fn used_secrets(ctx: &CamelContext) -> Arc<Mutex<BTreeSet<String>>> {
    if let Some(s) = ctx.registry().lookup::<Mutex<BTreeSet<String>>>(USED_SECRETS_BEAN) {
        return s;
    }
    let s = Arc::new(Mutex::new(BTreeSet::new()));
    ctx.registry().bind_arc(USED_SECRETS_BEAN, s.clone());
    s
}

/// The `aws:` properties function for a context.
pub fn properties_function(ctx: &CamelContext) -> PropertiesFunction {
    let weak = ctx.downgrade();
    Arc::new(move |r: &str| {
        let ctx = weak.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let (name, field, version) = parse_secret_ref(r);
        let svc = SecretsManagerService { aws: vault_config(&ctx)?, operation: None, binary_payload: false };
        match svc.get_secret_value(&name, version.as_deref()) {
            Ok(v) => {
                used_secrets(&ctx).lock().unwrap().insert(name.clone());
                Ok(secret_field(&v.to_string(), field.as_deref()))
            }
            Err(e) if error_code(&e).as_deref() == Some("ResourceNotFoundException") => Ok(None),
            Err(e) => Err(e),
        }
    })
}

/// Called with the names of the updated secrets when a reload is triggered.
pub type ReloadListener = Arc<dyn Fn(&[String]) + Send + Sync>;

/// CloudTrail event names that change a secret value.
pub const UPDATE_EVENTS: &[&str] = &["PutSecretValue", "UpdateSecret", "RotateSecret", "UpdateSecretVersionStage", "RestoreSecret"];

/// Periodic secret refresh triggering a context reload (see the module docs).
pub struct SecretsRefreshTask {
    ctx: WeakContext,
    running: Arc<(Mutex<bool>, Condvar)>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    since: Mutex<Option<f64>>,
    seen: Mutex<HashSet<String>>,
    listeners: Mutex<Vec<ReloadListener>>,
    reloads: AtomicUsize,
    enabled: AtomicBool,
}

/// The refresh task of a context (registered by [`crate::register`]).
pub fn refresh_task(ctx: &CamelContext) -> Option<Arc<SecretsRefreshTask>> {
    ctx.registry().lookup::<SecretsRefreshTask>(REFRESH_TASK_BEAN)
}

/// Name of a secret from a secret id (name or ARN `...:secret:name-AbCdEf`).
pub fn secret_name_of(id: &str) -> String {
    match id.split(":secret:").nth(1) {
        Some(rest) => match rest.rsplit_once('-') {
            Some((n, suffix)) if suffix.len() == 6 => n.to_string(),
            _ => rest.to_string(),
        },
        None => id.to_string(),
    }
}

impl SecretsRefreshTask {
    /// Create the task of a context and register it as bean and service.
    pub fn install(ctx: &CamelContext) -> Arc<SecretsRefreshTask> {
        if let Some(t) = refresh_task(ctx) {
            return t;
        }
        let t = Arc::new(SecretsRefreshTask {
            ctx: ctx.downgrade(),
            running: Arc::new((Mutex::new(false), Condvar::new())),
            thread: Mutex::new(None),
            since: Mutex::new(None),
            seen: Mutex::new(HashSet::new()),
            listeners: Mutex::new(Vec::new()),
            reloads: AtomicUsize::new(0),
            enabled: AtomicBool::new(false),
        });
        ctx.registry().bind_arc(REFRESH_TASK_BEAN, t.clone());
        let _ = ctx.add_service(Arc::new(RefreshService(t.clone())));
        t
    }

    /// Register a listener called on every reload.
    pub fn add_reload_listener(&self, l: impl Fn(&[String]) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Arc::new(l));
    }

    /// Number of reloads triggered so far.
    pub fn reload_count(&self) -> usize {
        self.reloads.load(Ordering::SeqCst)
    }

    /// Whether the periodic refresh is running.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn prop(ctx: &CamelContext, k: &str) -> Option<String> {
        ctx.properties().get(&format!("camel.vault.aws.{}", k)).filter(|v| !v.trim().is_empty())
    }

    fn matches(&self, ctx: &CamelContext, name: &str) -> bool {
        match Self::prop(ctx, "secrets") {
            Some(p) => p.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| camel_core::util::matches_pattern(name, p)),
            None => {
                let used = used_secrets(ctx);
                let used = used.lock().unwrap();
                used.is_empty() || used.contains(name)
            }
        }
    }

    /// One refresh check: look for secret updates since the previous check and reload when
    /// one of the watched secrets changed. Returns the updated secret names.
    pub fn check_now(&self) -> Result<Vec<String>> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let aws = vault_config(&ctx)?;
        let since = *self.since.lock().unwrap();
        let mut req = ObjectMap::new();
        req.insert("LookupAttributes".into(), Value::Array(vec![obj([("AttributeKey", Value::from("EventSource")), ("AttributeValue", Value::from("secretsmanager.amazonaws.com"))])]));
        if let Some(t) = since {
            req.insert("StartTime".into(), Value::Float(t));
        }
        let api = super::JsonApi { host: "cloudtrail", service: "cloudtrail", target: "com.amazonaws.cloudtrail.v20131101.CloudTrail_20131101", version: "1.1", kind: kinds::CLOUDTRAIL };
        let events = aws.api_paged(&api, "LookupEvents", req, "Events", "NextToken")?;
        let mut updated: Vec<String> = Vec::new();
        let mut newest = since.unwrap_or(0.0);
        for e in &events {
            let id = e.get("EventId").map(|i| i.to_string()).unwrap_or_default();
            if !self.seen.lock().unwrap().insert(id) {
                continue;
            }
            let t = e.get("EventTime").and_then(|t| t.as_f64()).unwrap_or(0.0);
            newest = newest.max(t);
            let name = e.get("EventName").map(|n| n.to_string()).unwrap_or_default();
            if !UPDATE_EVENTS.contains(&name.as_str()) {
                continue;
            }
            let mut ids: Vec<String> = e.get("Resources").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|r| r.get("ResourceName").map(|n| n.to_string())).collect();
            if let Some(doc) = e.get("CloudTrailEvent").and_then(|d| parse_json(d.to_string().as_bytes()).ok()) {
                if let Some(sid) = doc.get("requestParameters").and_then(|r| r.get("secretId")) {
                    ids.push(sid.to_string());
                }
            }
            for sid in ids {
                let n = secret_name_of(&sid);
                if self.matches(&ctx, &n) && !updated.contains(&n) {
                    updated.push(n);
                }
            }
        }
        if newest > 0.0 {
            *self.since.lock().unwrap() = Some(newest);
        }
        if !updated.is_empty() {
            camel_core::camel_info!("aws-secrets-manager", "Secrets {:?} updated: reloading the CamelContext routes", updated);
            self.reloads.fetch_add(1, Ordering::SeqCst);
            let listeners = self.listeners.lock().unwrap().clone();
            for l in listeners {
                l(&updated);
            }
            for id in ctx.route_ids() {
                if ctx.route_status(&id).is_some_and(|s| s == camel_core::context::ServiceStatus::Started) {
                    ctx.stop_route(&id)?;
                    ctx.start_route(&id)?;
                }
            }
        }
        Ok(updated)
    }

    fn start(self: &Arc<Self>) -> Result<()> {
        let Some(ctx) = self.ctx.upgrade() else { return Ok(()) };
        if !Self::prop(&ctx, "refreshEnabled").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Ok(());
        }
        let period = Self::prop(&ctx, "refreshPeriod").map(|p| p.parse::<u64>().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.vault.aws.refreshPeriod: {}", p)))).transpose()?.unwrap_or(30_000).max(10);
        // only events after start count
        *self.since.lock().unwrap() = Some(crate::util::now_millis() as f64 / 1000.0);
        *self.running.0.lock().unwrap() = true;
        self.enabled.store(true, Ordering::SeqCst);
        let me = self.clone();
        let h = std::thread::Builder::new().name("aws-secrets-refresh".into()).spawn(move || loop {
            {
                let (lock, cv) = &*me.running;
                let guard = lock.lock().unwrap();
                let (guard, _) = cv.wait_timeout_while(guard, Duration::from_millis(period), |r| *r).unwrap();
                if !*guard {
                    return;
                }
            }
            if let Err(e) = me.check_now() {
                camel_core::camel_warn!("aws-secrets-manager", "Secret refresh check failed: {}", e);
            }
        });
        *self.thread.lock().unwrap() = h.ok();
        Ok(())
    }

    fn stop(&self) {
        *self.running.0.lock().unwrap() = false;
        self.running.1.notify_all();
        self.enabled.store(false, Ordering::SeqCst);
        if let Some(h) = self.thread.lock().unwrap().take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

struct RefreshService(Arc<SecretsRefreshTask>);

impl Service for RefreshService {
    fn start(&self) -> Result<()> {
        self.0.start()
    }
    fn stop(&self) -> Result<()> {
        self.0.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_references() {
        assert_eq!(parse_secret_ref("db"), ("db".into(), None, None));
        assert_eq!(parse_secret_ref("db/user@v2"), ("db".into(), Some("user".into()), Some("v2".into())));
        assert_eq!(secret_field(r#"{"user":"scott","port":5432}"#, Some("port")).as_deref(), Some("5432"));
        assert_eq!(secret_field("plain", None).as_deref(), Some("plain"));
        assert_eq!(secret_field(r#"{"user":"scott"}"#, Some("nope")), None);
        assert_eq!(secret_name_of("arn:aws:secretsmanager:us-east-1:000:secret:db-pass-AbC123"), "db-pass");
        assert_eq!(secret_name_of("plain"), "plain");
    }
}
