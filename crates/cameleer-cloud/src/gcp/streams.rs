//! Google Workspace streaming (polling) consumers, as Camel's `google-mail-stream`,
//! `google-calendar-stream` and `google-sheets-stream` components. They authenticate like the
//! other Google components: user credentials (`clientId` / `clientSecret` / `refreshToken`), a
//! fixed `accessToken`, or a service account with `delegate` (domain-wide delegation).
//!
//! **`google-mail-stream://index`**: every poll lists the messages matching `query` (default
//! `is:unread`) and `labels` (comma separated label ids), at most `maxResults` (default 10),
//! fetches each one and emits it oldest first: body = the plain-text part (the raw RFC 822 message
//! with `raw=true`), headers `CamelGoogleMailStreamId`, `...ThreadId`, `...Subject`, `...From`,
//! `...To`, `...Cc`, `...Bcc`, `...ReceivedDate`, `...Labels`. With `markAsRead=true` (default)
//! successfully processed messages lose the `UNREAD` label; messages already emitted are not
//! emitted again while they still match.
//!
//! **`google-calendar-stream://index`** (`calendarId` default `primary`, `query`, `maxResults`
//! default 10): emits events (body = the event resource) with `consumeFromNow=true` (default)
//! limiting to events starting after the consumer started and `considerLastUpdate=true` asking
//! only for events updated since the previous poll (`updatedMin`). An event is emitted again only
//! when its `updated` time changes (so the stream carries new and changed events); header
//! `CamelGoogleCalendarStreamEventId`. With `syncFlow=true` the consumer uses incremental sync
//! tokens instead: a full sync (all pages, `nextSyncToken` kept; its events are emitted only with
//! `consumeFromNow=false`), then every poll asks `events.list?syncToken=...` and emits the events
//! changed since, deleted ones included (`status` = `cancelled`, header
//! `CamelGoogleCalendarStreamEventStatus`); a `410 GONE` (expired token) restarts with a full sync
//! that only emits events not delivered before.
//!
//! **`google-sheets-stream://spreadsheetId`** (`range` required, `majorDimension` default `ROWS`,
//! `valueRenderOption`): every poll reads the range and emits the rows appended since the
//! previous poll (all rows on the first poll): with `splitResults=true` one exchange per row
//! (body = the row as a list, header `CamelGoogleSheets.valueIndex` (1-based row number in the
//! range)), otherwise one exchange whose body is a `ValueRange` map (`range`, `majorDimension`,
//! `values` = the new rows). Headers `CamelGoogleSheets.spreadsheetId`, `CamelGoogleSheets.range`,
//! `CamelGoogleSheets.majorDimension`. (Camel's consumer re-emits the whole range on every poll;
//! Cameleer emits the new rows only.)

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::{ObjectMap, Value};

use super::workspace::{message_text, CALENDAR_SCOPE, MAIL_SCOPE};
use super::GcpConfig;
use crate::endpoint::{process, set_batch, CloudService};
use crate::http::{Request, Url};
use crate::util::{encode_form, iso8601_millis, kinds, now_millis, obj, parse_json, str_list, uri_encode, Opts};

fn scopes(o: &mut Opts, default: &str) -> String {
    o.get("scopes").map(|s| str_list(&Value::from(s)).join(" ")).unwrap_or_else(|| default.to_string())
}

fn get_json(gcp: &GcpConfig, scope: &str, url: &str, query: &[(String, String)]) -> Result<Value> {
    let full = if query.is_empty() { url.to_string() } else { format!("{}?{}", url, encode_form(query)) };
    parse_json(&gcp.send_scoped(Request::new("GET", Url::parse(&full)?), kinds::GOOGLE_JSON, scope)?.body)
}

fn produce_unsupported(scheme: &str) -> CamelError {
    CamelError::new(cameleer_core::error::kinds::UNSUPPORTED_OPERATION, format!("{} only supports consumers", scheme))
}

// ---------------------------------------------------------------------------------- mail

