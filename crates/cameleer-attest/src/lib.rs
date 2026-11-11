//! # Cameleer attestation
//!
//! When Cameleer runs inside a hardware TEE (an Intel TDX or AMD SEV-SNP confidential VM, or a
//! dstack CVM), this crate produces **attestation evidence** that proves to a client what is
//! running:
//!
//! - **the evidence** comes from the Linux kernel's vendor-neutral report interface
//!   (configfs-tsm, `/sys/kernel/config/tsm/report`, kernel 6.7+) or from the dstack guest agent
//!   (`/var/run/dstack.sock`), see [`provider`];
//! - **the binding**: the 64 bytes the hardware signs (REPORTDATA) are
//!   `SHA-512("cameleer-attest-v1" || 0x00 || manifest head (48) || TLS SPKI SHA-256 (32) ||
//!   nonce length (u16 BE) || nonce)`, see [`report_data`]. A client recomputes them from the
//!   manifest digest it is given, the TLS certificate it saw on its own connection and the nonce
//!   it chose;
//! - **the manifest** records the version, the route files and routes, the configuration
//!   (secret values left out) and the published tools, and a hash-chained log records every
//!   change after start-up; the head of that chain is what the evidence binds, see [`Manifest`]
//!   and [`ManifestLog`].
//!
//! The hardware vouches only that the *measured* code chose those bytes: the evidence means
//! "this exact Cameleer release" only when the platform measurement (the image digest, the
//! dstack compose hash) shows a known build.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use cameleer_core::attest::AttestationService;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};
use cameleer_tls::crypto::hash::{Digest, Sha256, Sha384, Sha512};

pub mod provider;

pub use provider::{Dstack, Evidence, EvidenceProvider, Tsm};

/// The domain string that starts every Cameleer REPORTDATA preimage, so a Cameleer quote can't
/// be replayed as another protocol's.
pub const DOMAIN: &str = "cameleer-attest-v1";

/// Smallest and largest client nonce, in bytes.
pub const NONCE_MIN: usize = 16;
pub const NONCE_MAX: usize = 64;

/// The REPORTDATA for a manifest head, a TLS key hash and a nonce (see the crate docs).
pub fn report_data(manifest_head: &[u8; 48], tls_spki_sha256: Option<&[u8; 32]>, nonce: &[u8]) -> [u8; 64] {
    let mut pre = Vec::with_capacity(DOMAIN.len() + 1 + 48 + 32 + 2 + nonce.len());
    pre.extend_from_slice(DOMAIN.as_bytes());
    pre.push(0);
    pre.extend_from_slice(manifest_head);
    pre.extend_from_slice(tls_spki_sha256.unwrap_or(&[0u8; 32]));
    pre.extend_from_slice(&(nonce.len() as u16).to_be_bytes());
    pre.extend_from_slice(nonce);
    let d = Sha512::digest(&pre);
    let mut out = [0u8; 64];
    out.copy_from_slice(&d);
    out
}

/// SHA-256 of a certificate's SubjectPublicKeyInfo (the TLS key the evidence is bound to).
pub fn spki_sha256(cert_der: &[u8]) -> Result<[u8; 32]> {
    let cert = cameleer_tls::x509::Certificate::from_der(cert_der).map_err(|e| CamelError::illegal_argument(format!("Cannot parse the TLS certificate: {:?}", e)))?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(&cert.spki));
    Ok(out)
}

