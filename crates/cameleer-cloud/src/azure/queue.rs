//! The `azure-storage-queue` component (Camel `camel-azure-storage-queue`) over the Queue REST API.
//!
//! `azure-storage-queue://accountName/queueName?operation=...&accessKey=...`
//!
//! Producer operations (`operation` / `CamelAzureStorageQueueOperation`; default
//! `sendMessage`): `listQueues`, `createQueue`, `deleteQueue`, `clearQueue`, `sendMessage`
//! (`visibilityTimeout` / `CamelAzureStorageQueueVisibilityTimeout`, `timeToLive` /
//! `CamelAzureStorageQueueTimeToLive`; `createQueue=true` creates the queue first),
//! `receiveMessages` / `peekMessages` (`maxMessages`, body = list of messages), `deleteMessage`
//! (`CamelAzureStorageQueueMessageId` + `CamelAzureStorageQueuePopReceipt`), `updateMessage`
//! (same headers, body = new text). Message text is sent as is (no base64), like the Azure SDK v12.
//!
//! Consumer: receives up to `maxMessages` (default 1) messages per poll with
//! `visibilityTimeout`, one exchange each (body = text, headers `CamelAzureStorageQueueMessageId`,
//! `CamelAzureStorageQueuePopReceipt`, `CamelAzureStorageQueueInsertionTime`,
//! `CamelAzureStorageQueueExpirationTime`, `CamelAzureStorageQueueTimeNextVisible`,
//! `CamelAzureStorageQueueDequeueCount`); successfully processed messages are deleted.

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::Value;

use super::{duration_secs, error_code, AzureConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{header_str, kinds, obj, uri_encode, xml_escape, Opts, Xml};

/// Header names (Camel `QueueConstants`).
pub mod headers {
    pub const MESSAGE_ID: &str = "CamelAzureStorageQueueMessageId";
    pub const POP_RECEIPT: &str = "CamelAzureStorageQueuePopReceipt";
    pub const INSERTION_TIME: &str = "CamelAzureStorageQueueInsertionTime";
    pub const EXPIRATION_TIME: &str = "CamelAzureStorageQueueExpirationTime";
    pub const TIME_NEXT_VISIBLE: &str = "CamelAzureStorageQueueTimeNextVisible";
    pub const DEQUEUE_COUNT: &str = "CamelAzureStorageQueueDequeueCount";
    pub const VISIBILITY_TIMEOUT: &str = "CamelAzureStorageQueueVisibilityTimeout";
    pub const TIME_TO_LIVE: &str = "CamelAzureStorageQueueTimeToLive";
    pub const MAX_MESSAGES: &str = "CamelAzureStorageQueueMaxMessages";
    pub const OPERATION: &str = "CamelAzureStorageQueueOperation";
    pub const QUEUE_NAME: &str = "CamelAzureStorageQueueName";
}

/// A message returned by Get/Peek/Put Messages.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueMessage {
    pub message_id: String,
    pub pop_receipt: String,
    pub text: String,
    pub dequeue_count: i64,
    pub insertion_time: String,
    pub expiration_time: String,
    pub time_next_visible: String,
}

impl QueueMessage {
    pub fn to_value(&self) -> Value {
        obj([
            ("messageId", Value::from(self.message_id.to_string())),
            ("popReceipt", Value::from(self.pop_receipt.clone())),
            ("messageText", Value::from(self.text.clone())),
            ("dequeueCount", Value::Int(self.dequeue_count)),
            ("insertionTime", Value::from(self.insertion_time.clone())),
            ("expirationTime", Value::from(self.expiration_time.clone())),
            ("timeNextVisible", Value::from(self.time_next_visible.clone())),
        ])
    }

    fn set_headers(&self, ex: &mut Exchange) {
        ex.set_header(headers::MESSAGE_ID, self.message_id.to_string());
        ex.set_header(headers::POP_RECEIPT, self.pop_receipt.clone());
        ex.set_header(headers::INSERTION_TIME, self.insertion_time.clone());
        ex.set_header(headers::EXPIRATION_TIME, self.expiration_time.clone());
        if !self.time_next_visible.is_empty() {
            ex.set_header(headers::TIME_NEXT_VISIBLE, self.time_next_visible.clone());
        }
        ex.set_header(headers::DEQUEUE_COUNT, self.dequeue_count);
    }
}

