//! Google Workspace components over their REST APIs, authenticated with a service account and
//! (for user data) domain-wide delegation: `delegate` (alias `subject`) is the user to
//! impersonate (the JWT `sub` claim); `scopes` (comma separated) overrides the default scopes.
//! Options can also be given as `Camel<Component>.<option>` headers (Camel's API-component
//! convention, e.g. `CamelGoogleMail.q`).
//!
//! **`google-mail://apiName/methodName`** (Gmail v1, default scope `https://mail.google.com/`,
//! `userId` default `me`):
//! * `messages/send`: a map body `{to, cc, bcc, from, subject, text, html, replyTo}` (lists or
//!   comma separated addresses) is turned into a MIME message; a text body is sent as the text
//!   of a message addressed by the `to` / `subject` / `cc` / `bcc` / `from` options, or as a raw
//!   RFC 822 message when no `to` is set; body = the sent message resource (`id`, `threadId`).
//! * `messages/list` (`q`, `labelIds`, `maxResults` (one page when set, else all pages),
//!   `includeSpamTrash`): body = list of `{id, threadId}`.
//! * `messages/get` (`id`, `format` default `full`): body = message resource; the plain-text
//!   part is decoded into `CamelGoogleMail.text` and the `Subject` / `From` / `To` headers into
//!   `CamelGoogleMail.subject` / `.from` / `.to`.
//! * `messages/delete`, `messages/trash`, `messages/untrash` (`id`), `messages/modify` (`id`,
//!   `addLabelIds`, `removeLabelIds`), `labels/list` (body = labels), `threads/get` (`id`).
//!
//! **`google-calendar://apiName/methodName`** (Calendar v3, scope
//! `https://www.googleapis.com/auth/calendar`, `calendarId` default `primary`):
//! `events/list` (`timeMin`, `timeMax`, `q`, `maxResults`, `singleEvents`, `orderBy`; body =
//! events, all pages), `events/insert` (`content` header or map body; body = created event),
//! `events/get`, `events/update` (`eventId`, `content`), `events/patch`, `events/delete`,
//! `events/quickAdd` (`text`), `calendarList/list`, `calendars/get`.
//!
//! **`google-drive://apiName/methodName`** (Drive v3, scope
//! `https://www.googleapis.com/auth/drive`): `drive-files/list` (`q`, `pageSize`, `orderBy`;
//! body = files, all pages), `drive-files/get` (`fileId`; metadata), `drive-files/download`
//! (`fileId`; body = bytes), `drive-files/insert` / `drive-files/create` (multipart upload of
//! the body with `name` (alias `title`), `mimeType`, `parents`; body = created file),
//! `drive-files/update` (`fileId`; new content), `drive-files/copy` (`fileId`, `name`),
//! `drive-files/delete` (`fileId`).

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::GcpConfig;
use crate::endpoint::CloudService;
use crate::http::{Request, Url};
use crate::util::{base64_decode, base64url_encode, kinds, obj, parse_json, random_hex, str_list, uri_encode, Opts};

/// Gmail default scope.
pub const MAIL_SCOPE: &str = "https://mail.google.com/";
/// Calendar scope.
pub const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
/// Drive scope.
pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Api {
    Mail,
    Calendar,
    Drive,
}

const MAIL_OPTIONS: &[&str] = &["userId", "to", "cc", "bcc", "from", "subject", "replyTo", "id", "q", "labelIds", "maxResults", "includeSpamTrash", "format", "addLabelIds", "removeLabelIds"];
const CALENDAR_OPTIONS: &[&str] = &["calendarId", "eventId", "timeMin", "timeMax", "q", "maxResults", "singleEvents", "orderBy", "text", "sendUpdates"];
const DRIVE_OPTIONS: &[&str] = &["fileId", "q", "pageSize", "orderBy", "name", "title", "mimeType", "parents", "fields"];

/// A Workspace API endpoint.
pub struct WorkspaceService {
    pub gcp: GcpConfig,
    api: Api,
    pub api_name: String,
    pub method: String,
    pub scope: String,
    options: BTreeMap<String, String>,
}