/// The SHA-256 of the embedded server's TLS key (`camel.server.ssl.*`), or `None` when that
/// server has no TLS.
pub fn server_tls_spki(ctx: &cameleer_core::context::CamelContext) -> Result<Option<[u8; 32]>> {
    if !cameleer_tls::camel_support::server_ssl_enabled(ctx) {
        return Ok(None);
    }
    let cfg = cameleer_tls::camel_support::server_config(ctx, None, "attestation")?;
    let leaf = cfg.cert_chain().first().ok_or_else(|| CamelError::illegal_argument("The embedded server's TLS configuration has no certificate"))?;
    spki_sha256(leaf).map(Some)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn sha384_hex(data: &[u8]) -> String {
    hex(&Sha384::digest(data))
}

pub fn hex(data: &[u8]) -> String {
    cameleer_tls::util::hex(data)
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Base64 (standard alphabet, padded).
pub fn b64(data: &[u8]) -> String {
    cameleer_tls::pem::base64_encode(data)
}

/// Decode base64 in either alphabet, padded or not (nonces arrive in URLs as base64url).
pub fn unb64(s: &str) -> Option<Vec<u8>> {
    let mut t: String = s.trim().chars().map(|c| match c {
        '-' => '+',
        '_' => '/',
        c => c,
    }).collect();
    while t.len() % 4 != 0 {
        t.push('=');
    }
    cameleer_tls::pem::base64_decode(&t)
}

/// JSON with object keys sorted at every level and no whitespace: the form that is hashed.
pub fn canonical_json(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<String, Value> = m.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                let mut out = ObjectMap::new();
                for (k, v) in sorted {
                    out.insert(k, v);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            v => v.clone(),
        }
    }
    sort(v).to_json()
}

/// Whether a property holds a secret, judged by its key: such values never enter the manifest
/// (a hash of a low-entropy secret would leak it).
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["password", "passwd", "secret", "token", "credential", "apikey", "api-key", "api_key", "private", "passphrase", "accesskey", "access-key", "access_key", "key"]
        .iter()
        .any(|w| k.contains(w))
}

/// What the manifest records: Cameleer's build, the loaded route files, the routes, the
/// configuration (secret values replaced by a marker) and the published tools.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub version: String,
    pub features: Vec<String>,
    pub target: String,
    /// SHA-384 of the running executable (self-reported; the platform measurement is what counts).
    pub exe_sha384: Option<String>,
    /// Route file path → SHA-256 of its bytes as loaded.
    pub route_files: BTreeMap<String, String>,
    /// Route id → (consumer URI, SHA-256 of the route definition).
    pub routes: BTreeMap<String, (String, String)>,
    /// Property → value, or `<secret>` for keys that hold secrets.
    pub properties: BTreeMap<String, String>,
    /// Tool name → its definition fingerprint.
    pub tools: BTreeMap<String, String>,
}

impl Manifest {
    /// Record `props`, replacing secret values with a marker. A value written as a placeholder
    /// (`{{aws:...}}`, `{{env:...}}`) is kept as written: it is the reference, not the secret.
    pub fn set_properties(&mut self, props: &BTreeMap<String, String>) {
        self.properties = props
            .iter()
            .map(|(k, v)| {
                let shown = if is_secret_key(k) && !(v.trim_start().starts_with("{{") && v.trim_end().ends_with("}}")) { "<secret>".to_string() } else { v.clone() };
                (k.clone(), shown)
            })
            .collect();
    }

    pub fn to_value(&self) -> Value {
        let map = |m: &BTreeMap<String, String>| Value::Object(m.iter().map(|(k, v)| (k.clone(), Value::from(v.as_str()))).collect());
        let mut o = ObjectMap::new();
        o.insert("version".into(), Value::from(self.version.as_str()));
        o.insert("features".into(), Value::Array(self.features.iter().map(|f| Value::from(f.as_str())).collect()));
        o.insert("target".into(), Value::from(self.target.as_str()));
        o.insert("exeSha384".into(), self.exe_sha384.as_deref().map(Value::from).unwrap_or(Value::Null));
        o.insert("routeFiles".into(), map(&self.route_files));
        o.insert(
            "routes".into(),
            Value::Object(self.routes.iter().map(|(id, (uri, d))| (id.clone(), Value::object([("uri", Value::from(uri.as_str())), ("sha256", Value::from(d.as_str()))]))).collect()),
        );
        o.insert("properties".into(), map(&self.properties));
        o.insert("tools".into(), map(&self.tools));
        Value::Object(o)
    }

    /// SHA-384 of the canonical JSON of [`to_value`](Self::to_value).
    pub fn digest(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        out.copy_from_slice(&Sha384::digest(canonical_json(&self.to_value()).as_bytes()));
        out
    }
}

/// One entry of the [`ManifestLog`].
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub seq: u64,
    pub at_ms: u64,
    pub kind: String,
    pub detail: String,
    /// The chain head after this entry.
    pub head: [u8; 48],
}