/// Parse a `QueueMessagesList` document.
pub fn parse_messages(xml: &str) -> Result<Vec<QueueMessage>> {
    if xml.trim().is_empty() {
        return Ok(Vec::new());
    }
    let x = Xml::parse(xml)?;
    Ok(x.descendants("QueueMessage")
        .into_iter()
        .map(|m| {
            let t = |n: &str| x.child_text(m, n).unwrap_or_default();
            QueueMessage {
                message_id: t("MessageId"),
                pop_receipt: t("PopReceipt"),
                text: t("MessageText"),
                dequeue_count: t("DequeueCount").parse().unwrap_or(0),
                insertion_time: t("InsertionTime"),
                expiration_time: t("ExpirationTime"),
                time_next_visible: t("TimeNextVisible"),
            }
        })
        .collect())
}

/// The queue endpoint configuration and client.
pub struct QueueService {
    pub azure: AzureConfig,
    pub queue: Option<String>,
    pub operation: Option<String>,
    pub max_messages: i64,
    pub visibility_timeout: Option<i64>,
    pub time_to_live: Option<i64>,
    pub create_queue: bool,
    pub scheduler: ScheduledPollConfig,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (account, queue) = match remaining.split_once('/') {
        Some((a, q)) => (a, Some(q.trim_matches('/').to_string()).filter(|q| !q.is_empty())),
        None => (remaining, None),
    };
    let azure = AzureConfig::from_opts(account, o)?;
    let s = QueueService {
        queue,
        operation: o.get("operation"),
        max_messages: o.i64("maxMessages", 1)?,
        visibility_timeout: o.get("visibilityTimeout").map(|v| duration_secs(&v)).transpose()?,
        time_to_live: o.get("timeToLive").map(|v| duration_secs(&v)).transpose()?,
        create_queue: o.bool("createQueue", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        azure,
    };
    o.ignore(&["popReceipt", "messageId", "timeout"]);
    Ok(Arc::new(s))
}

impl QueueService {
    fn send(&self, req: Request) -> Result<Response> {
        self.azure.send(req, kinds::QUEUE_STORAGE)
    }

    fn queue(&self, ex: Option<&Exchange>) -> Result<String> {
        ex.and_then(|e| header_str(e, headers::QUEUE_NAME))
            .or_else(|| self.queue.clone())
            .ok_or_else(|| CamelError::illegal_argument("No queue name: set it in the uri (azure-storage-queue://account/queue) or the CamelAzureStorageQueueName header"))
    }