/// Header names of `google-mail-stream` (Camel `GoogleMailStreamConstants`).
pub mod mail_headers {
    pub const ID: &str = "CamelGoogleMailStreamId";
    pub const THREAD_ID: &str = "CamelGoogleMailStreamThreadId";
    pub const SUBJECT: &str = "CamelGoogleMailStreamSubject";
    pub const FROM: &str = "CamelGoogleMailStreamFrom";
    pub const TO: &str = "CamelGoogleMailStreamTo";
    pub const CC: &str = "CamelGoogleMailStreamCc";
    pub const BCC: &str = "CamelGoogleMailStreamBcc";
    pub const RECEIVED_DATE: &str = "CamelGoogleMailStreamReceivedDate";
    pub const LABELS: &str = "CamelGoogleMailStreamLabels";
}

/// The `google-mail-stream` consumer.
pub struct MailStreamService {
    pub gcp: GcpConfig,
    pub scope: String,
    pub user: String,
    pub query: String,
    pub labels: Vec<String>,
    pub max_results: i64,
    pub mark_as_read: bool,
    pub raw: bool,
    pub scheduler: ScheduledPollConfig,
    seen: Mutex<HashSet<String>>,
}

/// Factory for `google-mail-stream`.
pub fn create_mail_stream(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    Ok(Arc::new(MailStreamService {
        scope: scopes(o, MAIL_SCOPE),
        user: o.string("userId", "me"),
        query: o.string("query", "is:unread"),
        labels: o.get("labels").map(|l| str_list(&Value::from(l))).unwrap_or_default(),
        max_results: o.i64("maxResults", 10)?.max(1),
        mark_as_read: o.bool("markAsRead", true)?,
        raw: o.bool("raw", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        seen: Mutex::new(HashSet::new()),
        gcp,
    }))
}

impl CloudService for MailStreamService {
    fn produce(&self, _ex: &mut Exchange) -> Result<()> {
        Err(produce_unsupported("google-mail-stream"))
    }
    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }
    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let base = format!("{}/gmail/v1/users/{}", self.gcp.base("https://gmail.googleapis.com"), uri_encode(&self.user, false));
        let mut q = vec![("q".to_string(), self.query.clone()), ("maxResults".to_string(), self.max_results.to_string())];
        q.extend(self.labels.iter().map(|l| ("labelIds".to_string(), l.clone())));
        let list = get_json(&self.gcp, &self.scope, &format!("{}/messages", base), &q)?;
        let mut ids: Vec<String> = list.get("messages").and_then(|m| m.as_array()).into_iter().flatten().filter_map(|m| m.get("id").map(|i| i.to_string())).collect();
        ids.reverse(); // the API lists newest first
        let current: HashSet<String> = ids.iter().cloned().collect();
        let fresh: Vec<String> = {
            let mut seen = self.seen.lock().unwrap();
            seen.retain(|i| current.contains(i));
            ids.into_iter().filter(|i| !seen.contains(i)).collect()
        };
        let total = fresh.len();
        for (i, id) in fresh.iter().enumerate() {
            let format = if self.raw { "raw" } else { "full" };
            let msg = get_json(&self.gcp, &self.scope, &format!("{}/messages/{}", base, uri_encode(id, false)), &[("format".into(), format.into())])?;
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(mail_headers::ID, id.clone());
            ex.set_header(mail_headers::THREAD_ID, msg.get("threadId").cloned().unwrap_or_default());
            ex.set_header(mail_headers::LABELS, msg.get("labelIds").cloned().unwrap_or(Value::Array(vec![])));
            if let Some(d) = msg.get("internalDate") {
                ex.set_header(mail_headers::RECEIVED_DATE, d.clone());
            }
            for h in msg.get("payload").and_then(|p| p.get("headers")).and_then(|h| h.as_array()).into_iter().flatten() {
                let name = h.get("name").map(|n| n.to_string()).unwrap_or_default().to_ascii_lowercase();
                let header = match name.as_str() {
                    "subject" => mail_headers::SUBJECT,
                    "from" => mail_headers::FROM,
                    "to" => mail_headers::TO,
                    "cc" => mail_headers::CC,
                    "bcc" => mail_headers::BCC,
                    _ => continue,
                };
                ex.set_header(header, h.get("value").cloned().unwrap_or_default());
            }
            if self.raw {
                ex.set_body(Value::Bytes(msg.get("raw").and_then(|r| crate::util::base64_decode(&r.to_string())).unwrap_or_default()));
            } else {
                ex.set_body(message_text(&msg).unwrap_or_default());
            }
            set_batch(&mut ex, i, total);
            let ok = process(logger, processor, &mut ex);
            self.seen.lock().unwrap().insert(id.clone());
            if ok && self.mark_as_read {
                let req = Request::new("POST", Url::parse(&format!("{}/messages/{}/modify", base, uri_encode(id, false)))?)
                    .header("Content-Type", "application/json")
                    .body(obj([("removeLabelIds", Value::from(vec!["UNREAD"]))]).to_json().into_bytes());
                if let Err(e) = self.gcp.send_scoped(req, kinds::GOOGLE_JSON, &self.scope) {
                    cameleer_core::camel_warn!(logger, "Cannot mark Gmail message {} as read: {}", id, e);
                }
            }
        }
        Ok(total)
    }
}

