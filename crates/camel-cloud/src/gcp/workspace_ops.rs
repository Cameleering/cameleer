//! The remaining Camel API names of `google-mail`, `google-calendar` and `google-drive`,
//! including push notification channels (watch / stop), as a table of REST calls. Path
//! parameters (`{name}`) come from the option or the `Camel<Component>.<name>` header
//! (`userId` defaults to `me`, `calendarId` to `primary`); request bodies from the `content`
//! header or a map body; list methods return the list items (all pages).
//!
//! * Gmail: `drafts/create` (a message map / text body becomes a MIME draft, or a `{message}`
//!   map), `drafts/list`, `drafts/get`, `drafts/update`, `drafts/send` (`id`), `drafts/delete`;
//!   `history/list` (`startHistoryId`, `historyTypes`); `attachments/get` (`messageId`, `id`;
//!   body = the decoded bytes); `users/getProfile`; `users/watch` (`topicName` = a Pub/Sub topic,
//!   `labelIds`; Gmail publishes `{emailAddress, historyId}` notifications to it) and
//!   `users/stop`; `labels/create`, `labels/get`, `labels/update`, `labels/delete`;
//!   `threads/list`, `threads/modify`, `threads/trash`, `threads/untrash`, `threads/delete`.
//! * Calendar: `acl/list`, `acl/get`, `acl/insert`, `acl/update`, `acl/delete` (`ruleId`);
//!   `colors/get`; `freebusy/query` (content `{timeMin, timeMax, items:[{id}]}`);
//!   `settings/list`, `settings/get` (`setting`); `calendars/insert`, `calendars/update`,
//!   `calendars/delete`, `calendars/clear`; `calendarList/get`, `calendarList/insert`,
//!   `calendarList/delete`; `events/instances`, `events/move` (`destination`), `events/import`;
//!   watch channels `events/watch`, `acl/watch`, `calendarList/watch`, `settings/watch`
//!   (content `{id, type: web_hook, address, token}` or the `channelId` / `address` / `token`
//!   options; body = the channel with its `resourceId`) and `channels/stop` (`channelId`,
//!   `resourceId`). Notifications arrive as HTTP POSTs with `X-Goog-Channel-ID`,
//!   `X-Goog-Resource-ID`, `X-Goog-Resource-State` (`sync`, `exists`, ...),
//!   `X-Goog-Message-Number`, `X-Goog-Channel-Token`.
//! * Drive: `drive-about/get` (`fields`, default `*`); `drive-changes/getStartPageToken`,
//!   `drive-changes/list` (`pageToken`; body = changes), `drive-changes/watch`;
//!   `drive-files/watch` (`fileId`), `drive-files/emptyTrash`, `drive-files/generateIds`;
//!   `drive-channels/stop`; `drive-permissions/create|list|get|update|delete` (`fileId`,
//!   `permissionId`); `drive-comments/create|list|get|update|delete` (`commentId`);
//!   `drive-replies/create|list|get|delete` (`replyId`); `drive-revisions/list|get|delete`
//!   (`revisionId`); `drive-drives/create|list|get|update|delete` (`driveId`; create sends a
//!   generated `requestId`); `drive-teamdrives/list`.

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

/// How the request body is built.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyKind {
    NoBody,
    Content,
    /// An empty JSON object.
    Empty,
    /// Gmail draft: a message map / text → `{message: {raw}}`.
    Draft,
    /// Watch channel request.
    Channel,
}

/// One extra API method: (api/method, HTTP method, path template, body, list field, query options).
pub(crate) type Entry = (&'static str, &'static str, &'static str, BodyKind, Option<&'static str>, &'static [&'static str]);

use BodyKind::*;

pub(crate) const MAIL: &[Entry] = &[
    ("drafts/create", "POST", "drafts", Draft, None, &[]),
    ("drafts/list", "GET", "drafts", NoBody, Some("drafts"), &["q", "maxResults"]),
    ("drafts/get", "GET", "drafts/{id}", NoBody, None, &["format"]),
    ("drafts/update", "PUT", "drafts/{id}", Draft, None, &[]),
    ("drafts/send", "POST", "drafts/send", Content, None, &[]),
    ("drafts/delete", "DELETE", "drafts/{id}", NoBody, None, &[]),
    ("history/list", "GET", "history", NoBody, Some("history"), &["startHistoryId", "historyTypes", "labelId", "maxResults"]),
    ("attachments/get", "GET", "messages/{messageId}/attachments/{id}", NoBody, None, &[]),
    ("users/getProfile", "GET", "profile", NoBody, None, &[]),
    ("users/watch", "POST", "watch", Content, None, &[]),
    ("users/stop", "POST", "stop", Empty, None, &[]),
    ("labels/create", "POST", "labels", Content, None, &[]),
    ("labels/get", "GET", "labels/{id}", NoBody, None, &[]),
    ("labels/update", "PUT", "labels/{id}", Content, None, &[]),
    ("labels/delete", "DELETE", "labels/{id}", NoBody, None, &[]),
    ("threads/list", "GET", "threads", NoBody, Some("threads"), &["q", "labelIds", "maxResults"]),
    ("threads/modify", "POST", "threads/{id}/modify", Content, None, &[]),
    ("threads/trash", "POST", "threads/{id}/trash", Empty, None, &[]),
    ("threads/untrash", "POST", "threads/{id}/untrash", Empty, None, &[]),
    ("threads/delete", "DELETE", "threads/{id}", NoBody, None, &[]),
];