fn create_for(api: Api, remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (scheme, supported, names, default_scope): (&str, &[&str], &[&str], &str) = match api {
        Api::Mail => (
            "google-mail",
            &["messages/send", "messages/list", "messages/get", "messages/delete", "messages/trash", "messages/untrash", "messages/modify", "labels/list", "threads/get"],
            MAIL_OPTIONS,
            MAIL_SCOPE,
        ),
        Api::Calendar => (
            "google-calendar",
            &["events/list", "events/insert", "events/get", "events/update", "events/patch", "events/delete", "events/quickAdd", "calendarList/list", "calendars/get"],
            CALENDAR_OPTIONS,
            CALENDAR_SCOPE,
        ),
        Api::Drive => (
            "google-drive",
            &["drive-files/list", "drive-files/get", "drive-files/download", "drive-files/insert", "drive-files/create", "drive-files/update", "drive-files/copy", "drive-files/delete"],
            DRIVE_OPTIONS,
            DRIVE_SCOPE,
        ),
    };
    let path = remaining.trim_matches('/');
    if !supported.contains(&path) {
        return Err(CamelError::illegal_argument(format!("Unsupported {} api/method: {} (supported: {})", scheme, path, supported.join(", "))));
    }
    let (api_name, method) = path.split_once('/').unwrap_or((path, ""));
    let gcp = GcpConfig::from_opts(o)?;
    let scope = o.get("scopes").map(|s| str_list(&Value::from(s)).join(" ")).unwrap_or_else(|| default_scope.to_string());
    let mut options = BTreeMap::new();
    for n in names {
        if let Some(v) = o.get(n) {
            options.insert(n.to_string(), v);
        }
    }
    o.ignore(&["clientId", "clientSecret", "accessToken", "refreshToken", "applicationName", "inBody", "content", "splitResult"]);
    Ok(Arc::new(WorkspaceService { gcp, api, api_name: api_name.to_string(), method: method.to_string(), scope, options }))
}

/// Factory for `google-mail`.
pub fn create_mail(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    create_for(Api::Mail, remaining, o)
}

/// Factory for `google-calendar`.
pub fn create_calendar(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    create_for(Api::Calendar, remaining, o)
}

/// Factory for `google-drive`.
pub fn create_drive(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    create_for(Api::Drive, remaining, o)
}

fn addresses(v: &Value) -> String {
    str_list(v).join(", ")
}

/// Build a MIME message (`text` and/or `html`) from a message map.
pub fn build_mime(m: &Value) -> Vec<u8> {
    let g = |k: &str| m.get(k).filter(|v| !v.is_null());
    let mut out = String::new();
    for (k, h) in [("from", "From"), ("to", "To"), ("cc", "Cc"), ("bcc", "Bcc"), ("replyTo", "Reply-To")] {
        if let Some(v) = g(k) {
            out.push_str(&format!("{}: {}\r\n", h, addresses(v)));
        }
    }
    if let Some(s) = g("subject") {
        let s = s.to_string();
        if s.is_ascii() {
            out.push_str(&format!("Subject: {}\r\n", s));
        } else {
            out.push_str(&format!("Subject: =?UTF-8?B?{}?=\r\n", crate::util::base64_encode(s.as_bytes())));
        }
    }
    out.push_str("MIME-Version: 1.0\r\n");
    let text = g("text").or_else(|| g("body")).map(|t| t.to_string());
    let html = g("html").map(|t| t.to_string());
    let part = |ct: &str, body: &str| format!("Content-Type: {}; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", ct, crate::util::base64_encode(body.as_bytes()));
    match (text, html) {
        (Some(t), Some(h)) => {
            let b = format!("camel-{}", random_hex(8));
            out.push_str(&format!("Content-Type: multipart/alternative; boundary=\"{}\"\r\n\r\n", b));
            out.push_str(&format!("--{}\r\n{}--{}\r\n{}--{}--\r\n", b, part("text/plain", &t), b, part("text/html", &h), b));
        }
        (None, Some(h)) => out.push_str(&part("text/html", &h)),
        (t, None) => out.push_str(&part("text/plain", &t.unwrap_or_default())),
    }
    out.into_bytes()
}

/// Decode the first `text/plain` part of a Gmail message resource.
pub fn message_text(msg: &Value) -> Option<String> {
    fn walk(p: &Value) -> Option<String> {
        if p.get("mimeType").map(|m| m.to_string()).as_deref() == Some("text/plain") {
            let data = p.get("body").and_then(|b| b.get("data")).map(|d| d.to_string())?;
            return base64_decode(&data).map(|b| String::from_utf8_lossy(&b).into_owned());
        }
        p.get("parts").and_then(|ps| ps.as_array()).and_then(|ps| ps.iter().find_map(walk))
    }
    msg.get("payload").and_then(walk)
}

