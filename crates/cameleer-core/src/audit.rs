//! The audit log: hash-chained records (one per AI tool call, per de-identified instance...),
//! so an edited, removed or reordered record shows. One log per context
//! ([`AuditLog::of`]) is shared by everything that audits: the AI action layer and the packs.
//!
//! Each record is a JSON object written on one line. Its `hash` is the SHA-256 of the previous
//! record's hash followed by the record's canonical JSON without the `hash` field; the first
//! record chains from 64 zeros. [`verify_file`] recomputes the chain.
//!
//! Configuration (context properties):
//! - `camel.ai.audit.enabled` (default `true`): keep records at all.
//! - `camel.ai.audit.file`: also append them to this JSON lines file. An existing file is
//!   continued: the chain picks up from its last record.
//! - `camel.ai.audit.maxRecords` (default 1000): records kept in memory.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::json::canonical_json;
use crate::value::Value;

fn sha256_hex(data: &[u8]) -> String {
    crate::language::support::to_hex(&crate::language::support::sha256(data))
}

/// The registry bean that holds the context's shared [`AuditLog`].
pub const AUDIT_LOG_BEAN: &str = "cameleer.audit.log";

static CREATE: Mutex<()> = Mutex::new(());

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One audited call. Digests are SHA-256 of the canonical JSON, so the log proves what was
/// called without storing the arguments or results themselves.
#[derive(Clone, Debug, Default)]
pub struct AuditEntry {
    pub tool: String,
    pub principal: Option<String>,
    pub call_id: Option<String>,
    pub conversation_id: Option<String>,
    pub arguments_digest: String,
    /// `allowed`, `denied`, `invalid`, `disabled`, `unknown-tool`, `rate-limited`,
    /// `circuit-open`, `approval-required`, `approved`, `rejected`.
    pub decision: String,
    pub approver: Option<String>,
    pub duplicate: bool,
    /// `ok`, `error`, `timeout`, `pending` or `none` (not run).
    pub outcome: String,
    pub result_digest: Option<String>,
    pub duration_ms: u64,
    pub detail: Option<String>,
}

/// The in-memory log and the optional file.
pub struct AuditLog {
    enabled: bool,
    file: Option<PathBuf>,
    max: usize,
    state: Mutex<AuditState>,
}

struct AuditState {
    seq: u64,
    last_hash: String,
    /// The records as their canonical JSON with `hash` (sorted keys); parsed on read, so a
    /// call pays for one string and one SHA-256, not for building a JSON object.
    records: VecDeque<String>,
    /// The audit file, opened once.
    out: Option<File>,
}