pub(crate) const CALENDAR: &[Entry] = &[
    ("acl/list", "GET", "calendars/{calendarId}/acl", NoBody, Some("items"), &[]),
    ("acl/get", "GET", "calendars/{calendarId}/acl/{ruleId}", NoBody, None, &[]),
    ("acl/insert", "POST", "calendars/{calendarId}/acl", Content, None, &["sendNotifications"]),
    ("acl/update", "PUT", "calendars/{calendarId}/acl/{ruleId}", Content, None, &[]),
    ("acl/delete", "DELETE", "calendars/{calendarId}/acl/{ruleId}", NoBody, None, &[]),
    ("acl/watch", "POST", "calendars/{calendarId}/acl/watch", Channel, None, &[]),
    ("colors/get", "GET", "colors", NoBody, None, &[]),
    ("freebusy/query", "POST", "freeBusy", Content, None, &[]),
    ("settings/list", "GET", "users/me/settings", NoBody, Some("items"), &[]),
    ("settings/get", "GET", "users/me/settings/{setting}", NoBody, None, &[]),
    ("settings/watch", "POST", "users/me/settings/watch", Channel, None, &[]),
    ("calendars/insert", "POST", "calendars", Content, None, &[]),
    ("calendars/update", "PUT", "calendars/{calendarId}", Content, None, &[]),
    ("calendars/delete", "DELETE", "calendars/{calendarId}", NoBody, None, &[]),
    ("calendars/clear", "POST", "calendars/{calendarId}/clear", Empty, None, &[]),
    ("calendarList/get", "GET", "users/me/calendarList/{calendarId}", NoBody, None, &[]),
    ("calendarList/insert", "POST", "users/me/calendarList", Content, None, &[]),
    ("calendarList/delete", "DELETE", "users/me/calendarList/{calendarId}", NoBody, None, &[]),
    ("calendarList/watch", "POST", "users/me/calendarList/watch", Channel, None, &[]),
    ("events/instances", "GET", "calendars/{calendarId}/events/{eventId}/instances", NoBody, Some("items"), &["timeMin", "timeMax"]),
    ("events/move", "POST", "calendars/{calendarId}/events/{eventId}/move", Empty, None, &["destination"]),
    ("events/import", "POST", "calendars/{calendarId}/events/import", Content, None, &[]),
    ("events/watch", "POST", "calendars/{calendarId}/events/watch", Channel, None, &[]),
    ("channels/stop", "POST", "channels/stop", Channel, None, &[]),
];