    pub fn create_queue(&self, queue: &str) -> Result<()> {
        match self.send(Request::new("PUT", self.azure.url("queue", &uri_encode(queue, false), "")?)) {
            Ok(_) => Ok(()),
            Err(e) if error_code(&e).as_deref() == Some("QueueAlreadyExists") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Put a message; returns the created message (id, pop receipt, times).
    pub fn send_message(&self, queue: &str, text: &str, visibility: Option<i64>, ttl: Option<i64>) -> Result<QueueMessage> {
        let mut q = Vec::new();
        if let Some(v) = visibility {
            q.push(format!("visibilitytimeout={}", v));
        }
        if let Some(t) = ttl {
            q.push(format!("messagettl={}", t));
        }
        let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><QueueMessage><MessageText>{}</MessageText></QueueMessage>", xml_escape(text));
        let req = Request::new("POST", self.azure.url("queue", &format!("{}/messages", uri_encode(queue, false)), &q.join("&"))?)
            .header("Content-Type", "application/xml")
            .body(body.into_bytes());
        let resp = self.send(req)?;
        Ok(parse_messages(&resp.text())?.into_iter().next().unwrap_or_default())
    }

    /// Get (or peek) messages.
    pub fn receive_messages(&self, queue: &str, max: i64, visibility: Option<i64>, peek: bool) -> Result<Vec<QueueMessage>> {
        let mut q = vec![format!("numofmessages={}", max.clamp(1, 32))];
        if peek {
            q.push("peekonly=true".into());
        } else if let Some(v) = visibility {
            q.push(format!("visibilitytimeout={}", v));
        }
        let resp = self.send(Request::new("GET", self.azure.url("queue", &format!("{}/messages", uri_encode(queue, false)), &q.join("&"))?))?;
        parse_messages(&resp.text())
    }

    pub fn delete_message(&self, queue: &str, id: &str, pop_receipt: &str) -> Result<()> {
        let path = format!("{}/messages/{}", uri_encode(queue, false), uri_encode(id, false));
        self.send(Request::new("DELETE", self.azure.url("queue", &path, &format!("popreceipt={}", uri_encode(pop_receipt, false)))?))?;
        Ok(())
    }

    fn id_and_receipt(ex: &Exchange) -> Result<(String, String)> {
        let id = header_str(ex, headers::MESSAGE_ID).ok_or_else(|| CamelError::illegal_argument("The CamelAzureStorageQueueMessageId header is required"))?;
        let pr = header_str(ex, headers::POP_RECEIPT).ok_or_else(|| CamelError::illegal_argument("The CamelAzureStorageQueuePopReceipt header is required"))?;
        Ok((id, pr))
    }
}

impl CloudService for QueueService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "sendMessage");
        let secs = |ex: &Exchange, h: &str| ex.header(h).filter(|v| !v.is_null()).map(|v| v.as_i64().map(Ok).unwrap_or_else(|| duration_secs(&v.to_string()))).transpose();
        match op.as_str() {
            "listQueues" => {
                let resp = self.send(Request::new("GET", self.azure.url("queue", "", "comp=list")?))?;
                let x = Xml::parse(&resp.text())?;
                let names: Vec<Value> = x.descendants("Queue").into_iter().map(|q| obj([("name", Value::from(x.child_text(q, "Name").unwrap_or_default()))])).collect();
                ex.set_body(Value::Array(names));
            }
            "createQueue" => self.create_queue(&self.queue(Some(ex))?)?,
            "deleteQueue" => {
                self.send(Request::new("DELETE", self.azure.url("queue", &uri_encode(&self.queue(Some(ex))?, false), "")?))?;
            }
            "clearQueue" => {
                self.send(Request::new("DELETE", self.azure.url("queue", &format!("{}/messages", uri_encode(&self.queue(Some(ex))?, false)), "")?))?;
            }
            "sendMessage" => {
                let queue = self.queue(Some(ex))?;
                if self.create_queue {
                    self.create_queue(&queue)?;
                }
                let vis = secs(ex, headers::VISIBILITY_TIMEOUT)?.or(self.visibility_timeout);
                let ttl = secs(ex, headers::TIME_TO_LIVE)?.or(self.time_to_live);
                let m = self.send_message(&queue, &ex.body_as::<String>()?, vis, ttl)?;
                m.set_headers(ex);
            }
            "receiveMessages" | "peekMessages" => {
                let max = ex.header(headers::MAX_MESSAGES).and_then(|v| v.as_i64()).unwrap_or(self.max_messages);
                let vis = secs(ex, headers::VISIBILITY_TIMEOUT)?.or(self.visibility_timeout);
                let msgs = self.receive_messages(&self.queue(Some(ex))?, max, vis, op == "peekMessages")?;
                ex.set_body(Value::Array(msgs.iter().map(|m| m.to_value()).collect()));
            }
            "deleteMessage" => {
                let (id, pr) = Self::id_and_receipt(ex)?;
                self.delete_message(&self.queue(Some(ex))?, &id, &pr)?;
            }
            "updateMessage" => {
                let (id, pr) = Self::id_and_receipt(ex)?;
                let vis = secs(ex, headers::VISIBILITY_TIMEOUT)?.or(self.visibility_timeout).unwrap_or(0);
                let path = format!("{}/messages/{}", uri_encode(&self.queue(Some(ex))?, false), uri_encode(&id, false));
                let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><QueueMessage><MessageText>{}</MessageText></QueueMessage>", xml_escape(&ex.body_as::<String>()?));
                let req = Request::new("PUT", self.azure.url("queue", &path, &format!("popreceipt={}&visibilitytimeout={}", uri_encode(&pr, false), vis))?)
                    .header("Content-Type", "application/xml")
                    .body(body.into_bytes());
                let resp = self.send(req)?;
                if let Some(p) = resp.header("x-ms-popreceipt") {
                    ex.set_header(headers::POP_RECEIPT, p.to_string());
                }
            }
            other => return Err(unsupported("azure-storage-queue", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if self.create_queue {
            if let Some(q) = &self.queue {
                self.create_queue(q)?;
            }
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let queue = self.queue(None)?;
        let msgs = self.receive_messages(&queue, self.max_messages, self.visibility_timeout, false)?;
        let total = msgs.len();
        for (i, m) in msgs.iter().enumerate() {
            let mut ex = Exchange::new(ctx.clone());
            m.set_headers(&mut ex);
            ex.set_header(headers::QUEUE_NAME, queue.clone());
            ex.set_body(m.text.clone());
            set_batch(&mut ex, i, total);
            if process(logger, processor, &mut ex) {
                if let Err(e) = self.delete_message(&queue, &m.message_id, &m.pop_receipt) {
                    cameleer_core::camel_warn!(logger, "Error deleting queue message {}: {}", m.message_id, e);
                }
            }
        }
        Ok(total)
    }
}