impl AuditLog {
    /// A log; `file` is continued if it exists.
    pub fn new(enabled: bool, file: Option<PathBuf>, max: usize) -> Result<AuditLog> {
        let mut state = AuditState { seq: 0, last_hash: GENESIS.to_string(), records: VecDeque::new(), out: None };
        if let Some(path) = &file {
            if path.exists() {
                let (count, last) = verify_file(path)?;
                state.seq = count as u64;
                if let Some(h) = last {
                    state.last_hash = h;
                }
            }
        }
        if let Some(path) = &file {
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => state.out = Some(f),
                Err(err) => crate::camel_warn!("audit", "Cannot open the audit file {}: {}", path.display(), err),
            }
        }
        Ok(AuditLog { enabled, file, max: max.max(1), state: Mutex::new(state) })
    }

    /// The context's shared log, created on first use from `camel.ai.audit.enabled`,
    /// `camel.ai.audit.file` and `camel.ai.audit.maxRecords`.
    pub fn of(ctx: &CamelContext) -> Result<Arc<AuditLog>> {
        if let Some(l) = ctx.registry().lookup::<AuditLog>(AUDIT_LOG_BEAN) {
            return Ok(l);
        }
        let _g = CREATE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(l) = ctx.registry().lookup::<AuditLog>(AUDIT_LOG_BEAN) {
            return Ok(l);
        }
        let props = ctx.properties();
        let enabled = props.get("camel.ai.audit.enabled").map(|v| !v.trim().eq_ignore_ascii_case("false")).unwrap_or(true);
        let file = props.get("camel.ai.audit.file").filter(|f| !f.trim().is_empty()).map(|f| PathBuf::from(f.trim()));
        let max = props.get("camel.ai.audit.maxRecords").and_then(|m| m.trim().parse().ok()).unwrap_or(1000);
        let log = Arc::new(AuditLog::new(enabled, file, max)?);
        ctx.registry().bind_arc(AUDIT_LOG_BEAN, log.clone());
        Ok(log)
    }

    /// A log kept in memory only.
    pub fn in_memory() -> AuditLog {
        AuditLog::new(true, None, 1000).expect("in-memory audit log")
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Append a record; returns its `seq`. The record (with `seq`, `time`, `prev` and `hash`)
    /// is in [`AuditLog::records`] and the file.
    pub fn append(&self, e: &AuditEntry) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let time = now_iso();
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.seq += 1;
        let seq = st.seq;
        // the canonical JSON without `hash`: keys in sorted order, as `canonical_json` writes it
        let mut body = String::with_capacity(512);
        let s = |out: &mut String, k: &str, v: &str| {
            out.push_str(k);
            crate::value::json_escape(v, out);
        };
        let o = |out: &mut String, k: &str, v: &Option<String>| {
            out.push_str(k);
            match v {
                Some(v) => crate::value::json_escape(v, out),
                None => out.push_str("null"),
            }
        };
        o(&mut body, "{\"approver\":", &e.approver);
        s(&mut body, ",\"argumentsDigest\":", &e.arguments_digest);
        o(&mut body, ",\"callId\":", &e.call_id);
        o(&mut body, ",\"conversationId\":", &e.conversation_id);
        s(&mut body, ",\"decision\":", &e.decision);
        o(&mut body, ",\"detail\":", &e.detail);
        body.push_str(if e.duplicate { ",\"duplicate\":true" } else { ",\"duplicate\":false" });
        body.push_str(",\"durationMs\":");
        body.push_str(&e.duration_ms.to_string());
        s(&mut body, ",\"outcome\":", &e.outcome);
        s(&mut body, ",\"prev\":", &st.last_hash);
        o(&mut body, ",\"principal\":", &e.principal);
        o(&mut body, ",\"resultDigest\":", &e.result_digest);
        body.push_str(",\"seq\":");
        body.push_str(&seq.to_string());
        s(&mut body, ",\"time\":", &time);
        s(&mut body, ",\"tool\":", &e.tool);
        let mut data = String::with_capacity(64 + body.len() + 1);
        data.push_str(&st.last_hash);
        data.push_str(&body);
        data.push('}');
        let hash = sha256_hex(data.as_bytes());
        // the record with `hash`, which sorts between `durationMs` and `outcome`
        let at = body.find(",\"outcome\":").expect("outcome field");
        let mut record = String::with_capacity(body.len() + 80);
        record.push_str(&body[..at]);
        record.push_str(",\"hash\":\"");
        record.push_str(&hash);
        record.push('"');
        record.push_str(&body[at..]);
        record.push('}');
        if let Some(f) = st.out.as_mut() {
            let mut line = String::with_capacity(record.len() + 1);
            line.push_str(&record);
            line.push('\n');
            if let Err(err) = f.write_all(line.as_bytes()) {
                let path = self.file.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                crate::camel_warn!("audit", "Cannot append to the audit file {}: {}", path, err);
            }
        }
        st.last_hash = hash;
        st.records.push_back(record);
        while st.records.len() > self.max {
            st.records.pop_front();
        }
        Some(seq)
    }

    /// The records kept in memory, oldest first.
    pub fn records(&self) -> Vec<Value> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.records.iter().filter_map(|r| crate::json::parse(r).ok()).collect()
    }

    /// The hash of the latest record (64 zeros before the first one).
    pub fn head(&self) -> String {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).last_hash.clone()
    }
}

