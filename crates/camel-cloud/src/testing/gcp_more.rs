//! More services of the [`GcpMock`]:
//!
//! * Gmail (`/gmail/v1/users/{user}/...`), Calendar (`/calendar/v3/...`) and Drive
//!   (`/drive/v3/files`, `/upload/drive/v3/files`): tokens need the matching scope, and Gmail
//!   additionally a delegated subject (JWT `sub`, domain-wide delegation; `me` = the subject).
//! * Vertex AI `generateContent` / `countTokens` (echoes the last user turn), Speech-to-Text
//!   (`speech:recognize`, `speech:longrunningrecognize`, `operations/...`; the "audio" bytes are
//!   the transcript) and Text-to-Speech (`text:synthesize`, `voices`).
//! * Cloud Storage V4 signed URLs (`GET /{bucket}/{object}?X-Goog-Signature=...`, RSA
//!   signature verified with the trusted service account key, expiry checked; no bearer token).
//! * Pub/Sub **StreamingPull** over gRPC on a second port (camel-grpc HTTP/2 server): the bearer
//!   token is verified in the `authorization` metadata, messages are leased for the stream ack
//!   deadline, acks remove them, `modify_deadline_seconds` 0 nacks (immediate redelivery) and
//!   expired leases are redelivered; `max_outstanding_messages` is honoured.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::value::{ObjectMap, Value};
use camel_grpc::grpc::server::{GrpcServer, ServerCall};
use camel_grpc::grpc::{Code, Status};

use super::{gerror, GcpMock, Shared};
use crate::gcp::pubsub_stream::pool;
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, base64url_encode, iso8601_millis, now_millis, now_secs, obj, parse_json, percent_decode, random_hex};

#[derive(Clone)]
struct GmailMsg {
    id: String,
    thread: String,
    labels: Vec<String>,
    raw: Vec<u8>,
}

#[derive(Clone)]
struct DriveFile {
    name: String,
    mime: String,
    parents: Vec<String>,
    data: Vec<u8>,
    modified: i64,
}

/// Streaming pull statistics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamingStats {
    /// Streams opened (successfully authenticated).
    pub streams: usize,
    pub acks: usize,
    /// `modify_deadline_seconds` = 0 requests.
    pub nacks: usize,
    /// Deadline extensions (> 0).
    pub extensions: usize,
    /// Messages delivered (including redeliveries).
    pub delivered: usize,
    /// Streams rejected for authentication.
    pub rejected: usize,
}

/// State of these services.
#[derive(Default)]
pub(super) struct MoreState {
    pub(super) token_subjects: BTreeMap<String, String>,
    mailboxes: BTreeMap<String, Vec<GmailMsg>>,
    calendars: BTreeMap<String, BTreeMap<String, Value>>,
    drive: BTreeMap<String, DriveFile>,
    operations: BTreeMap<String, (usize, Value)>,
    vertex_requests: Vec<Value>,
    stats: StreamingStats,
    next: u64,
}

impl GcpMock {
    /// Deliver a raw RFC 822 message into a Gmail mailbox (INBOX, UNREAD); returns its id.
    pub fn gmail_deliver(&self, user: &str, raw: &str) -> String {
        let mut s = self.state.lock().unwrap();
        let id = next_id(&mut s.more, "m");
        s.more.mailboxes.entry(user.to_string()).or_default().push(GmailMsg { id: id.clone(), thread: format!("t{}", id), labels: vec!["INBOX".into(), "UNREAD".into()], raw: raw.as_bytes().to_vec() });
        id
    }
    /// Raw messages of a mailbox with their labels.
    pub fn gmail_messages(&self, user: &str) -> Vec<(String, Vec<String>)> {
        self.state.lock().unwrap().more.mailboxes.get(user).map(|m| m.iter().map(|g| (String::from_utf8_lossy(&g.raw).into_owned(), g.labels.clone())).collect()).unwrap_or_default()
    }
    /// Events of a calendar.
    pub fn calendar_events(&self, calendar: &str) -> Vec<Value> {
        self.state.lock().unwrap().more.calendars.get(calendar).map(|c| c.values().cloned().collect()).unwrap_or_default()
    }
    /// Store a Drive file; returns its id.
    pub fn put_drive_file(&self, name: &str, mime: &str, data: &[u8]) -> String {
        let mut s = self.state.lock().unwrap();
        let id = next_id(&mut s.more, "f");
        s.more.drive.insert(id.clone(), DriveFile { name: name.into(), mime: mime.into(), parents: vec!["root".into()], data: data.to_vec(), modified: now_millis() });
        id
    }
    /// (name, content) of the Drive files.
    pub fn drive_files(&self) -> Vec<(String, Vec<u8>)> {
        self.state.lock().unwrap().more.drive.values().map(|f| (f.name.clone(), f.data.clone())).collect()
    }
    /// Request bodies received by Vertex AI.
    pub fn vertex_requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().more.vertex_requests.clone()
    }
    /// Streaming pull statistics.
    pub fn streaming_stats(&self) -> StreamingStats {
        self.state.lock().unwrap().more.stats.clone()
    }
    /// The Pub/Sub gRPC endpoint (`127.0.0.1:port`).
    pub fn pubsub_grpc_address(&self) -> String {
        format!("127.0.0.1:{}", self.grpc.local_addr().port())
    }
}

fn next_id(m: &mut MoreState, prefix: &str) -> String {
    m.next += 1;
    format!("{}{:08x}{}", prefix, m.next, random_hex(2))
}

fn ok(v: Value) -> Response {
    Response::json(200, v.to_json())
}