impl LogEntry {
    fn to_value(&self) -> Value {
        Value::object([
            ("seq", Value::Int(self.seq as i64)),
            ("atMs", Value::Int(self.at_ms as i64)),
            ("kind", Value::from(self.kind.as_str())),
            ("detail", Value::from(self.detail.as_str())),
            ("head", Value::from(format!("sha384:{}", hex(&self.head)))),
        ])
    }
}

/// The hash-chained record of what ran: the first entry carries the start-up manifest digest,
/// later ones every change (a route added or removed, a tool disabled, a reload). Each head is
/// `SHA-384(previous head || canonical JSON of the entry without its head)`; the evidence binds
/// the latest head, so a change after start-up is visible to every later client.
#[derive(Default)]
pub struct ManifestLog {
    entries: Mutex<Vec<LogEntry>>,
}

impl ManifestLog {
    pub fn new(manifest: &Manifest) -> ManifestLog {
        let log = ManifestLog::default();
        log.record("start", &format!("sha384:{}", hex(&manifest.digest())));
        log
    }

    /// Append an entry; returns the new head.
    pub fn record(&self, kind: &str, detail: &str) -> [u8; 48] {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let prev = entries.last().map(|e| e.head).unwrap_or([0u8; 48]);
        let seq = entries.len() as u64 + 1;
        let at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let body = canonical_json(&Value::object([("seq", Value::Int(seq as i64)), ("atMs", Value::Int(at_ms as i64)), ("kind", Value::from(kind)), ("detail", Value::from(detail))]));
        let mut pre = prev.to_vec();
        pre.extend_from_slice(body.as_bytes());
        let mut head = [0u8; 48];
        head.copy_from_slice(&Sha384::digest(&pre));
        entries.push(LogEntry { seq, at_ms, kind: kind.to_string(), detail: detail.to_string(), head });
        head
    }

    pub fn head(&self) -> [u8; 48] {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).last().map(|e| e.head).unwrap_or([0u8; 48])
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Recompute every head from the entries: true when the chain is intact.
    pub fn verify(entries: &[LogEntry]) -> bool {
        let mut prev = [0u8; 48];
        for e in entries {
            let body = canonical_json(&Value::object([("seq", Value::Int(e.seq as i64)), ("atMs", Value::Int(e.at_ms as i64)), ("kind", Value::from(e.kind.as_str())), ("detail", Value::from(e.detail.as_str()))]));
            let mut pre = prev.to_vec();
            pre.extend_from_slice(body.as_bytes());
            if Sha384::digest(&pre) != e.head {
                return false;
            }
            prev = e.head;
        }
        true
    }
}

/// The attestation service: an evidence provider, the manifest and its log, and the TLS key
/// the evidence is bound to.
pub struct Attestation {
    provider: Box<dyn EvidenceProvider>,
    manifest: Manifest,
    log: Arc<ManifestLog>,
    tls_spki: Option<[u8; 32]>,
    endpoint: String,
    tool: Option<String>,
    watches: Mutex<Vec<Watch>>,
}

/// A part of the running state that is fingerprinted again whenever evidence is asked for; a
/// changed fingerprint is recorded in the log before the evidence is made. Checking lazily
/// costs nothing while routes run (no per-exchange event listener).
struct Watch {
    name: String,
    fingerprint: Box<dyn Fn() -> String + Send + Sync>,
    last: String,
}

impl Attestation {
    pub fn new(provider: Box<dyn EvidenceProvider>, manifest: Manifest, tls_spki: Option<[u8; 32]>) -> Attestation {
        let log = Arc::new(ManifestLog::new(&manifest));
        Attestation { provider, manifest, log, tls_spki, endpoint: "/q/attestation".into(), tool: None, watches: Mutex::default() }
    }

    /// Watch a part of the running state (the routes, the published tools): `fingerprint` is
    /// called now and whenever evidence is asked for, and a change is logged as `<name>-changed`.
    pub fn watch(self, name: &str, fingerprint: impl Fn() -> String + Send + Sync + 'static) -> Self {
        let last = fingerprint();
        self.watches.lock().unwrap_or_else(|p| p.into_inner()).push(Watch { name: name.to_string(), fingerprint: Box::new(fingerprint), last });
        self
    }