// ------------------------------------------------------------------------------ calendar

/// The `google-calendar-stream` consumer.
pub struct CalendarStreamService {
    pub gcp: GcpConfig,
    pub scope: String,
    pub calendar_id: String,
    pub query: Option<String>,
    pub max_results: i64,
    pub consume_from_now: bool,
    pub consider_last_update: bool,
    pub scheduler: ScheduledPollConfig,
    /// (consumer start, last poll time, event id -> updated)
    state: Mutex<(Option<i64>, Option<i64>, BTreeMap<String, String>)>,
    pub sync_flow: bool,
    sync_token: Mutex<Option<String>>,
}

/// Status header of `google-calendar-stream` with `syncFlow`.
pub const CALENDAR_EVENT_STATUS: &str = "CamelGoogleCalendarStreamEventStatus";

/// Header of `google-calendar-stream`.
pub const CALENDAR_EVENT_ID: &str = "CamelGoogleCalendarStreamEventId";

/// Factory for `google-calendar-stream`.
pub fn create_calendar_stream(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    let sync_flow = o.bool("syncFlow", false)?;
    Ok(Arc::new(CalendarStreamService {
        sync_flow,
        sync_token: Mutex::new(None),
        scope: scopes(o, CALENDAR_SCOPE),
        calendar_id: o.string("calendarId", "primary"),
        query: o.get("query"),
        max_results: o.i64("maxResults", 10)?.max(1),
        consume_from_now: o.bool("consumeFromNow", true)?,
        consider_last_update: o.bool("considerLastUpdate", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        state: Mutex::new((None, None, BTreeMap::new())),
        gcp,
    }))
}

impl CloudService for CalendarStreamService {
    fn produce(&self, _ex: &mut Exchange) -> Result<()> {
        Err(produce_unsupported("google-calendar-stream"))
    }
    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }
    fn on_start(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.0.is_none() {
            s.0 = Some(now_millis());
        }
        Ok(())
    }
    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        if self.sync_flow {
            return self.poll_sync(ctx, processor, logger);
        }
        let (start, last, _) = self.state.lock().unwrap().clone();
        let poll_time = now_millis();
        let url = format!("{}/calendar/v3/calendars/{}/events", self.gcp.base("https://www.googleapis.com"), uri_encode(&self.calendar_id, false));
        let mut q = vec![("maxResults".to_string(), self.max_results.to_string()), ("singleEvents".to_string(), "true".to_string()), ("orderBy".to_string(), "startTime".to_string())];
        if let Some(qq) = &self.query {
            q.push(("q".into(), qq.clone()));
        }
        if self.consume_from_now {
            q.push(("timeMin".into(), iso8601_millis(start.unwrap_or(poll_time))));
        }
        if self.consider_last_update {
            if let Some(l) = last {
                // a small overlap: ids + updated de-duplicate
                q.push(("updatedMin".into(), iso8601_millis(l - 1000)));
            }
        }
        let v = get_json(&self.gcp, &self.scope, &url, &q)?;
        let items = v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        let fresh: Vec<Value> = {
            let s = self.state.lock().unwrap();
            items
                .into_iter()
                .filter(|e| {
                    let id = e.get("id").map(|i| i.to_string()).unwrap_or_default();
                    let upd = e.get("updated").map(|u| u.to_string()).unwrap_or_default();
                    s.2.get(&id) != Some(&upd)
                })
                .collect()
        };
        let total = fresh.len();
        for (i, e) in fresh.into_iter().enumerate() {
            let id = e.get("id").map(|i| i.to_string()).unwrap_or_default();
            let upd = e.get("updated").map(|u| u.to_string()).unwrap_or_default();
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(CALENDAR_EVENT_ID, id.clone());
            ex.set_body(e);
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
            self.state.lock().unwrap().2.insert(id, upd);
        }
        self.state.lock().unwrap().1 = Some(poll_time);
        Ok(total)
    }
}