impl WorkspaceService {
    fn prefix(&self) -> &'static str {
        match self.api {
            Api::Mail => "CamelGoogleMail.",
            Api::Calendar => "CamelGoogleCalendar.",
            Api::Drive => "CamelGoogleDrive.",
        }
    }

    fn opt(&self, ex: &Exchange, name: &str) -> Option<String> {
        crate::util::header_str(ex, &format!("{}{}", self.prefix(), name)).or_else(|| self.options.get(name).cloned())
    }

    fn opt_value(&self, ex: &Exchange, name: &str) -> Option<Value> {
        ex.header(&format!("{}{}", self.prefix(), name)).filter(|v| !v.is_null()).cloned().or_else(|| self.options.get(name).map(|s| Value::from(s.clone())))
    }

    fn need(&self, ex: &Exchange, name: &str) -> Result<String> {
        self.opt(ex, name).ok_or_else(|| CamelError::illegal_argument(format!("{}/{} requires the {} option (or the {}{} header)", self.api_name, self.method, name, self.prefix(), name)))
    }

    fn send(&self, req: Request) -> Result<crate::http::Response> {
        self.gcp.send_scoped(req, kinds::GOOGLE_JSON, &self.scope)
    }

    fn call(&self, method: &str, url: &str, body: Option<&Value>) -> Result<Value> {
        let mut req = Request::new(method, Url::parse(url)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        parse_json(&self.send(req)?.body)
    }

    /// GET all pages of a list (`items` field, `nextPageToken`), or one page when `single`.
    fn paged(&self, url: &str, query: Vec<(String, String)>, field: &str, single: bool) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = query.clone();
            if let Some(t) = &token {
                q.push(("pageToken".into(), t.clone()));
            }
            let full = if q.is_empty() { url.to_string() } else { format!("{}?{}", url, crate::util::encode_form(&q)) };
            let v = self.call("GET", &full, None)?;
            out.extend(v.get(field).and_then(|i| i.as_array()).cloned().unwrap_or_default());
            token = v.get("nextPageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() || single {
                return Ok(out);
            }
        }
    }

    fn query(&self, ex: &Exchange, names: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut q = Vec::new();
        for (opt, param) in names {
            if let Some(v) = self.opt_value(ex, opt) {
                for item in if matches!(v, Value::Array(_)) { str_list(&v) } else { vec![v.to_string()] } {
                    q.push((param.to_string(), item));
                }
            }
        }
        q
    }

    fn content(&self, ex: &Exchange) -> Result<Value> {
        let v = ex.header(&format!("{}content", self.prefix())).cloned().unwrap_or_else(|| ex.body().clone());
        match v {
            Value::Object(_) => Ok(v),
            Value::String(_) | Value::Bytes(_) => parse_json(&v.to_bytes()),
            _ => Err(CamelError::illegal_argument(format!("{}/{} needs a content map (body or {}content header)", self.api_name, self.method, self.prefix()))),
        }
    }

    fn mail(&self, ex: &mut Exchange) -> Result<()> {
        let user = self.opt(ex, "userId").unwrap_or_else(|| "me".into());
        let base = format!("{}/gmail/v1/users/{}", self.gcp.base("https://gmail.googleapis.com"), uri_encode(&user, false));
        let id_url = |ex: &Exchange, coll: &str| -> Result<String> { Ok(format!("{}/{}/{}", base, coll, uri_encode(&self.need(ex, "id")?, false))) };
        match (self.api_name.as_str(), self.method.as_str()) {
            ("messages", "send") => {
                let raw = match ex.body() {
                    Value::Object(_) => build_mime(ex.body()),
                    other => match self.opt_value(ex, "to") {
                        Some(to) => {
                            let mut m = ObjectMap::new();
                            m.insert("to".into(), to);
                            for k in ["cc", "bcc", "from", "subject", "replyTo"] {
                                if let Some(v) = self.opt_value(ex, k) {
                                    m.insert(k.into(), v);
                                }
                            }
                            m.insert("text".into(), Value::from(other.to_string()));
                            build_mime(&Value::Object(m))
                        }
                        None => other.to_bytes(),
                    },
                };
                if raw.is_empty() {
                    return Err(CamelError::illegal_argument("messages/send needs a message (map body, text body with the to option, or a raw RFC 822 body)"));
                }
                let r = self.call("POST", &format!("{}/messages/send", base), Some(&obj([("raw", Value::from(base64url_encode(&raw)))])))?;
                ex.set_header(&format!("{}id", self.prefix()), r.get("id").cloned().unwrap_or_default());
                ex.set_body(r);
            }
            ("messages", "list") => {
                let q = self.query(ex, &[("q", "q"), ("labelIds", "labelIds"), ("maxResults", "maxResults"), ("includeSpamTrash", "includeSpamTrash")]);
                let single = self.opt(ex, "maxResults").is_some();
                ex.set_body(Value::Array(self.paged(&format!("{}/messages", base), q, "messages", single)?));
            }
            ("messages", "get") => {
                let format = self.opt(ex, "format").unwrap_or_else(|| "full".into());
                let r = self.call("GET", &format!("{}?format={}", id_url(ex, "messages")?, uri_encode(&format, false)), None)?;
                if let Some(t) = message_text(&r) {
                    ex.set_header(&format!("{}text", self.prefix()), t);
                }
                for h in r.get("payload").and_then(|p| p.get("headers")).and_then(|h| h.as_array()).cloned().unwrap_or_default() {
                    let name = h.get("name").map(|n| n.to_string()).unwrap_or_default().to_ascii_lowercase();
                    if matches!(name.as_str(), "subject" | "from" | "to") {
                        ex.set_header(&format!("{}{}", self.prefix(), name), h.get("value").cloned().unwrap_or_default());
                    }
                }
                ex.set_body(r);
            }
            ("messages", "delete") => {
                self.send(Request::new("DELETE", Url::parse(&id_url(ex, "messages")?)?))?;
            }
            ("messages", "trash") | ("messages", "untrash") => {
                let r = self.call("POST", &format!("{}/{}", id_url(ex, "messages")?, self.method), None)?;
                ex.set_body(r);
            }
            ("messages", "modify") => {
                let mut m = ObjectMap::new();
                for k in ["addLabelIds", "removeLabelIds"] {
                    if let Some(v) = self.opt_value(ex, k) {
                        m.insert(k.into(), Value::from(str_list(&v)));
                    }
                }
                let r = self.call("POST", &format!("{}/modify", id_url(ex, "messages")?), Some(&Value::Object(m)))?;
                ex.set_body(r);
            }
            ("labels", "list") => {
                let r = self.call("GET", &format!("{}/labels", base), None)?;
                ex.set_body(r.get("labels").cloned().unwrap_or(Value::Array(vec![])));
            }
            ("threads", "get") => {
                let r = self.call("GET", &id_url(ex, "threads")?, None)?;
                ex.set_body(r);
            }
            _ => unreachable!("validated at endpoint creation"),
        }
        Ok(())
    }

    fn calendar(&self, ex: &mut Exchange) -> Result<()> {
        let base = format!("{}/calendar/v3", self.gcp.base("https://www.googleapis.com"));
        let cal = self.opt(ex, "calendarId").unwrap_or_else(|| "primary".into());
        let events = format!("{}/calendars/{}/events", base, uri_encode(&cal, false));
        let event_url = |ex: &Exchange| -> Result<String> { Ok(format!("{}/{}", events, uri_encode(&self.need(ex, "eventId")?, false))) };
        let send_updates = self.opt(ex, "sendUpdates").map(|s| format!("?sendUpdates={}", uri_encode(&s, false))).unwrap_or_default();
        match (self.api_name.as_str(), self.method.as_str()) {
            ("events", "list") => {
                let q = self.query(ex, &[("timeMin", "timeMin"), ("timeMax", "timeMax"), ("q", "q"), ("maxResults", "maxResults"), ("singleEvents", "singleEvents"), ("orderBy", "orderBy")]);
                let single = self.opt(ex, "maxResults").is_some();
                ex.set_body(Value::Array(self.paged(&events, q, "items", single)?));
            }
            ("events", "insert") => {
                let c = self.content(ex)?;
                ex.set_body(self.call("POST", &format!("{}{}", events, send_updates), Some(&c))?);
            }
            ("events", "get") => {
                let u = event_url(ex)?;
                ex.set_body(self.call("GET", &u, None)?);
            }
            ("events", "update") | ("events", "patch") => {
                let c = self.content(ex)?;
                let u = format!("{}{}", event_url(ex)?, send_updates);
                ex.set_body(self.call(if self.method == "update" { "PUT" } else { "PATCH" }, &u, Some(&c))?);
            }
            ("events", "delete") => {
                self.send(Request::new("DELETE", Url::parse(&event_url(ex)?)?))?;
            }
            ("events", "quickAdd") => {
                let text = self.opt(ex, "text").unwrap_or_else(|| ex.body().to_string());
                ex.set_body(self.call("POST", &format!("{}/quickAdd?text={}", events, uri_encode(&text, false)), None)?);
            }
            ("calendarList", "list") => ex.set_body(Value::Array(self.paged(&format!("{}/users/me/calendarList", base), vec![], "items", false)?)),
            ("calendars", "get") => ex.set_body(self.call("GET", &format!("{}/calendars/{}", base, uri_encode(&cal, false)), None)?),
            _ => unreachable!("validated at endpoint creation"),
        }
        Ok(())
    }

    fn drive(&self, ex: &mut Exchange) -> Result<()> {
        let root = self.gcp.base("https://www.googleapis.com");
        let files = format!("{}/drive/v3/files", root);
        let file_url = |ex: &Exchange| -> Result<String> { Ok(format!("{}/{}", files, uri_encode(&self.need(ex, "fileId")?, false))) };
        match self.method.as_str() {
            "list" => {
                let mut q = self.query(ex, &[("q", "q"), ("pageSize", "pageSize"), ("orderBy", "orderBy")]);
                q.push(("fields".into(), self.opt(ex, "fields").unwrap_or_else(|| "nextPageToken,files(id,name,mimeType,size,parents,modifiedTime)".into())));
                ex.set_body(Value::Array(self.paged(&files, q, "files", false)?));
            }
            "get" => {
                let u = file_url(ex)?;
                ex.set_body(self.call("GET", &u, None)?);
            }
            "download" => {
                let r = self.send(Request::new("GET", Url::parse(&format!("{}?alt=media", file_url(ex)?))?))?;
                ex.set_body(Value::Bytes(r.body));
            }
            "insert" | "create" => {
                let mut meta = ObjectMap::new();
                let name = self.opt(ex, "name").or_else(|| self.opt(ex, "title")).or_else(|| crate::util::header_str(ex, "CamelFileName")).ok_or_else(|| CamelError::illegal_argument("drive-files/create needs the name option"))?;
                meta.insert("name".into(), Value::from(name));
                let mime = self.opt(ex, "mimeType").unwrap_or_else(|| "application/octet-stream".into());
                meta.insert("mimeType".into(), Value::from(mime.clone()));
                if let Some(p) = self.opt_value(ex, "parents") {
                    meta.insert("parents".into(), Value::from(str_list(&p)));
                }
                let boundary = format!("camel-{}", random_hex(12));
                let mut body = format!("--{}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{}\r\nContent-Type: {}\r\n\r\n", boundary, Value::Object(meta).to_json(), boundary, mime).into_bytes();
                body.extend_from_slice(&ex.body().to_bytes());
                body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
                let req = Request::new("POST", Url::parse(&format!("{}/upload/drive/v3/files?uploadType=multipart", root))?).header("Content-Type", format!("multipart/related; boundary={}", boundary)).body(body);
                ex.set_body(parse_json(&self.send(req)?.body)?);
            }
            "update" => {
                let id = self.need(ex, "fileId")?;
                let req = Request::new("PATCH", Url::parse(&format!("{}/upload/drive/v3/files/{}?uploadType=media", root, uri_encode(&id, false)))?)
                    .header("Content-Type", self.opt(ex, "mimeType").unwrap_or_else(|| "application/octet-stream".into()))
                    .body(ex.body().to_bytes());
                ex.set_body(parse_json(&self.send(req)?.body)?);
            }
            "copy" => {
                let mut m = ObjectMap::new();
                if let Some(n) = self.opt(ex, "name") {
                    m.insert("name".into(), Value::from(n));
                }
                let u = format!("{}/copy", file_url(ex)?);
                ex.set_body(self.call("POST", &u, Some(&Value::Object(m)))?);
            }
            "delete" => {
                self.send(Request::new("DELETE", Url::parse(&file_url(ex)?)?))?;
            }
            _ => unreachable!("validated at endpoint creation"),
        }
        Ok(())
    }
}

impl CloudService for WorkspaceService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        match self.api {
            Api::Mail => self.mail(ex),
            Api::Calendar => self.calendar(ex),
            Api::Drive => self.drive(ex),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_building() {
        let m = Value::object([("to", Value::from(vec!["a@x.org", "b@x.org"])), ("subject", Value::from("Hi")), ("text", Value::from("plain")), ("html", Value::from("<b>x</b>"))]);
        let mime = String::from_utf8(build_mime(&m)).unwrap();
        assert!(mime.contains("To: a@x.org, b@x.org\r\n"));
        assert!(mime.contains("Subject: Hi\r\n"));
        assert!(mime.contains("multipart/alternative"));
        let msg = Value::object([("payload", Value::object([("mimeType", Value::from("multipart/alternative")), ("parts", Value::Array(vec![Value::object([("mimeType", Value::from("text/plain")), ("body", Value::object([("data", base64url_encode(b"hello"))]))])]))]))]);
        assert_eq!(message_text(&msg).as_deref(), Some("hello"));
    }
}
