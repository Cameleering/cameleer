//! The `aws2-sqs` component (Camel `camel-aws2-sqs`) over the SQS AWS JSON 1.0 protocol.
//!
//! `aws2-sqs://queueNameOrArn?...`
//!
//! Producer operations (`operation` option or `CamelAwsSqsOperation` header; default
//! `sendMessage`): `sendMessage` (body, `delaySeconds` / `CamelAwsSqsDelaySeconds`, user headers
//! become message attributes; FIFO queues get `MessageGroupId` per `messageGroupIdStrategy`
//! (`useConstant`, `useExchangeId`, `usePropertyValue` = property `CamelAwsSqsMessageGroupId`) and
//! `MessageDeduplicationId` per `messageDeduplicationIdStrategy` (`useExchangeId` default,
//! `useContentBasedDeduplication`)), `sendBatchMessage` (body = array), `deleteMessage`
//! (`CamelAwsSqsReceiptHandle`), `listQueues` (`CamelAwsSqsPrefix`), `purgeQueue`, `deleteQueue`.
//!
//! Consumer: `ReceiveMessage` with `maxMessagesPerPoll` (default 1, at most 10),
//! `waitTimeSeconds`, `visibilityTimeout`, `messageAttributeNames` (default `All`),
//! `attributeNames` (default `All`); headers `CamelAwsSqsMessageId`, `CamelAwsSqsReceiptHandle`,
//! `CamelAwsSqsMD5OfBody`, `CamelAwsSqsAttributes`, `CamelAwsSqsMessageAttributes` and one
//! header per message attribute. Successfully processed messages are deleted
//! (`deleteAfterRead`, default true); failed ones become visible again after the visibility timeout.
//! With `extendMessageVisibility=true` (requires `visibilityTimeout`) the visibility of a message
//! being processed is extended to 1.5 x `visibilityTimeout` every `visibilityTimeout / 2`
//! (`ChangeMessageVisibility`), as Camel's timeout extender does.
//!
//! Other options: `queueUrl`, `autoCreateQueue` (default false), `queueOwnerAWSAccountId`,
//! `defaultVisibilityTimeout`, `messageRetentionPeriod`, plus the [common AWS options](super).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::Value;