impl CalendarStreamService {
    /// One page loop of `events.list` (`syncToken` or a full listing); returns (events, next sync token).
    fn list_all(&self, sync: Option<&str>) -> Result<(Vec<Value>, Option<String>)> {
        let url = format!("{}/calendar/v3/calendars/{}/events", self.gcp.base("https://www.googleapis.com"), uri_encode(&self.calendar_id, false));
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut q = vec![("maxResults".to_string(), "250".to_string())];
            if let Some(t) = sync {
                q.push(("syncToken".into(), t.to_string()));
            }
            if let Some(p) = &page {
                q.push(("pageToken".into(), p.clone()));
            }
            let v = get_json(&self.gcp, &self.scope, &url, &q)?;
            out.extend(v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default());
            page = v.get("nextPageToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if page.is_none() {
                return Ok((out, v.get("nextSyncToken").map(|t| t.to_string())));
            }
        }
    }

    fn poll_sync(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let token = self.sync_token.lock().unwrap().clone();
        let (events, next, full) = match &token {
            Some(t) => match self.list_all(Some(t)) {
                Ok((e, n)) => (e, n, false),
                Err(e) if super::error_status(&e) == Some(410) => {
                    cameleer_core::camel_info!(logger, "Calendar sync token expired: full resynchronization");
                    let (e, n) = self.list_all(None)?;
                    (e, n, true)
                }
                Err(e) => return Err(e),
            },
            None => {
                let (e, n) = self.list_all(None)?;
                if self.consume_from_now {
                    // the initial full sync only records what exists
                    let mut s = self.state.lock().unwrap();
                    for ev in &e {
                        s.2.insert(ev.get("id").map(|i| i.to_string()).unwrap_or_default(), ev.get("updated").map(|u| u.to_string()).unwrap_or_default());
                    }
                    drop(s);
                    *self.sync_token.lock().unwrap() = n;
                    return Ok(0);
                }
                (e, n, true)
            }
        };
        let fresh: Vec<Value> = {
            let s = self.state.lock().unwrap();
            events
                .into_iter()
                .filter(|e| {
                    if !full {
                        return true;
                    }
                    let id = e.get("id").map(|i| i.to_string()).unwrap_or_default();
                    s.2.get(&id) != e.get("updated").map(|u| u.to_string()).as_ref()
                })
                .collect()
        };
        let total = fresh.len();
        for (i, e) in fresh.into_iter().enumerate() {
            let id = e.get("id").map(|i| i.to_string()).unwrap_or_default();
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(CALENDAR_EVENT_ID, id.clone());
            ex.set_header(CALENDAR_EVENT_STATUS, e.get("status").cloned().unwrap_or_else(|| Value::from("confirmed")));
            self.state.lock().unwrap().2.insert(id, e.get("updated").map(|u| u.to_string()).unwrap_or_default());
            ex.set_body(e);
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
        }
        if next.is_some() {
            *self.sync_token.lock().unwrap() = next;
        }
        Ok(total)
    }
}

