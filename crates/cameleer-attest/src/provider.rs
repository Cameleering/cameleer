//! Where evidence comes from.
//!
//! - [`Tsm`]: the Linux kernel's vendor-neutral report interface (configfs-tsm, kernel 6.7+).
//!   A report is a directory under `/sys/kernel/config/tsm/report`: write the 64-byte
//!   REPORTDATA to `inblob`, read the signed report from `outblob`, check that `generation` did
//!   not move in between (another writer would have changed it), read `provider`
//!   (`tdx_guest`, `sev_guest`), remove the directory. Plain file I/O, so it works without any
//!   ioctl or library.
//! - [`Dstack`]: the dstack guest agent (the open-source layer under Phala Cloud, also on GCP
//!   TDX confidential VMs): `POST /GetQuote` with the REPORTDATA as hex over its unix socket
//!   (`/var/run/dstack.sock`), or over HTTP for its simulator. The quote binds the app identity
//!   (compose hash, app id) in RTMR3; since dstack 0.6.0 applications cannot add their own events,
//!   so Cameleer's data goes in REPORTDATA.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

use crate::{hex, unhex};

/// Evidence for one REPORTDATA.
#[derive(Clone, Debug)]
pub struct Evidence {
    /// `tdx`, `sev-snp`, or the kernel's provider name for others.
    pub platform: String,
    /// `tdx-quote`, `sev-snp-report`, `tsm-report`.
    pub format: String,
    /// The signed report, as the platform produced it.
    pub evidence: Vec<u8>,
    /// More fields for the report (`auxblob`, the dstack event log and app identity).
    pub extra: ObjectMap,
}

/// A source of attestation evidence.
pub trait EvidenceProvider: Send + Sync {
    /// `tsm` or `dstack`.
    fn name(&self) -> &str;
    /// The platform, as far as it is known before a report is made.
    fn platform(&self) -> String;
    /// Signed evidence over `report_data`.
    fn evidence(&self, report_data: &[u8; 64]) -> Result<Evidence>;
}

fn tee_error(msg: String) -> CamelError {
    CamelError::new("AttestationException", msg)
}

// ---------------------------------------------------------------------------------------------
// configfs-tsm

/// The kernel's configfs-tsm report interface.
pub struct Tsm {
    root: PathBuf,
    seq: AtomicU64,
    lock: Mutex<()>,
    /// The platform of the last report (the provider is only readable inside a report entry).
    seen: Mutex<Option<String>>,
}

/// The default configfs-tsm report directory.
pub const TSM_DEFAULT: &str = "/sys/kernel/config/tsm/report";

impl Tsm {
    pub fn new(root: impl Into<PathBuf>) -> Tsm {
        Tsm { root: root.into(), seq: AtomicU64::new(0), lock: Mutex::new(()), seen: Mutex::new(None) }
    }

    /// Whether the interface exists at `root`.
    pub fn available(root: &Path) -> bool {
        root.is_dir()
    }

    fn platform_of(provider: &str) -> (String, String) {
        match provider.trim() {
            "tdx_guest" => ("tdx".into(), "tdx-quote".into()),
            "sev_guest" => ("sev-snp".into(), "sev-snp-report".into()),
            other => (other.to_string(), "tsm-report".into()),
        }
    }
}

impl EvidenceProvider for Tsm {
    fn name(&self) -> &str {
        "tsm"
    }

    fn platform(&self) -> String {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_else(|| "tsm".into())
    }

    fn evidence(&self, report_data: &[u8; 64]) -> Result<Evidence> {
        // one report at a time from this process; other writers are caught by `generation`
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let dir = self.root.join(format!("cameleer-{}-{}", std::process::id(), n));
        std::fs::create_dir(&dir).map_err(|e| tee_error(format!("Cannot create a report entry in {}: {}", self.root.display(), e)))?;
        let result = (|| {
            std::fs::write(dir.join("inblob"), report_data).map_err(|e| tee_error(format!("Cannot write inblob: {}", e)))?;
            // the kernel has `generation` from the start; wait briefly for an emulation that
            // writes it when it sees `inblob`
            let mut gen_before = read_trimmed(&dir.join("generation")).filter(|g| !g.is_empty());
            for _ in 0..50 {
                if gen_before.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                gen_before = read_trimmed(&dir.join("generation")).filter(|g| !g.is_empty());
            }
            let out = read_report(&dir.join("outblob"))?;
            let gen_after = read_trimmed(&dir.join("generation"));
            if gen_before != gen_after {
                return Err(tee_error("The report entry changed while it was read (another writer); try again".into()));
            }
            let provider = read_trimmed(&dir.join("provider")).unwrap_or_default();
            let (platform, format) = Tsm::platform_of(&provider);
            *self.seen.lock().unwrap_or_else(|p| p.into_inner()) = Some(platform.clone());
            let mut extra = ObjectMap::new();
            extra.insert("tsmProvider".into(), Value::from(provider.as_str()));
            if let Ok(aux) = std::fs::read(dir.join("auxblob")) {
                if !aux.is_empty() {
                    extra.insert("auxblob".into(), Value::from(crate::b64(&aux)));
                }
            }
            Ok(Evidence { platform, format, evidence: out, extra })
        })();
        // configfs entries go with rmdir; a plain directory (in tests) needs its files removed
        if std::fs::remove_dir(&dir).is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }
}