use super::{error_code, AwsConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::util::{header_str, kinds, obj, user_headers, Opts};

/// Header names (Camel `Sqs2Constants`).
pub mod headers {
    pub const MESSAGE_ID: &str = "CamelAwsSqsMessageId";
    pub const RECEIPT_HANDLE: &str = "CamelAwsSqsReceiptHandle";
    pub const MD5_OF_BODY: &str = "CamelAwsSqsMD5OfBody";
    pub const ATTRIBUTES: &str = "CamelAwsSqsAttributes";
    pub const MESSAGE_ATTRIBUTES: &str = "CamelAwsSqsMessageAttributes";
    pub const DELAY_HEADER: &str = "CamelAwsSqsDelaySeconds";
    pub const OPERATION: &str = "CamelAwsSqsOperation";
    pub const SQS_QUEUE_PREFIX: &str = "CamelAwsSqsPrefix";
    /// Exchange property used by `messageGroupIdStrategy=usePropertyValue`.
    pub const MESSAGE_GROUP_ID_PROPERTY: &str = "CamelAwsSqsMessageGroupId";
}

/// The SQS endpoint configuration and client.
pub struct SqsService {
    pub aws: AwsConfig,
    pub queue_name: String,
    pub operation: Option<String>,
    pub queue_url: Option<String>,
    pub delete_after_read: bool,
    pub max_messages_per_poll: i64,
    pub wait_time_seconds: Option<i64>,
    pub visibility_timeout: Option<i64>,
    pub message_attribute_names: Vec<String>,
    pub attribute_names: Vec<String>,
    pub auto_create_queue: bool,
    pub delay_seconds: Option<i64>,
    pub queue_owner: Option<String>,
    pub default_visibility_timeout: Option<i64>,
    pub message_retention_period: Option<i64>,
    pub message_group_id_strategy: Option<String>,
    pub message_deduplication_id_strategy: String,
    pub extend_message_visibility: bool,
    pub scheduler: ScheduledPollConfig,
    resolved_url: Mutex<Option<String>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    Ok(Arc::new(SqsService::from_opts(remaining, o)?))
}

fn list(v: Option<String>, default: &[&str]) -> Vec<String> {
    match v {
        Some(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        None => default.iter().map(|s| s.to_string()).collect(),
    }
}

impl SqsService {
    pub fn from_opts(remaining: &str, o: &mut Opts) -> Result<SqsService> {
        let queue_name = remaining.rsplit(':').next().unwrap_or(remaining).to_string();
        if queue_name.is_empty() {
            return Err(CamelError::illegal_argument("Queue name must be specified: aws2-sqs://queueName"));
        }
        let aws = AwsConfig::from_opts(o)?;
        let opt_i = |o: &mut Opts, k: &str| -> Result<Option<i64>> { o.get(k).map(|v| v.parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid number for {}: {}", k, v)))).transpose() };
        let s = SqsService {
            operation: o.get("operation"),
            queue_url: o.get("queueUrl"),
            delete_after_read: o.bool("deleteAfterRead", true)?,
            max_messages_per_poll: o.i64("maxMessagesPerPoll", 1)?,
            wait_time_seconds: opt_i(o, "waitTimeSeconds")?,
            visibility_timeout: opt_i(o, "visibilityTimeout")?,
            message_attribute_names: list(o.get("messageAttributeNames"), &["All"]),
            attribute_names: list(o.get("attributeNames"), &["All"]),
            auto_create_queue: o.bool("autoCreateQueue", false)?,
            delay_seconds: opt_i(o, "delaySeconds")?,
            queue_owner: o.get("queueOwnerAWSAccountId"),
            default_visibility_timeout: opt_i(o, "defaultVisibilityTimeout")?,
            message_retention_period: opt_i(o, "messageRetentionPeriod")?,
            message_group_id_strategy: o.get("messageGroupIdStrategy"),
            message_deduplication_id_strategy: o.string("messageDeduplicationIdStrategy", "useExchangeId"),
            extend_message_visibility: o.bool("extendMessageVisibility", false)?,
            scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
            resolved_url: Mutex::new(None),
            queue_name,
            aws,
        };
        if s.extend_message_visibility && s.visibility_timeout.is_none_or(|v| v <= 0) {
            return Err(CamelError::illegal_argument("extendMessageVisibility=true requires a positive visibilityTimeout"));
        }
        o.ignore(&["concurrentConsumers", "deleteIfFiltered", "delayQueue", "serverSideEncryptionEnabled", "kmsMasterKeyId", "kmsDataKeyReusePeriodSeconds", "batchSeparator", "sortAttributeName"]);
        Ok(s)
    }

    /// Invoke an SQS JSON API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("sqs", "sqs", "AmazonSQS", action, &body, "1.0", kinds::SQS)
    }

    fn is_fifo(&self) -> bool {
        self.queue_name.ends_with(".fifo")
    }

    /// Create the queue (idempotent) and return its URL.
    pub fn create_queue(&self) -> Result<String> {
        let mut attrs = BTreeMap::new();
        if let Some(v) = self.default_visibility_timeout {
            attrs.insert("VisibilityTimeout".to_string(), Value::from(v.to_string()));
        }
        if let Some(v) = self.message_retention_period {
            attrs.insert("MessageRetentionPeriod".to_string(), Value::from(v.to_string()));
        }
        if self.is_fifo() {
            attrs.insert("FifoQueue".to_string(), Value::from("true"));
            if self.message_deduplication_id_strategy == "useContentBasedDeduplication" {
                attrs.insert("ContentBasedDeduplication".to_string(), Value::from("true"));
            }
        }
        let r = self.call("CreateQueue", obj([("QueueName", Value::from(self.queue_name.clone())), ("Attributes", Value::Object(attrs))]))?;
        Ok(r.get("QueueUrl").map(|v| v.to_string()).unwrap_or_default())
    }

    /// The queue URL (option, cached, `GetQueueUrl`, or created when `autoCreateQueue`).
    pub fn queue_url(&self) -> Result<String> {
        if let Some(u) = &self.queue_url {
            return Ok(u.clone());
        }
        if let Some(u) = self.resolved_url.lock().unwrap().clone() {
            return Ok(u);
        }
        let mut req = obj([("QueueName", Value::from(self.queue_name.clone()))]);
        if let (Some(owner), Value::Object(m)) = (&self.queue_owner, &mut req) {
            m.insert("QueueOwnerAWSAccountId".into(), Value::from(owner.clone()));
        }
        let url = match self.call("GetQueueUrl", req) {
            Ok(r) => r.get("QueueUrl").map(|v| v.to_string()).unwrap_or_default(),
            Err(e) if self.auto_create_queue && error_code(&e).is_some_and(|c| c.contains("QueueDoesNotExist") || c.contains("NonExistentQueue")) => self.create_queue()?,
            Err(e) => return Err(e),
        };
        *self.resolved_url.lock().unwrap() = Some(url.clone());
        Ok(url)
    }

    fn attributes_of(ex: &Exchange) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in user_headers(ex) {
            let (dt, sv) = match &v {
                Value::Int(_) | Value::Float(_) => ("Number", v.to_string()),
                Value::Bool(_) => ("String", v.to_string()),
                _ => ("String", v.to_string()),
            };
            m.insert(k, obj([("DataType", Value::from(dt)), ("StringValue", Value::from(sv))]));
        }
        Value::Object(m)
    }

    fn fifo_fields(&self, ex: &Exchange, entry: &mut BTreeMap<String, Value>, body: &str) {
        if !self.is_fifo() {
            return;
        }
        let group = match self.message_group_id_strategy.as_deref() {
            Some("useExchangeId") => ex.exchange_id.clone(),
            Some("usePropertyValue") => ex.property(headers::MESSAGE_GROUP_ID_PROPERTY).map(|v| v.to_string()).unwrap_or_default(),
            _ => "CamelSingleMessageGroup".to_string(),
        };
        entry.insert("MessageGroupId".into(), Value::from(group));
        if self.message_deduplication_id_strategy != "useContentBasedDeduplication" {
            entry.insert("MessageDeduplicationId".into(), Value::from(format!("{}-{}", ex.exchange_id, crate::util::md5_hex(body.as_bytes()))));
        }
    }

    fn produce_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        match op {
            "sendMessage" => {
                let body = ex.body_as::<String>()?;
                let mut m = BTreeMap::new();
                m.insert("QueueUrl".to_string(), Value::from(self.queue_url()?));
                m.insert("MessageBody".to_string(), Value::from(body.clone()));
                let delay = ex.header(headers::DELAY_HEADER).and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok())).or(self.delay_seconds);
                if let Some(d) = delay {
                    m.insert("DelaySeconds".into(), Value::Int(d));
                }
                let attrs = Self::attributes_of(ex);
                if attrs.as_object().is_some_and(|a| !a.is_empty()) {
                    m.insert("MessageAttributes".into(), attrs);
                }
                self.fifo_fields(ex, &mut m, &body);
                let r = self.call("SendMessage", Value::Object(m))?;
                ex.set_header(headers::MESSAGE_ID, r.get("MessageId").cloned().unwrap_or_default());
                ex.set_header(headers::MD5_OF_BODY, r.get("MD5OfMessageBody").cloned().unwrap_or_default());
            }
            "sendBatchMessage" => {
                let items: Vec<String> = match ex.body() {
                    Value::Array(a) => a.iter().map(|v| v.to_string()).collect(),
                    other => other.to_string().split(',').map(|s| s.to_string()).collect(),
                };
                let mut entries = Vec::new();
                for (i, body) in items.iter().enumerate() {
                    let mut e = BTreeMap::new();
                    e.insert("Id".to_string(), Value::from(i.to_string()));
                    e.insert("MessageBody".to_string(), Value::from(body.clone()));
                    if let Some(d) = self.delay_seconds {
                        e.insert("DelaySeconds".into(), Value::Int(d));
                    }
                    self.fifo_fields(ex, &mut e, &format!("{}{}", i, body));
                    entries.push(Value::Object(e));
                }
                let r = self.call("SendMessageBatch", obj([("QueueUrl", Value::from(self.queue_url()?)), ("Entries", Value::Array(entries))]))?;
                let pick = |name: &str| r.get(name).cloned().unwrap_or(Value::Array(vec![]));
                ex.set_body(obj([("successful", pick("Successful")), ("failed", pick("Failed"))]));
            }
            "deleteMessage" => {
                let handle = header_str(ex, headers::RECEIPT_HANDLE)
                    .ok_or_else(|| CamelError::illegal_argument("deleteMessage requires the CamelAwsSqsReceiptHandle header"))?;
                self.call("DeleteMessage", obj([("QueueUrl", Value::from(self.queue_url()?)), ("ReceiptHandle", Value::from(handle))]))?;
            }
            "listQueues" => {
                let mut req = BTreeMap::new();
                if let Some(p) = header_str(ex, headers::SQS_QUEUE_PREFIX) {
                    req.insert("QueueNamePrefix".to_string(), Value::from(p));
                }
                let r = self.call("ListQueues", Value::Object(req))?;
                ex.set_body(r.get("QueueUrls").cloned().unwrap_or(Value::Array(vec![])));
            }
            "purgeQueue" => {
                self.call("PurgeQueue", obj([("QueueUrl", Value::from(self.queue_url()?))]))?;
            }
            "deleteQueue" => {
                self.call("DeleteQueue", obj([("QueueUrl", Value::from(self.queue_url()?))]))?;
                *self.resolved_url.lock().unwrap() = None;
            }
            other => return Err(unsupported("aws2-sqs", other)),
        }
        Ok(())
    }
}