pub(crate) const DRIVE: &[Entry] = &[
    ("drive-about/get", "GET", "about", NoBody, None, &["fields"]),
    ("drive-changes/getStartPageToken", "GET", "changes/startPageToken", NoBody, None, &[]),
    ("drive-changes/list", "GET", "changes", NoBody, Some("changes"), &["pageToken", "pageSize"]),
    ("drive-changes/watch", "POST", "changes/watch", Channel, None, &["pageToken"]),
    ("drive-files/watch", "POST", "files/{fileId}/watch", Channel, None, &[]),
    ("drive-files/emptyTrash", "DELETE", "files/trash", NoBody, None, &[]),
    ("drive-files/generateIds", "GET", "files/generateIds", NoBody, None, &["count"]),
    ("drive-channels/stop", "POST", "channels/stop", Channel, None, &[]),
    ("drive-permissions/create", "POST", "files/{fileId}/permissions", Content, None, &[]),
    ("drive-permissions/list", "GET", "files/{fileId}/permissions", NoBody, Some("permissions"), &[]),
    ("drive-permissions/get", "GET", "files/{fileId}/permissions/{permissionId}", NoBody, None, &[]),
    ("drive-permissions/update", "PATCH", "files/{fileId}/permissions/{permissionId}", Content, None, &[]),
    ("drive-permissions/delete", "DELETE", "files/{fileId}/permissions/{permissionId}", NoBody, None, &[]),
    ("drive-comments/create", "POST", "files/{fileId}/comments", Content, None, &["fields"]),
    ("drive-comments/list", "GET", "files/{fileId}/comments", NoBody, Some("comments"), &["fields"]),
    ("drive-comments/get", "GET", "files/{fileId}/comments/{commentId}", NoBody, None, &["fields"]),
    ("drive-comments/update", "PATCH", "files/{fileId}/comments/{commentId}", Content, None, &["fields"]),
    ("drive-comments/delete", "DELETE", "files/{fileId}/comments/{commentId}", NoBody, None, &[]),
    ("drive-replies/create", "POST", "files/{fileId}/comments/{commentId}/replies", Content, None, &["fields"]),
    ("drive-replies/list", "GET", "files/{fileId}/comments/{commentId}/replies", NoBody, Some("replies"), &["fields"]),
    ("drive-replies/get", "GET", "files/{fileId}/comments/{commentId}/replies/{replyId}", NoBody, None, &["fields"]),
    ("drive-replies/delete", "DELETE", "files/{fileId}/comments/{commentId}/replies/{replyId}", NoBody, None, &[]),
    ("drive-revisions/list", "GET", "files/{fileId}/revisions", NoBody, Some("revisions"), &[]),
    ("drive-revisions/get", "GET", "files/{fileId}/revisions/{revisionId}", NoBody, None, &[]),
    ("drive-revisions/delete", "DELETE", "files/{fileId}/revisions/{revisionId}", NoBody, None, &[]),
    ("drive-drives/create", "POST", "drives", Content, None, &["requestId"]),
    ("drive-drives/list", "GET", "drives", NoBody, Some("drives"), &["q"]),
    ("drive-drives/get", "GET", "drives/{driveId}", NoBody, None, &[]),
    ("drive-drives/update", "PATCH", "drives/{driveId}", Content, None, &[]),
    ("drive-drives/delete", "DELETE", "drives/{driveId}", NoBody, None, &[]),
    ("drive-teamdrives/list", "GET", "drives", NoBody, Some("drives"), &[]),
];

/// All option names used by the table (so the endpoint keeps them).
pub(crate) const EXTRA_OPTIONS: &[&str] = &[
    "messageId", "startHistoryId", "historyTypes", "labelId", "topicName", "ruleId", "setting", "destination", "channelId", "address", "token", "resourceId", "pageToken", "count",
    "permissionId", "commentId", "replyId", "revisionId", "driveId", "requestId", "sendNotifications", "syncToken",
];

/// Find the entry of an api/method.
pub(crate) fn entry(table: &'static [Entry], path: &str) -> Option<&'static Entry> {
    table.iter().find(|e| e.0 == path)
}

/// Fill `{name}` placeholders with values from `get` (defaults for userId / calendarId).
pub(crate) fn fill(template: &str, get: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').ok_or_else(|| CamelError::illegal_argument("bad template"))? + i;
        let name = &rest[i + 1..j];
        let v = get(name).or_else(|| match name {
            "calendarId" => Some("primary".into()),
            "userId" => Some("me".into()),
            _ => None,
        });
        let v = v.ok_or_else(|| CamelError::illegal_argument(format!("This method requires the {} option (or header)", name)))?;
        out.push_str(&crate::util::uri_encode(&v, false));
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A watch channel request from the content or the channel options.
pub(crate) fn channel_body(content: Option<Value>, get: &dyn Fn(&str) -> Option<String>) -> Result<Value> {
    if let Some(c) = content {
        return Ok(c);
    }
    let mut m = camel_core::value::ObjectMap::new();
    m.insert("id".into(), Value::from(get("channelId").unwrap_or_else(crate::util::uuid)));
    if let Some(r) = get("resourceId") {
        m.insert("resourceId".into(), Value::from(r));
    }
    if let Some(a) = get("address") {
        m.insert("type".into(), Value::from("web_hook"));
        m.insert("address".into(), Value::from(a));
    }
    if let Some(t) = get("token") {
        m.insert("token".into(), Value::from(t));
    }
    Ok(Value::Object(m))
}

/// The draft request of a message map / text / `{message}` map.
pub(crate) fn draft_body(ex: &Exchange, content: Option<Value>, id: Option<String>) -> Value {
    let message = match content {
        Some(Value::Object(m)) if m.contains_key("message") => return Value::Object(m),
        Some(v @ Value::Object(_)) => super::workspace::build_mime(&v),
        _ => match ex.body() {
            v @ Value::Object(_) => super::workspace::build_mime(v),
            other => other.to_bytes(),
        },
    };
    let mut m = camel_core::value::ObjectMap::new();
    if let Some(i) = id {
        m.insert("id".into(), Value::from(i));
    }
    m.insert("message".into(), crate::util::obj([("raw", Value::from(crate::util::base64url_encode(&message)))]));
    Value::Object(m)
}