fn read_trimmed(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// The kernel produces `outblob` on the first read; give a slow quoting service a moment.
fn read_report(p: &Path) -> Result<Vec<u8>> {
    let mut last = None;
    for _ in 0..50 {
        match std::fs::read(p) {
            Ok(b) if !b.is_empty() => return Ok(b),
            Ok(_) => {}
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(tee_error(match last {
        Some(e) => format!("Cannot read outblob: {}", e),
        None => "The TEE returned an empty report".into(),
    }))
}

// ---------------------------------------------------------------------------------------------
// dstack

/// The dstack guest agent, over its unix socket or (for the simulator) HTTP.
pub struct Dstack {
    endpoint: String,
    info: Mutex<Option<ObjectMap>>,
}

/// The default dstack guest agent socket.
pub const DSTACK_DEFAULT: &str = "/var/run/dstack.sock";

impl Dstack {
    /// `endpoint` is a unix socket path or an `http://host:port` URL.
    pub fn new(endpoint: impl Into<String>) -> Dstack {
        Dstack { endpoint: endpoint.into(), info: Mutex::new(None) }
    }

    /// Whether the agent's socket exists (URLs are assumed reachable).
    pub fn available(endpoint: &str) -> bool {
        endpoint.starts_with("http://") || Path::new(endpoint).exists()
    }

    fn call(&self, path: &str, body: &str) -> Result<Value> {
        let (status, resp) = if let Some(rest) = self.endpoint.strip_prefix("http://") {
            let host = rest.split('/').next().unwrap_or(rest);
            let s = std::net::TcpStream::connect(host).map_err(|e| tee_error(format!("Cannot reach the dstack agent at {}: {}", self.endpoint, e)))?;
            let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
            post(s, host, path, body)?
        } else {
            self.call_unix(path, body)?
        };
        if status != 200 {
            return Err(tee_error(format!("dstack {} answered HTTP {}: {}", path, status, String::from_utf8_lossy(&resp))));
        }
        cameleer_core::json::parse(&String::from_utf8_lossy(&resp)).map_err(|e| tee_error(format!("dstack {} answered invalid JSON: {}", path, e.message)))
    }

    #[cfg(unix)]
    fn call_unix(&self, path: &str, body: &str) -> Result<(u16, Vec<u8>)> {
        let s = std::os::unix::net::UnixStream::connect(&self.endpoint).map_err(|e| tee_error(format!("Cannot reach the dstack agent at {}: {}", self.endpoint, e)))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
        post(s, "localhost", path, body)
    }

    #[cfg(not(unix))]
    fn call_unix(&self, _path: &str, _body: &str) -> Result<(u16, Vec<u8>)> {
        Err(tee_error(format!("The dstack agent socket {} needs a unix system; use an http:// endpoint", self.endpoint)))
    }

    /// The app identity from `/Info` (app id, instance id, compose hash, OS image hash), once.
    fn identity(&self) -> ObjectMap {
        let mut cached = self.info.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(i) = cached.as_ref() {
            return i.clone();
        }
        let mut out = ObjectMap::new();
        if let Ok(Value::Object(info)) = self.call("/Info", "{}") {
            for k in ["app_id", "instance_id", "compose_hash", "os_image_hash", "app_name"] {
                if let Some(v) = info.get(k) {
                    out.insert(k.to_string(), v.clone());
                }
            }
        }
        *cached = Some(out.clone());
        out
    }
}

impl EvidenceProvider for Dstack {
    fn name(&self) -> &str {
        "dstack"
    }

    fn platform(&self) -> String {
        "tdx".into()
    }

    fn evidence(&self, report_data: &[u8; 64]) -> Result<Evidence> {
        let body = Value::object([("report_data", Value::from(hex(report_data)))]).to_json();
        let r = self.call("/GetQuote", &body)?;
        let quote = r.get("quote").and_then(|q| q.as_str()).and_then(unhex).ok_or_else(|| tee_error("dstack GetQuote returned no quote".into()))?;
        let mut extra = ObjectMap::new();
        if let Some(log) = r.get("event_log") {
            // dstack sends the event log as JSON text; keep it parsed when it is
            let v = match log {
                Value::String(s) => cameleer_core::json::parse(s).unwrap_or_else(|_| log.clone()),
                v => v.clone(),
            };
            extra.insert("eventLog".into(), v);
        }
        let identity = self.identity();
        if !identity.is_empty() {
            extra.insert("dstack".into(), Value::Object(identity));
        }
        Ok(Evidence { platform: "tdx".into(), format: "tdx-quote".into(), evidence: quote, extra })
    }
}

/// One HTTP/1.1 POST with `Connection: close`; returns the status and the body (content-length,
/// chunked or until close).
fn post<S: Read + Write>(mut s: S, host: &str, path: &str, body: &str) -> Result<(u16, Vec<u8>)> {
    let req = format!("POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", path, host, body.len(), body);
    s.write_all(req.as_bytes()).map_err(|e| tee_error(format!("Cannot send to the dstack agent: {}", e)))?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| tee_error(format!("Cannot read the dstack agent's answer: {}", e)))?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| tee_error("The dstack agent sent no HTTP header".into()))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).ok_or_else(|| tee_error(format!("Bad HTTP status line from the dstack agent: {}", head.lines().next().unwrap_or(""))))?;
    let lower = head.to_ascii_lowercase();
    if lower.contains("transfer-encoding: chunked") {
        body = dechunk(&body)?;
    } else if let Some(len) = lower.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()) {
        body.truncate(len);
    }
    Ok((status, body))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").ok_or_else(|| tee_error("Bad chunked body from the dstack agent".into()))?;
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| tee_error("Bad chunk size from the dstack agent".into()))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err(tee_error("Truncated chunked body from the dstack agent".into()));
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_bodies_are_joined() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
    }

    #[test]
    fn tsm_provider_names_map_to_platforms() {
        assert_eq!(Tsm::platform_of("tdx_guest"), ("tdx".to_string(), "tdx-quote".to_string()));
        assert_eq!(Tsm::platform_of("sev_guest\n"), ("sev-snp".to_string(), "sev-snp-report".to_string()));
    }
}