    /// Log what changed since the last look.
    fn refresh(&self) {
        let mut watches = self.watches.lock().unwrap_or_else(|p| p.into_inner());
        for w in watches.iter_mut() {
            let now = (w.fingerprint)();
            if now != w.last {
                self.log.record(&format!("{}-changed", w.name), &now);
                w.last = now;
            }
        }
    }

    /// The HTTP path reports are served at (for [`summary`](AttestationService::summary)).
    pub fn with_endpoint(mut self, path: &str) -> Self {
        self.endpoint = path.to_string();
        self
    }

    /// The MCP tool name that returns reports, when that tool is enabled.
    pub fn with_tool(mut self, name: Option<String>) -> Self {
        self.tool = name;
        self
    }

    /// The log, to record changes after start-up.
    pub fn log(&self) -> Arc<ManifestLog> {
        self.log.clone()
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl AttestationService for Attestation {
    fn summary(&self) -> Value {
        self.refresh();
        let mut o = ObjectMap::new();
        o.insert("version".into(), Value::Int(1));
        o.insert("domain".into(), Value::from(DOMAIN));
        o.insert("platform".into(), Value::from(self.provider.platform()));
        o.insert("provider".into(), Value::from(self.provider.name()));
        o.insert("manifestDigest".into(), Value::from(format!("sha384:{}", hex(&self.log.head()))));
        o.insert("tlsSpkiSha256".into(), self.tls_spki.map(|s| Value::from(hex(&s))).unwrap_or(Value::Null));
        o.insert("endpoint".into(), Value::from(self.endpoint.as_str()));
        if let Some(t) = &self.tool {
            o.insert("tool".into(), Value::from(t.as_str()));
        }
        o.insert("nonce".into(), Value::from(format!("{} to {} bytes, base64", NONCE_MIN, NONCE_MAX)));
        Value::Object(o)
    }

    fn report(&self, nonce: Option<&[u8]>) -> Result<Value> {
        let generated;
        let (nonce, chosen) = match nonce {
            Some(n) => {
                if n.len() < NONCE_MIN || n.len() > NONCE_MAX {
                    return Err(CamelError::illegal_argument(format!("The nonce must be {} to {} bytes, got {}", NONCE_MIN, NONCE_MAX, n.len())));
                }
                (n, true)
            }
            None => {
                generated = cameleer_tls::crypto::rng::random_bytes(32);
                (&generated[..], false)
            }
        };
        self.refresh();
        let head = self.log.head();
        let rd = report_data(&head, self.tls_spki.as_ref(), nonce);
        let ev = self.provider.evidence(&rd)?;
        let mut o = ObjectMap::new();
        o.insert("version".into(), Value::Int(1));
        o.insert("platform".into(), Value::from(ev.platform.as_str()));
        o.insert("provider".into(), Value::from(self.provider.name()));
        o.insert("format".into(), Value::from(ev.format.as_str()));
        o.insert("nonce".into(), Value::from(b64(nonce)));
        o.insert("nonceChosenBy".into(), Value::from(if chosen { "client" } else { "server" }));
        o.insert(
            "binding".into(),
            Value::object([
                ("domain", Value::from(DOMAIN)),
                ("manifestDigest", Value::from(format!("sha384:{}", hex(&head)))),
                ("tlsSpkiSha256", self.tls_spki.map(|s| Value::from(hex(&s))).unwrap_or(Value::Null)),
                ("reportData", Value::from(hex(&rd))),
            ]),
        );
        o.insert("evidence".into(), Value::from(b64(&ev.evidence)));
        for (k, v) in ev.extra.iter() {
            o.insert(k.clone(), v.clone());
        }
        o.insert("manifest".into(), self.manifest.to_value());
        o.insert("log".into(), Value::Array(self.log.entries().iter().map(|e| e.to_value()).collect()));
        Ok(Value::Object(o))
    }
}

/// Check a report's binding as a client would: the REPORTDATA recomputes from the manifest
/// digest, the TLS key hash the client observed (`None` without TLS) and the nonce it sent, and
/// the log chain is intact and ends at that digest. This does not verify the hardware evidence
/// itself (the quote's signature chain), which needs a verifier for the platform.
pub fn check_binding(report: &Value, observed_spki: Option<&[u8; 32]>, nonce: &[u8]) -> Result<[u8; 64]> {
    let bad = |m: &str| CamelError::illegal_argument(format!("Attestation report: {}", m));
    let b = report.get("binding").ok_or_else(|| bad("no binding"))?;
    let digest = b.get("manifestDigest").and_then(|v| v.as_str()).and_then(|s| s.strip_prefix("sha384:")).and_then(unhex).ok_or_else(|| bad("no manifest digest"))?;
    let head: [u8; 48] = digest.try_into().map_err(|_| bad("manifest digest is not 48 bytes"))?;
    let sent = b.get("tlsSpkiSha256").and_then(|v| v.as_str()).and_then(unhex);
    match (observed_spki, sent.as_deref()) {
        (Some(o), Some(s)) if o.as_slice() == s => {}
        (None, None) => {}
        _ => return Err(bad("the TLS key it is bound to is not the one on this connection")),
    }
    let rd = report_data(&head, observed_spki, nonce);
    if b.get("reportData").and_then(|v| v.as_str()).and_then(unhex).as_deref() != Some(&rd[..]) {
        return Err(bad("REPORTDATA does not match the manifest digest, TLS key and nonce"));
    }
    let entries: Vec<LogEntry> = report
        .get("log")
        .and_then(|l| l.as_array())
        .ok_or_else(|| bad("no log"))?
        .iter()
        .map(|e| {
            let head = e.get("head").and_then(|h| h.as_str()).and_then(|s| s.strip_prefix("sha384:")).and_then(unhex).and_then(|h| <[u8; 48]>::try_from(h).ok());
            Some(LogEntry {
                seq: e.get("seq")?.as_i64()? as u64,
                at_ms: e.get("atMs")?.as_i64()? as u64,
                kind: e.get("kind")?.as_str()?.to_string(),
                detail: e.get("detail")?.as_str()?.to_string(),
                head: head?,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("malformed log"))?;
    if !ManifestLog::verify(&entries) || entries.last().map(|e| e.head) != Some(head) {
        return Err(bad("the log chain is broken or does not end at the manifest digest"));
    }
    Ok(rd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_data_is_stable_and_binds_every_input() {
        let head = [7u8; 48];
        let spki = [9u8; 32];
        let a = report_data(&head, Some(&spki), b"0123456789abcdef");
        assert_eq!(a, report_data(&head, Some(&spki), b"0123456789abcdef"));
        assert_ne!(a, report_data(&[8u8; 48], Some(&spki), b"0123456789abcdef"));
        assert_ne!(a, report_data(&head, None, b"0123456789abcdef"));
        assert_ne!(a, report_data(&head, Some(&spki), b"0123456789abcdeg"));
    }

    #[test]
    fn secrets_never_enter_the_manifest() {
        let mut m = Manifest::default();
        let mut p = BTreeMap::new();
        p.insert("camel.component.sql.password".to_string(), "hunter2".to_string());
        p.insert("camel.vault.aws.secretKey".to_string(), "{{env:AWS_SECRET}}".to_string());
        p.insert("camel.server.port".to_string(), "8080".to_string());
        m.set_properties(&p);
        let j = canonical_json(&m.to_value());
        assert!(!j.contains("hunter2"), "{}", j);
        assert!(j.contains("<secret>") && j.contains("{{env:AWS_SECRET}}") && j.contains("8080"), "{}", j);
    }

    #[test]
    fn the_log_chain_detects_edits() {
        let log = ManifestLog::new(&Manifest::default());
        log.record("route-added", "orders");
        log.record("tool-disabled", "refund");
        let mut entries = log.entries();
        assert!(ManifestLog::verify(&entries));
        entries[1].detail = "billing".into();
        assert!(!ManifestLog::verify(&entries));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let v = cameleer_core::json::parse(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        assert_eq!(canonical_json(&v), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn base64_in_either_alphabet() {
        assert_eq!(unb64("-_8").unwrap(), unb64("+/8=").unwrap());
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
    }
}