// -------------------------------------------------------------------------------- sheets

/// The `google-sheets-stream` consumer.
pub struct SheetsStreamService {
    pub gcp: GcpConfig,
    pub scope: String,
    pub spreadsheet_id: String,
    pub range: String,
    pub major_dimension: String,
    pub value_render_option: Option<String>,
    pub split_results: bool,
    pub scheduler: ScheduledPollConfig,
    rows_seen: Mutex<usize>,
}

/// Factory for `google-sheets-stream`.
pub fn create_sheets_stream(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let gcp = GcpConfig::from_opts(o)?;
    let spreadsheet_id = o.get("spreadsheetId").unwrap_or_else(|| remaining.to_string());
    if spreadsheet_id.is_empty() || spreadsheet_id == "index" {
        return Err(CamelError::illegal_argument("google-sheets-stream needs the spreadsheet id (google-sheets-stream://spreadsheetId or spreadsheetId option)"));
    }
    let range = o.get("range").ok_or_else(|| CamelError::illegal_argument("google-sheets-stream needs the range option"))?;
    o.ignore(&["includeGridData", "maxResults"]);
    Ok(Arc::new(SheetsStreamService {
        scope: scopes(o, super::sheets::SCOPE),
        spreadsheet_id,
        range,
        major_dimension: o.string("majorDimension", "ROWS"),
        value_render_option: o.get("valueRenderOption"),
        split_results: o.bool("splitResults", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        rows_seen: Mutex::new(0),
        gcp,
    }))
}

impl CloudService for SheetsStreamService {
    fn produce(&self, _ex: &mut Exchange) -> Result<()> {
        Err(produce_unsupported("google-sheets-stream"))
    }
    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }
    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let url = format!("{}/v4/spreadsheets/{}/values/{}", self.gcp.base("https://sheets.googleapis.com"), uri_encode(&self.spreadsheet_id, false), uri_encode(&self.range, false));
        let mut q = vec![("majorDimension".to_string(), self.major_dimension.clone())];
        if let Some(v) = &self.value_render_option {
            q.push(("valueRenderOption".into(), v.clone()));
        }
        let v = get_json(&self.gcp, &self.scope, &url, &q)?;
        let rows = v.get("values").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let seen = *self.rows_seen.lock().unwrap();
        // rows deleted from the range: start over from the new end
        let start = if rows.len() < seen { rows.len() } else { seen };
        let new_rows: Vec<Value> = rows[start..].to_vec();
        *self.rows_seen.lock().unwrap() = rows.len();
        if new_rows.is_empty() {
            return Ok(0);
        }
        let range = v.get("range").cloned().unwrap_or_else(|| Value::from(self.range.clone()));
        let headers = |ex: &mut Exchange| {
            ex.set_header("CamelGoogleSheets.spreadsheetId", self.spreadsheet_id.clone());
            ex.set_header("CamelGoogleSheets.range", range.clone());
            ex.set_header("CamelGoogleSheets.majorDimension", self.major_dimension.clone());
        };
        if self.split_results {
            let total = new_rows.len();
            for (i, r) in new_rows.into_iter().enumerate() {
                let mut ex = Exchange::new(ctx.clone());
                headers(&mut ex);
                ex.set_header("CamelGoogleSheets.valueIndex", (start + i + 1) as i64);
                ex.set_body(r);
                set_batch(&mut ex, i, total);
                process(logger, processor, &mut ex);
            }
            Ok(total)
        } else {
            let mut ex = Exchange::new(ctx);
            headers(&mut ex);
            let mut m = ObjectMap::new();
            m.insert("range".into(), range.clone());
            m.insert("majorDimension".into(), Value::from(self.major_dimension.clone()));
            m.insert("values".into(), Value::Array(new_rows));
            ex.set_body(Value::Object(m));
            process(logger, processor, &mut ex);
            Ok(1)
        }
    }
}