type Extender = (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>);

impl SqsService {
    /// Start the visibility extender of an in-flight message (when enabled).
    fn start_extender(&self, url: &str, handle: &str, logger: &str) -> Option<Extender> {
        let vis = self.visibility_timeout.filter(|_| self.extend_message_visibility)?;
        let period = Duration::from_millis((vis.max(1) as u64) * 500);
        let extend_to = (vis * 3 + 1) / 2;
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (aws, url, handle, logger) = (self.aws.clone(), url.to_string(), handle.to_string(), logger.to_string());
        let join = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(period) {
                let req = obj([("QueueUrl", Value::from(url.clone())), ("ReceiptHandle", Value::from(handle.clone())), ("VisibilityTimeout", Value::Int(extend_to))]);
                if let Err(e) = aws.json_call("sqs", "sqs", "AmazonSQS", "ChangeMessageVisibility", &req, "1.0", kinds::SQS) {
                    camel_core::camel_warn!(logger.as_str(), "Error extending the visibility of SQS message: {}", e);
                }
            }
        });
        Some((tx, join))
    }
}

impl CloudService for SqsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "sendMessage");
        self.produce_op(ex, &op)
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if self.auto_create_queue && self.queue_url.is_none() {
            let url = self.create_queue()?;
            *self.resolved_url.lock().unwrap() = Some(url);
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let url = self.queue_url()?;
        let mut req = BTreeMap::new();
        req.insert("QueueUrl".to_string(), Value::from(url.clone()));
        req.insert("MaxNumberOfMessages".to_string(), Value::Int(self.max_messages_per_poll.clamp(1, 10)));
        if let Some(w) = self.wait_time_seconds {
            req.insert("WaitTimeSeconds".into(), Value::Int(w));
        }
        if let Some(v) = self.visibility_timeout {
            req.insert("VisibilityTimeout".into(), Value::Int(v));
        }
        req.insert("MessageAttributeNames".into(), Value::from(self.message_attribute_names.clone()));
        req.insert("AttributeNames".into(), Value::from(self.attribute_names.clone()));
        let r = self.call("ReceiveMessage", Value::Object(req))?;
        let messages = r.get("Messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
        let total = messages.len();
        for (i, m) in messages.iter().enumerate() {
            let mut ex = Exchange::new(ctx.clone());
            ex.set_body(m.get("Body").map(|b| b.to_string()).unwrap_or_default());
            let handle = m.get("ReceiptHandle").map(|v| v.to_string()).unwrap_or_default();
            ex.set_header(headers::MESSAGE_ID, m.get("MessageId").cloned().unwrap_or_default());
            ex.set_header(headers::RECEIPT_HANDLE, handle.clone());
            ex.set_header(headers::MD5_OF_BODY, m.get("MD5OfBody").cloned().unwrap_or_default());
            ex.set_header(headers::ATTRIBUTES, m.get("Attributes").cloned().unwrap_or(Value::Object(BTreeMap::new())));
            let mut attrs = BTreeMap::new();
            if let Some(Value::Object(ma)) = m.get("MessageAttributes") {
                for (k, v) in ma {
                    let val = match (v.get("DataType").map(|d| d.to_string()), v.get("StringValue")) {
                        (Some(dt), Some(s)) if dt.starts_with("Number") => {
                            let s = s.to_string();
                            s.parse::<i64>().map(Value::Int).or_else(|_| s.parse::<f64>().map(Value::Float)).unwrap_or(Value::String(s))
                        }
                        (_, Some(s)) => s.clone(),
                        _ => v.get("BinaryValue").cloned().unwrap_or_default(),
                    };
                    ex.set_header(k.clone(), val.clone());
                    attrs.insert(k.clone(), val);
                }
            }
            ex.set_header(headers::MESSAGE_ATTRIBUTES, Value::Object(attrs));
            set_batch(&mut ex, i, total);
            let extender = self.start_extender(&url, &handle, logger);
            let ok = process(logger, processor, &mut ex);
            if let Some((stop, join)) = extender {
                let _ = stop.send(());
                let _ = join.join();
            }
            if ok && self.delete_after_read {
                if let Err(e) = self.call("DeleteMessage", obj([("QueueUrl", Value::from(url.clone())), ("ReceiptHandle", Value::from(handle))])) {
                    camel_core::camel_warn!(logger, "Error deleting SQS message: {}", e);
                }
            }
        }
        Ok(total)
    }
}