fn chain_hash(prev: &str, record_without_hash: &Value) -> String {
    let mut data = String::with_capacity(64 + 256);
    data.push_str(prev);
    data.push_str(&canonical_json(record_without_hash));
    sha256_hex(data.as_bytes())
}

/// Verify a chain of records (oldest first). Returns the number of records and the last hash,
/// or an error naming the first record that does not verify.
pub fn verify_records(records: &[Value]) -> Result<(usize, Option<String>)> {
    let mut prev = GENESIS.to_string();
    for (i, r) in records.iter().enumerate() {
        let Some(m) = r.as_object() else {
            return Err(CamelError::illegal_state(format!("audit record {} is not a JSON object", i + 1)));
        };
        let stated = m.get("hash").and_then(|h| h.as_str()).unwrap_or("");
        let stated_prev = m.get("prev").and_then(|h| h.as_str()).unwrap_or("");
        if stated_prev != prev {
            return Err(CamelError::illegal_state(format!(
                "audit record {} does not follow record {}: a record was removed, reordered or edited",
                i + 1,
                i
            )));
        }
        let mut without = m.clone();
        without.remove("hash");
        let actual = chain_hash(&prev, &Value::Object(without));
        if actual != stated {
            return Err(CamelError::illegal_state(format!("audit record {} was modified: its hash does not match its content", i + 1)));
        }
        prev = actual;
    }
    Ok((records.len(), if records.is_empty() { None } else { Some(prev) }))
}

/// Verify an audit file (JSON lines).
pub fn verify_file(path: &Path) -> Result<(usize, Option<String>)> {
    let text = std::fs::read_to_string(path).map_err(|e| CamelError::illegal_state(format!("Cannot read audit file {}: {}", path.display(), e)))?;
    let mut records = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v = crate::json::parse(line)
            .map_err(|e| CamelError::illegal_state(format!("audit record {} is not valid JSON: {}", i + 1, e.message)))?;
        records.push(v);
    }
    verify_records(&records)
}

fn now_iso() -> String {
    let now = crate::clock::now_system_time();
    let d = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, mo, da) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, da, rem / 3600, (rem % 3600) / 60, rem % 60, d.subsec_millis())
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: &str) -> AuditEntry {
        AuditEntry { tool: tool.into(), arguments_digest: "d".into(), decision: "allowed".into(), outcome: "ok".into(), ..Default::default() }
    }

    #[test]
    fn chain_verifies_and_detects_tampering() {
        let log = AuditLog::in_memory();
        for t in ["a", "b", "c"] {
            log.append(&entry(t));
        }
        let records = log.records();
        assert_eq!(verify_records(&records).unwrap().0, 3);
        // edit a field
        let mut edited = records.clone();
        if let Value::Object(m) = &mut edited[1] {
            m.insert("tool".into(), Value::from("x"));
        }
        assert!(verify_records(&edited).unwrap_err().message.contains("record 2 was modified"));
        // remove a record
        let removed = vec![records[0].clone(), records[2].clone()];
        assert!(verify_records(&removed).unwrap_err().message.contains("record 2 does not follow"));
    }

    #[test]
    fn file_is_continued_across_restarts() {
        let dir = std::env::temp_dir().join(format!("cameleer-ai-audit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        AuditLog::new(true, Some(path.clone()), 10).unwrap().append(&entry("a"));
        let second = AuditLog::new(true, Some(path.clone()), 10).unwrap();
        second.append(&entry("b"));
        assert_eq!(verify_file(&path).unwrap().0, 2);
        // a hand edit breaks it, and a log refuses to continue a broken file
        let text = std::fs::read_to_string(&path).unwrap().replace("\"tool\":\"a\"", "\"tool\":\"z\"");
        std::fs::write(&path, text).unwrap();
        assert!(verify_file(&path).is_err());
        assert!(AuditLog::new(true, Some(path.clone()), 10).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