/// Scope / subject checks of a bearer token.
fn token_info(st: &Shared, req: &ServerRequest) -> (String, Option<String>) {
    let tok = req.header("Authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("").to_string();
    let s = st.lock().unwrap();
    (s.ext.token_scopes.get(&tok).cloned().unwrap_or_default(), s.more.token_subjects.get(&tok).cloned())
}

fn has_scope(scopes: &str, any: &[&str]) -> bool {
    scopes.split(' ').any(|s| any.iter().any(|a| s.ends_with(a)))
}

/// Dispatch (`None` = not handled here). Called after the bearer token check.
pub(super) fn handle(st: &Shared, req: &ServerRequest, s: &[&str]) -> Option<Response> {
    let require = st.lock().unwrap().require_auth;
    let scope_check = |any: &[&str]| -> Option<Response> {
        if !require {
            return None;
        }
        let (scopes, _) = token_info(st, req);
        (!has_scope(&scopes, any)).then(|| {
            st.lock().unwrap().auth_failures += 1;
            gerror(403, "PERMISSION_DENIED", "Request had insufficient authentication scopes.")
        })
    };
    Some(match s {
        ["gmail", "v1", "users", user, rest @ ..] => {
            if let Some(r) = scope_check(&["mail.google.com/", "/auth/gmail.send", "/auth/gmail.modify", "/auth/gmail.readonly"]) {
                return Some(r);
            }
            let (_, subject) = token_info(st, req);
            let user = if *user == "me" { subject.clone() } else { Some(user.to_string()) };
            match (subject, user) {
                (Some(_), Some(u)) => gmail(st, req, &u, rest),
                _ if !require => gmail(st, req, &user_or(s), rest),
                _ => gerror(400, "FAILED_PRECONDITION", "Precondition check failed. (service accounts need domain-wide delegation: set a subject)"),
            }
        }
        ["calendar", "v3", rest @ ..] => {
            if let Some(r) = scope_check(&["/auth/calendar", "/auth/calendar.events"]) {
                return Some(r);
            }
            calendar(st, req, rest)
        }
        ["drive", "v3", "files", rest @ ..] => {
            if let Some(r) = scope_check(&["/auth/drive", "/auth/drive.file"]) {
                return Some(r);
            }
            drive(st, req, rest, false)
        }
        ["upload", "drive", "v3", "files", rest @ ..] => {
            if let Some(r) = scope_check(&["/auth/drive", "/auth/drive.file"]) {
                return Some(r);
            }
            drive(st, req, rest, true)
        }
        ["v1", "projects", _, "locations", _, "publishers", _, "models", mv] => vertex(st, req, mv),
        ["v1", "speech:recognize"] | ["v1", "speech:longrunningrecognize"] => speech(st, req, s[1].ends_with("longrunningrecognize")),
        ["v1", "operations", name @ ..] => operation(st, &name.join("/")),
        ["v1", "text:synthesize"] => synthesize(req),
        ["v1", "voices"] => voices(req),
        _ => return None,
    })
}

fn user_or(s: &[&str]) -> String {
    s.get(3).map(|u| u.to_string()).unwrap_or_else(|| "me".into())
}

// ---------------------------------------------------------------------------
// Cloud Storage signed URLs
// ---------------------------------------------------------------------------

/// Serve a V4 signed URL request (`Some` when the request carries `X-Goog-Signature`).
pub(super) fn signed_url(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let params = req.query_params();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let sig = get("X-Goog-Signature")?;
    let deny = |msg: &str| {
        st.lock().unwrap().auth_failures += 1;
        Some(Response::xml(403, format!("<?xml version='1.0' encoding='UTF-8'?><Error><Code>SignatureDoesNotMatch</Code><Message>{}</Message></Error>", msg)))
    };
    let cred = get("X-Goog-Credential").unwrap_or_default();
    let email = cred.split('/').next().unwrap_or("").to_string();
    let Some(key) = st.lock().unwrap().keys.get(&email).cloned() else { return deny("Unknown service account") };
    let date = get("X-Goog-Date").unwrap_or_default();
    let secs = crate::util::parse_iso8601(&date).unwrap_or(0);
    let expires: i64 = get("X-Goog-Expires").and_then(|e| e.parse().ok()).unwrap_or(0);
    if now_secs() > secs + expires {
        return deny("Request has expired");
    }
    let (_, sts) = crate::gcp::storage::v4_string_to_sign(&req.method, req.header("Host").unwrap_or(""), &req.path, &email, secs, expires);
    let raw_sig: Vec<u8> = (0..sig.len() / 2).filter_map(|i| u8::from_str_radix(&sig[i * 2..i * 2 + 2], 16).ok()).collect();
    let digest = crate::util::sha256(sts.as_bytes());
    let valid = matches!(&key, camel_tls::PublicKey::Rsa(r) if r.verify_pkcs1(camel_tls::crypto::hash::HashAlg::Sha256, &digest, &raw_sig));
    if !valid {
        return deny("The request signature we calculated does not match the signature you provided.");
    }
    let path = req.path.trim_start_matches('/');
    let (bucket, object) = path.split_once('/').map(|(b, o)| (percent_decode(b, false), percent_decode(o, false))).unwrap_or_default();
    let s = st.lock().unwrap();
    Some(match s.buckets.get(&bucket).and_then(|b| b.objects.get(&object)) {
        Some(o) => Response::new(200).with_header("Content-Type", o.content_type.clone()).with_body(o.data.clone()),
        None => Response::xml(404, "<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code></Error>"),
    })
}

// ---------------------------------------------------------------------------
// Gmail
// ---------------------------------------------------------------------------

fn header_of(raw: &str, name: &str) -> String {
    raw.split("\r\n\r\n")
        .next()
        .unwrap_or("")
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default()
}

/// Decode the text parts of a (simple) MIME message: (content type, decoded body) pairs.
fn mime_parts(raw: &str) -> Vec<(String, String)> {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let ct = header_of(raw, "Content-Type");
    let decode = |headers: &str, body: &str| -> String {
        if headers.to_ascii_lowercase().contains("content-transfer-encoding: base64") {
            String::from_utf8_lossy(&base64_decode(body.trim()).unwrap_or_default()).into_owned()
        } else {
            body.to_string()
        }
    };
    if let Some(b) = ct.split("boundary=\"").nth(1).and_then(|r| r.split('"').next()) {
        return body
            .split(&format!("--{}", b))
            .filter(|p| !p.trim().is_empty() && !p.starts_with("--"))
            .map(|p| {
                let p = p.trim_start_matches("\r\n");
                let (h, b2) = p.split_once("\r\n\r\n").unwrap_or((p, ""));
                (header_of(&format!("{}\r\n\r\n", h), "Content-Type").split(';').next().unwrap_or("").to_string(), decode(h, b2.trim_end()))
            })
            .collect();
    }
    let ct = ct.split(';').next().unwrap_or("").trim().to_string();
    vec![(if ct.is_empty() { "text/plain".to_string() } else { ct }, decode(head, body))]
}

fn gmail_resource(m: &GmailMsg, format: &str) -> Value {
    let raw = String::from_utf8_lossy(&m.raw).into_owned();
    let mut o = ObjectMap::new();
    o.insert("id".into(), Value::from(m.id.clone()));
    o.insert("threadId".into(), Value::from(m.thread.clone()));
    o.insert("labelIds".into(), Value::from(m.labels.clone()));
    o.insert("snippet".into(), Value::from(mime_parts(&raw).first().map(|(_, b)| b.chars().take(50).collect::<String>()).unwrap_or_default()));
    if format == "raw" {
        o.insert("raw".into(), Value::from(base64url_encode(&m.raw)));
    } else {
        let headers: Vec<Value> = ["From", "To", "Cc", "Subject"].iter().map(|h| (h, header_of(&raw, h))).filter(|(_, v)| !v.is_empty()).map(|(h, v)| obj([("name", Value::from(*h)), ("value", Value::from(v))])).collect();
        let parts: Vec<Value> = mime_parts(&raw).into_iter().map(|(ct, b)| obj([("mimeType", Value::from(ct)), ("body", obj([("data", Value::from(base64url_encode(b.as_bytes()))), ("size", Value::Int(b.len() as i64))]))])).collect();
        o.insert("payload".into(), obj([("mimeType", Value::from("multipart/alternative")), ("headers", Value::Array(headers)), ("parts", Value::Array(parts))]));
    }
    Value::Object(o)
}

fn gmail(st: &Shared, req: &ServerRequest, user: &str, rest: &[&str]) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let not_found = || gerror(404, "NOT_FOUND", "Requested entity was not found.");
    match (req.method.as_str(), rest) {
        ("POST", ["messages", "send"]) => {
            let Some(raw) = body.get("raw").and_then(|r| base64_decode(&r.to_string())) else { return gerror(400, "INVALID_ARGUMENT", "'raw' RFC822 payload message string or uploading message via /upload/* URL required") };
            let text = String::from_utf8_lossy(&raw).into_owned();
            let to = header_of(&text, "To");
            if to.is_empty() {
                return gerror(400, "INVALID_ARGUMENT", "Invalid To header");
            }
            let id = next_id(&mut s.more, "m");
            let msg = GmailMsg { id: id.clone(), thread: format!("t{}", id), labels: vec!["SENT".into()], raw: raw.clone() };
            s.more.mailboxes.entry(user.to_string()).or_default().push(msg.clone());
            for rcpt in to.split(',').chain(header_of(&text, "Cc").split(',')).map(|a| a.trim()).filter(|a| !a.is_empty()) {
                let addr = rcpt.rsplit('<').next().unwrap_or(rcpt).trim_end_matches('>').to_string();
                if addr != user {
                    s.more.mailboxes.entry(addr).or_default().push(GmailMsg { labels: vec!["INBOX".into(), "UNREAD".into()], ..msg.clone() });
                }
            }
            ok(obj([("id", Value::from(id.clone())), ("threadId", Value::from(format!("t{}", id))), ("labelIds", Value::from(vec!["SENT"]))]))
        }
        ("GET", ["messages"]) => {
            let q = req.query_param("q").unwrap_or_default();
            let labels: Vec<String> = req.query_params().into_iter().filter(|(k, _)| k == "labelIds").map(|(_, v)| v).collect();
            let msgs: Vec<Value> = s
                .more
                .mailboxes
                .get(user)
                .map(|mb| {
                    mb.iter()
                        .rev()
                        .filter(|m| labels.iter().all(|l| m.labels.contains(l)) && (!m.labels.contains(&"TRASH".to_string()) || labels.contains(&"TRASH".to_string())))
                        .filter(|m| {
                            let raw = String::from_utf8_lossy(&m.raw).to_ascii_lowercase();
                            q.split_whitespace().all(|term| match term.split_once(':') {
                                Some(("from", v)) => header_of(&raw, "from").contains(&v.to_ascii_lowercase()),
                                Some(("subject", v)) => header_of(&raw, "subject").contains(&v.to_ascii_lowercase()),
                                _ => raw.contains(&term.to_ascii_lowercase()),
                            })
                        })
                        .map(|m| obj([("id", Value::from(m.id.clone())), ("threadId", Value::from(m.thread.clone()))]))
                        .collect()
                })
                .unwrap_or_default();
            let start: usize = req.query_param("pageToken").and_then(|t| t.parse().ok()).unwrap_or(0);
            let max: usize = req.query_param("maxResults").and_then(|m| m.parse().ok()).unwrap_or(100);
            let end = (start + max).min(msgs.len());
            let mut out = ObjectMap::new();
            out.insert("resultSizeEstimate".into(), Value::Int(msgs.len() as i64));
            if end > start {
                out.insert("messages".into(), Value::Array(msgs[start..end].to_vec()));
            }
            if end < msgs.len() {
                out.insert("nextPageToken".into(), Value::from(end.to_string()));
            }
            ok(Value::Object(out))
        }
        (method, ["messages", id, verb @ ..]) => {
            let Some(mb) = s.more.mailboxes.get_mut(user) else { return not_found() };
            let Some(pos) = mb.iter().position(|m| m.id == *id) else { return not_found() };
            match (method, verb) {
                ("GET", []) => ok(gmail_resource(&mb[pos], &req.query_param("format").unwrap_or_else(|| "full".into()))),
                ("DELETE", []) => {
                    mb.remove(pos);
                    Response::new(204)
                }
                ("POST", ["trash"]) => {
                    mb[pos].labels.retain(|l| l != "INBOX");
                    mb[pos].labels.push("TRASH".into());
                    ok(gmail_resource(&mb[pos], "minimal"))
                }
                ("POST", ["untrash"]) => {
                    mb[pos].labels.retain(|l| l != "TRASH");
                    mb[pos].labels.push("INBOX".into());
                    ok(gmail_resource(&mb[pos], "minimal"))
                }
                ("POST", ["modify"]) => {
                    let list = |k: &str| body.get(k).and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default();
                    let (add, remove) = (list("addLabelIds"), list("removeLabelIds"));
                    mb[pos].labels.retain(|l| !remove.contains(l));
                    for l in add {
                        if !mb[pos].labels.contains(&l) {
                            mb[pos].labels.push(l);
                        }
                    }
                    ok(gmail_resource(&mb[pos], "minimal"))
                }
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported message request"),
            }
        }
        ("GET", ["labels"]) => {
            let mut labels: Vec<String> = vec!["INBOX".into(), "SENT".into(), "TRASH".into(), "UNREAD".into()];
            for m in s.more.mailboxes.get(user).into_iter().flatten() {
                for l in &m.labels {
                    if !labels.contains(l) {
                        labels.push(l.clone());
                    }
                }
            }
            ok(obj([("labels", Value::Array(labels.into_iter().map(|l| obj([("id", Value::from(l.clone())), ("name", Value::from(l))])).collect()))]))
        }
        ("GET", ["threads", id]) => {
            let msgs: Vec<Value> = s.more.mailboxes.get(user).into_iter().flatten().filter(|m| m.thread == *id).map(|m| gmail_resource(m, "full")).collect();
            if msgs.is_empty() {
                return not_found();
            }
            ok(obj([("id", Value::from(id.to_string())), ("messages", Value::Array(msgs))]))
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Gmail request"),
    }
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

fn event_start(e: &Value) -> String {
    e.get("start").map(|s| s.get("dateTime").or_else(|| s.get("date")).map(|d| d.to_string()).unwrap_or_default()).unwrap_or_default()
}

fn calendar(st: &Shared, req: &ServerRequest, rest: &[&str]) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let not_found = || gerror(404, "NOT_FOUND", "Not Found");
    match (req.method.as_str(), rest) {
        ("GET", ["users", "me", "calendarList"]) => {
            let mut ids: Vec<String> = s.more.calendars.keys().cloned().collect();
            if !ids.contains(&"primary".to_string()) {
                ids.insert(0, "primary".into());
            }
            ok(obj([("items", Value::Array(ids.into_iter().map(|i| obj([("id", Value::from(i.clone())), ("summary", Value::from(i))])).collect()))]))
        }
        ("GET", ["calendars", cal]) => ok(obj([("id", Value::from(cal.to_string())), ("summary", Value::from(cal.to_string())), ("timeZone", Value::from("UTC"))])),
        ("POST", ["calendars", cal, "events", "quickAdd"]) => {
            let text = req.query_param("text").unwrap_or_default();
            let id = next_id(&mut s.more, "e");
            let e = obj([("id", Value::from(id.clone())), ("summary", Value::from(text)), ("status", Value::from("confirmed")), ("start", obj([("dateTime", Value::from(iso8601_millis(now_millis())))])), ("end", obj([("dateTime", Value::from(iso8601_millis(now_millis() + 3_600_000)))]))]);
            s.more.calendars.entry(cal.to_string()).or_default().insert(id, e.clone());
            ok(e)
        }
        ("POST", ["calendars", cal, "events"]) => {
            if body.get("start").is_none() || body.get("end").is_none() {
                return gerror(400, "INVALID_ARGUMENT", "Missing end time.");
            }
            let id = next_id(&mut s.more, "e");
            let mut e = body.as_object().cloned().unwrap_or_default();
            e.insert("id".into(), Value::from(id.clone()));
            e.insert("status".into(), Value::from("confirmed"));
            e.insert("htmlLink".into(), Value::from(format!("https://calendar.google.com/event?eid={}", id)));
            s.more.calendars.entry(cal.to_string()).or_default().insert(id, Value::Object(e.clone()));
            ok(Value::Object(e))
        }
        ("GET", ["calendars", cal, "events"]) => {
            let q = req.query_param("q").unwrap_or_default().to_ascii_lowercase();
            let (tmin, tmax) = (req.query_param("timeMin"), req.query_param("timeMax"));
            let mut items: Vec<Value> = s
                .more
                .calendars
                .get(*cal)
                .map(|c| c.values().filter(|e| e.to_json().to_ascii_lowercase().contains(&q)).filter(|e| tmin.as_ref().is_none_or(|t| event_start(e) >= *t) && tmax.as_ref().is_none_or(|t| event_start(e) < *t)).cloned().collect())
                .unwrap_or_default();
            if req.query_param("orderBy").as_deref() == Some("startTime") {
                items.sort_by_key(event_start);
            }
            let start: usize = req.query_param("pageToken").and_then(|t| t.parse().ok()).unwrap_or(0);
            let max: usize = req.query_param("maxResults").and_then(|m| m.parse().ok()).unwrap_or(250);
            let end = (start + max).min(items.len());
            let mut out = ObjectMap::new();
            out.insert("kind".into(), Value::from("calendar#events"));
            out.insert("items".into(), Value::Array(items.get(start..end).map(|x| x.to_vec()).unwrap_or_default()));
            if end < items.len() {
                out.insert("nextPageToken".into(), Value::from(end.to_string()));
            }
            ok(Value::Object(out))
        }
        (m, ["calendars", cal, "events", id]) => {
            let Some(c) = s.more.calendars.get_mut(*cal) else { return not_found() };
            match m {
                "GET" => c.get(*id).cloned().map(ok).unwrap_or_else(not_found),
                "DELETE" => match c.remove(*id) {
                    Some(_) => Response::new(204),
                    None => not_found(),
                },
                "PUT" | "PATCH" => {
                    let Some(existing) = c.get_mut(*id) else { return not_found() };
                    let mut e = if m == "PUT" { ObjectMap::new() } else { existing.as_object().cloned().unwrap_or_default() };
                    for (k, v) in body.as_object().cloned().unwrap_or_default() {
                        e.insert(k, v);
                    }
                    e.insert("id".into(), Value::from(id.to_string()));
                    e.insert("status".into(), Value::from("confirmed"));
                    *existing = Value::Object(e);
                    ok(existing.clone())
                }
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported event request"),
            }
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Calendar request"),
    }
}

// ---------------------------------------------------------------------------
// Drive
// ---------------------------------------------------------------------------

fn drive_resource(id: &str, f: &DriveFile) -> Value {
    obj([
        ("kind", Value::from("drive#file")),
        ("id", Value::from(id)),
        ("name", Value::from(f.name.clone())),
        ("mimeType", Value::from(f.mime.clone())),
        ("parents", Value::from(f.parents.clone())),
        ("size", Value::from(f.data.len().to_string())),
        ("modifiedTime", Value::from(iso8601_millis(f.modified))),
    ])
}

/// Evaluate a (small subset of the) Drive query language.
fn drive_matches(q: &str, f: &DriveFile) -> bool {
    q.split(" and ").map(|c| c.trim()).filter(|c| !c.is_empty()).all(|c| {
        let quoted = |s: &str| s.trim().trim_matches('\'').to_string();
        if let Some(v) = c.strip_prefix("name = ") {
            f.name == quoted(v)
        } else if let Some(v) = c.strip_prefix("name contains ") {
            f.name.contains(&quoted(v))
        } else if let Some(v) = c.strip_prefix("mimeType = ") {
            f.mime == quoted(v)
        } else if let Some((p, _)) = c.split_once(" in parents") {
            f.parents.contains(&quoted(p))
        } else {
            c.starts_with("trashed")
        }
    })
}

fn multipart_upload(req: &ServerRequest) -> Option<(Value, Vec<u8>, String)> {
    let ct = req.header("Content-Type")?;
    let boundary = ct.split("boundary=").nth(1)?.trim_matches('"').to_string();
    let delim = format!("--{}", boundary).into_bytes();
    let body = &req.body;
    let find = |from: usize, pat: &[u8]| (from..body.len().saturating_sub(pat.len() - 1)).find(|&i| &body[i..i + pat.len()] == pat);
    let mut parts = Vec::new();
    let mut pos = find(0, &delim)? + delim.len();
    while let Some(next) = find(pos, &delim) {
        let part = &body[pos..next];
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let part = part.strip_suffix(b"\r\n").unwrap_or(part);
        let split = (0..part.len().saturating_sub(3)).find(|&i| &part[i..i + 4] == b"\r\n\r\n")?;
        parts.push((String::from_utf8_lossy(&part[..split]).into_owned(), part[split + 4..].to_vec()));
        pos = next + delim.len();
    }
    if parts.len() != 2 {
        return None;
    }
    let meta = parse_json(&parts[0].1).ok()?;
    let mime = parts[1].0.lines().find_map(|l| l.strip_prefix("Content-Type:").map(|v| v.trim().to_string())).unwrap_or_else(|| "application/octet-stream".into());
    Some((meta, parts[1].1.clone(), mime))
}

fn drive(st: &Shared, req: &ServerRequest, rest: &[&str], upload: bool) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let not_found = || gerror(404, "NOT_FOUND", "File not found.");
    match (req.method.as_str(), rest, upload) {
        ("POST", [], true) => {
            let Some((meta, data, mime)) = multipart_upload(req) else { return gerror(400, "INVALID_ARGUMENT", "Invalid multipart upload") };
            let id = next_id(&mut s.more, "f");
            let parents = meta.get("parents").and_then(|p| p.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_else(|| vec!["root".to_string()]);
            let f = DriveFile { name: meta.get("name").map(|n| n.to_string()).unwrap_or_else(|| "Untitled".into()), mime: meta.get("mimeType").map(|m| m.to_string()).unwrap_or(mime), parents, data, modified: now_millis() };
            let r = drive_resource(&id, &f);
            s.more.drive.insert(id, f);
            ok(r)
        }
        ("PATCH", [id], true) => match s.more.drive.get_mut(*id) {
            Some(f) => {
                f.data = req.body.clone();
                f.modified = now_millis();
                ok(drive_resource(id, f))
            }
            None => not_found(),
        },
        ("GET", [], false) => {
            let q = req.query_param("q").unwrap_or_default();
            let files: Vec<Value> = s.more.drive.iter().filter(|(_, f)| drive_matches(&q, f)).map(|(id, f)| drive_resource(id, f)).collect();
            let start: usize = req.query_param("pageToken").and_then(|t| t.parse().ok()).unwrap_or(0);
            let max: usize = req.query_param("pageSize").and_then(|m| m.parse().ok()).unwrap_or(100);
            let end = (start + max).min(files.len());
            let mut out = ObjectMap::new();
            out.insert("files".into(), Value::Array(files.get(start..end).map(|x| x.to_vec()).unwrap_or_default()));
            if end < files.len() {
                out.insert("nextPageToken".into(), Value::from(end.to_string()));
            }
            ok(Value::Object(out))
        }
        ("GET", [id], false) => match s.more.drive.get(*id) {
            Some(f) if req.query_param("alt").as_deref() == Some("media") => Response::new(200).with_header("Content-Type", f.mime.clone()).with_body(f.data.clone()),
            Some(f) => ok(drive_resource(id, f)),
            None => not_found(),
        },
        ("DELETE", [id], false) => match s.more.drive.remove(*id) {
            Some(_) => Response::new(204),
            None => not_found(),
        },
        ("POST", [id, "copy"], false) => {
            let Some(src) = s.more.drive.get(*id).cloned() else { return not_found() };
            let nid = next_id(&mut s.more, "f");
            let f = DriveFile { name: body.get("name").map(|n| n.to_string()).unwrap_or_else(|| format!("Copy of {}", src.name)), modified: now_millis(), ..src };
            let r = drive_resource(&nid, &f);
            s.more.drive.insert(nid, f);
            ok(r)
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Drive request"),
    }
}

// ---------------------------------------------------------------------------
// Vertex AI / Speech / TTS
// ---------------------------------------------------------------------------

fn parts_text(parts: Option<&Value>) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut media = Vec::new();
    for p in parts.and_then(|p| p.as_array()).cloned().unwrap_or_default() {
        if let Some(t) = p.get("text") {
            text.push(t.to_string());
        }
        if let Some(d) = p.get("inlineData") {
            let n = d.get("data").and_then(|x| base64_decode(&x.to_string())).map(|b| b.len()).unwrap_or(0);
            media.push(format!("{}, {} bytes", d.get("mimeType").map(|m| m.to_string()).unwrap_or_default(), n));
        }
        if let Some(d) = p.get("fileData") {
            media.push(d.get("fileUri").map(|m| m.to_string()).unwrap_or_default());
        }
    }
    (text.join(" "), media)
}

fn vertex(st: &Shared, req: &ServerRequest, model_verb: &str) -> Response {
    let (model, verb) = model_verb.split_once(':').unwrap_or((model_verb, ""));
    if model.contains("missing") {
        return gerror(404, "NOT_FOUND", &format!("Publisher Model `{}` not found.", model));
    }
    let body = parse_json(&req.body).unwrap_or_default();
    st.lock().unwrap().more.vertex_requests.push(body.clone());
    let contents = body.get("contents").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    if contents.is_empty() {
        return gerror(400, "INVALID_ARGUMENT", "Unable to submit request because it has an empty contents field.");
    }
    let last_user = contents.iter().rev().find(|c| c.get("role").map(|r| r.to_string()).as_deref() != Some("model")).cloned().unwrap_or_default();
    let (text, media) = parts_text(last_user.get("parts"));
    let all_words: usize = contents.iter().map(|c| parts_text(c.get("parts")).0.split_whitespace().count()).sum();
    match verb {
        "countTokens" => ok(obj([("totalTokens", Value::Int(all_words as i64)), ("totalBillableCharacters", Value::Int(text.len() as i64))])),
        "generateContent" => {
            let mut reply = format!("Echo: {}", text);
            if !media.is_empty() {
                reply.push_str(&format!(" (+{})", media.join("; ")));
            }
            if let Some(si) = body.get("systemInstruction") {
                reply = format!("[{}] {}", parts_text(si.get("parts")).0, reply);
            }
            let max = body.get("generationConfig").and_then(|g| g.get("maxOutputTokens")).and_then(|m| m.as_i64());
            let (reply, finish) = match max {
                Some(n) if (n as usize) < reply.split_whitespace().count() => (reply.split_whitespace().take(n as usize).collect::<Vec<_>>().join(" "), "MAX_TOKENS"),
                _ => (reply, "STOP"),
            };
            let out = reply.split_whitespace().count() as i64;
            ok(obj([
                ("candidates", Value::Array(vec![obj([("content", obj([("role", Value::from("model")), ("parts", Value::Array(vec![obj([("text", Value::from(reply))])]))])), ("finishReason", Value::from(finish)), ("safetyRatings", Value::Array(vec![]))])])),
                ("usageMetadata", obj([("promptTokenCount", Value::Int(all_words as i64)), ("candidatesTokenCount", Value::Int(out)), ("totalTokenCount", Value::Int(all_words as i64 + out))])),
                ("modelVersion", Value::from(model)),
            ]))
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported Vertex AI method"),
    }
}

fn speech(st: &Shared, req: &ServerRequest, long_running: bool) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let cfg = body.get("config").cloned().unwrap_or_default();
    if cfg.get("languageCode").is_none_or(|l| l.to_string().is_empty()) {
        return gerror(400, "INVALID_ARGUMENT", "Invalid recognition 'config': missing language_code.");
    }
    let audio = body.get("audio").cloned().unwrap_or_default();
    let data = if let Some(c) = audio.get("content") {
        base64_decode(&c.to_string()).unwrap_or_default()
    } else if let Some(uri) = audio.get("uri").map(|u| u.to_string()) {
        let (bucket, obj_name) = uri.trim_start_matches("gs://").split_once('/').map(|(b, o)| (b.to_string(), o.to_string())).unwrap_or_default();
        match st.lock().unwrap().buckets.get(&bucket).and_then(|b| b.objects.get(&obj_name)) {
            Some(o) => o.data.clone(),
            None => return gerror(400, "INVALID_ARGUMENT", &format!("Requested entity was not found: {}", uri)),
        }
    } else {
        return gerror(400, "INVALID_ARGUMENT", "RecognitionAudio not set.");
    };
    let text = String::from_utf8_lossy(&data).into_owned();
    let results = Value::Array(
        text.split(". ")
            .filter(|s| !s.trim().is_empty())
            .map(|seg| obj([("alternatives", Value::Array(vec![obj([("transcript", Value::from(seg.trim())), ("confidence", Value::Float(0.92))])])), ("languageCode", cfg.get("languageCode").cloned().unwrap_or_default())]))
            .collect(),
    );
    if long_running {
        let mut s = st.lock().unwrap();
        let name = next_id(&mut s.more, "op");
        s.more.operations.insert(name.clone(), (0, results));
        return ok(obj([("name", Value::from(name))]));
    }
    ok(obj([("results", results), ("totalBilledTime", Value::from("1s"))]))
}

fn operation(st: &Shared, name: &str) -> Response {
    let mut s = st.lock().unwrap();
    let Some((polls, results)) = s.more.operations.get_mut(name) else { return gerror(404, "NOT_FOUND", "Operation not found") };
    *polls += 1;
    if *polls < 2 {
        return ok(obj([("name", Value::from(name)), ("done", Value::Bool(false))]));
    }
    ok(obj([("name", Value::from(name)), ("done", Value::Bool(true)), ("response", obj([("@type", Value::from("type.googleapis.com/google.cloud.speech.v1.LongRunningRecognizeResponse")), ("results", results.clone())]))]))
}

fn synthesize(req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let input = body.get("input").cloned().unwrap_or_default();
    let text = input.get("text").or_else(|| input.get("ssml")).map(|t| t.to_string()).unwrap_or_default();
    if text.is_empty() {
        return gerror(400, "INVALID_ARGUMENT", "Either `input.text` or `input.ssml` is required.");
    }
    let voice = body.get("voice").cloned().unwrap_or_default();
    if voice.get("languageCode").is_none() {
        return gerror(400, "INVALID_ARGUMENT", "Voice language code is required.");
    }
    let enc = body.get("audioConfig").and_then(|a| a.get("audioEncoding")).map(|e| e.to_string()).unwrap_or_default();
    if enc.is_empty() {
        return gerror(400, "INVALID_ARGUMENT", "AudioConfig audio encoding is required.");
    }
    let vname = voice.get("name").map(|n| n.to_string()).unwrap_or_else(|| voice.get("languageCode").map(|l| l.to_string()).unwrap_or_default());
    let kind = if input.get("ssml").is_some() { "ssml" } else { "text" };
    ok(obj([("audioContent", Value::from(base64_encode(format!("MOCKAUDIO:{}:{}:{}:{}", vname, enc, kind, text).as_bytes())))]))
}

fn voices(req: &ServerRequest) -> Response {
    let lang = req.query_param("languageCode").unwrap_or_default();
    let all = [("en-US-Standard-A", "en-US", "MALE"), ("en-US-Neural2-C", "en-US", "FEMALE"), ("it-IT-Standard-A", "it-IT", "FEMALE")];
    ok(obj([(
        "voices",
        Value::Array(all.iter().filter(|(_, l, _)| lang.is_empty() || *l == lang).map(|(n, l, g)| obj([("name", Value::from(*n)), ("languageCodes", Value::from(vec![*l])), ("ssmlGender", Value::from(*g)), ("naturalSampleRateHertz", Value::Int(24000))])).collect()),
    )]))
}

// ---------------------------------------------------------------------------
// Pub/Sub streaming pull (gRPC)
// ---------------------------------------------------------------------------

/// Start the gRPC server of the emulator.
pub(super) fn start_grpc(st: Shared) -> std::io::Result<GrpcServer> {
    let service = move |call: &ServerCall| -> Status {
        if call.service() != "google.pubsub.v1.Subscriber" || call.method() != "StreamingPull" {
            return Status::new(Code::Unimplemented, format!("Method not found: {}/{}", call.service(), call.method()));
        }
        streaming_pull(&st, call)
    };
    GrpcServer::bind("127.0.0.1:0", camel_grpc::h2::H2Config::default(), None, 4 << 20, Arc::new(service)).map_err(|e| std::io::Error::other(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<Value, Status> {
    pool().decode("google.pubsub.v1.StreamingPullRequest", bytes).map_err(|e| Status::new(Code::InvalidArgument, e.to_string()))
}

fn ids(v: &Value, k: &str) -> Vec<String> {
    v.get(k).and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default()
}

/// Apply acks / deadline modifications of a request to a subscription.
fn apply(st: &Shared, sub: &str, req: &Value) {
    let acks = ids(req, "ack_ids");
    let mod_ids = ids(req, "modify_deadline_ack_ids");
    let secs: Vec<i64> = req.get("modify_deadline_seconds").and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.as_i64().unwrap_or(0)).collect()).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let now = now_millis();
    let (mut nacks, mut ext) = (0, 0);
    if let Some(subscription) = s.subscriptions.get_mut(sub) {
        subscription.messages.retain(|m| m.ack_id.as_ref().is_none_or(|a| !acks.contains(a)));
        for (i, id) in mod_ids.iter().enumerate() {
            let d = secs.get(i).or(secs.last()).copied().unwrap_or(0);
            if let Some(m) = subscription.messages.iter_mut().find(|m| m.ack_id.as_deref() == Some(id.as_str())) {
                m.visible_at = now + d * 1000;
                if d == 0 {
                    m.ack_id = None;
                    nacks += 1;
                } else {
                    ext += 1;
                }
            }
        }
    }
    s.more.stats.acks += acks.len();
    s.more.stats.nacks += nacks;
    s.more.stats.extensions += ext;
}

fn streaming_pull(st: &Shared, call: &ServerCall) -> Status {
    {
        let mut s = st.lock().unwrap();
        if s.require_auth {
            let tok = call.metadata().iter().find(|(k, _)| k == "authorization").and_then(|(_, v)| v.strip_prefix("Bearer ")).unwrap_or("").to_string();
            if !s.tokens.get(&tok).is_some_and(|exp| *exp > now_secs()) {
                s.more.stats.rejected += 1;
                s.auth_failures += 1;
                return Status::new(Code::Unauthenticated, "Request had invalid authentication credentials. Expected OAuth 2 access token.");
            }
        }
    }
    let first = match call.recv() {
        Ok(Some(b)) => match decode(&b) {
            Ok(v) => v,
            Err(s) => return s,
        },
        Ok(None) => return Status::ok(),
        Err(s) => return s,
    };
    let sub = first.get("subscription").map(|v| v.to_string()).unwrap_or_default();
    if !st.lock().unwrap().subscriptions.contains_key(&sub) {
        return Status::new(Code::NotFound, format!("Resource not found (resource={}).", sub.rsplit('/').next().unwrap_or("")));
    }
    let deadline = first.get("stream_ack_deadline_seconds").and_then(|d| d.as_i64()).filter(|d| *d > 0).unwrap_or(10);
    let max_out = first.get("max_outstanding_messages").and_then(|d| d.as_i64()).filter(|d| *d > 0).unwrap_or(1000) as usize;
    st.lock().unwrap().more.stats.streams += 1;
    apply(st, &sub, &first);
    let closed = Arc::new(AtomicBool::new(false));
    let reader = {
        let (call, st, sub, closed) = (call.clone(), st.clone(), sub.clone(), closed.clone());
        std::thread::spawn(move || {
            loop {
                match call.recv() {
                    Ok(Some(b)) => {
                        if let Ok(v) = decode(&b) {
                            apply(&st, &sub, &v);
                        }
                    }
                    _ => break,
                }
            }
            closed.store(true, Ordering::SeqCst);
        })
    };
    let mut leased: Vec<String> = Vec::new();
    while !closed.load(Ordering::SeqCst) && !call.is_cancelled() {
        let batch: Vec<Value> = {
            let mut s = st.lock().unwrap();
            let now = now_millis();
            let Some(subscription) = s.subscriptions.get_mut(&sub) else { break };
            // outstanding = messages leased by this stream and not acked / expired / nacked
            leased.retain(|id| subscription.messages.iter().any(|m| m.ack_id.as_deref() == Some(id.as_str()) && m.visible_at > now));
            let room = max_out.saturating_sub(leased.len());
            let mut out = Vec::new();
            for m in subscription.messages.iter_mut().filter(|m| m.visible_at <= now).take(room) {
                let ack = format!("sack-{}-{}", m.id, random_hex(6));
                m.ack_id = Some(ack.clone());
                m.visible_at = now + deadline * 1000;
                m.attempts += 1;
                leased.push(ack.clone());
                let mut msg = ObjectMap::new();
                msg.insert("data".into(), Value::Bytes(base64_decode(&m.data).unwrap_or_default()));
                msg.insert("attributes".into(), m.attributes.clone());
                msg.insert("message_id".into(), Value::from(m.id.clone()));
                msg.insert("publish_time".into(), Value::from(iso8601_millis(m.publish_time)));
                if !m.ordering_key.is_empty() {
                    msg.insert("ordering_key".into(), Value::from(m.ordering_key.clone()));
                }
                out.push(obj([("ack_id", Value::from(ack)), ("message", Value::Object(msg)), ("delivery_attempt", Value::Int(m.attempts))]));
            }
            s.more.stats.delivered += out.len();
            out
        };
        if batch.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let resp = match pool().encode("google.pubsub.v1.StreamingPullResponse", &obj([("received_messages", Value::Array(batch))])) {
            Ok(b) => b,
            Err(e) => return Status::new(Code::Internal, e.to_string()),
        };
        if call.send(&resp).is_err() {
            break;
        }
    }
    let _ = reader.join();
    Status::ok()
}
