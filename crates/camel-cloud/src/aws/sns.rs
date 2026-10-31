//! The `aws2-sns` component (Camel `camel-aws2-sns`, producer only) over the SNS Query API.
//!
//! `aws2-sns://topicNameOrArn?subject=...&autoCreateTopic=true&messageStructure=json`
//!
//! Publishes the body (`Publish`) with the subject (`subject` / `CamelAwsSnsSubject`),
//! message structure (`messageStructure` / `CamelAwsSnsMessageStructure`) and the user headers
//! as message attributes; sets `CamelAwsSnsMessageId`. The topic ARN is the path when it is an
//! ARN, otherwise it is created (`autoCreateTopic`, default false) or looked up with
//! `ListTopics`. FIFO topics (`.fifo`) get `MessageGroupId` (`messageGroupIdStrategy`) and a
//! deduplication id (`messageDeduplicationIdStrategy`).

use std::sync::{Arc, Mutex};

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::AwsConfig;
use crate::endpoint::CloudService;
use crate::http::Request;
use crate::util::{cloud_error, encode_form, header_str, kinds, user_headers, Opts, Xml};

/// Header names (Camel `Sns2Constants`).
pub mod headers {
    pub const MESSAGE_ID: &str = "CamelAwsSnsMessageId";
    pub const SUBJECT: &str = "CamelAwsSnsSubject";
    pub const MESSAGE_STRUCTURE: &str = "CamelAwsSnsMessageStructure";
    pub const MESSAGE_GROUP_ID_PROPERTY: &str = "CamelAwsSnsMessageGroupId";
}

/// The SNS endpoint configuration and client.
pub struct SnsService {
    pub aws: AwsConfig,
    pub topic: String,
    pub subject: Option<String>,
    pub message_structure: Option<String>,
    pub auto_create_topic: bool,
    pub message_group_id_strategy: Option<String>,
    pub message_deduplication_id_strategy: String,
    topic_arn: Mutex<Option<String>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Topic name must be specified: aws2-sns://topicName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    let s = SnsService {
        topic: remaining.to_string(),
        subject: o.get("subject"),
        message_structure: o.get("messageStructure"),
        auto_create_topic: o.bool("autoCreateTopic", false)?,
        message_group_id_strategy: o.get("messageGroupIdStrategy"),
        message_deduplication_id_strategy: o.string("messageDeduplicationIdStrategy", "useExchangeId"),
        topic_arn: Mutex::new(None),
        aws,
    };
    o.ignore(&["subscribeSNStoSQS", "queueArn", "policy", "serverSideEncryptionEnabled", "kmsMasterKeyId", "queueUrl"]);
    Ok(Arc::new(s))
}

impl SnsService {
    /// Invoke a Query API action; returns the parsed XML response.
    pub fn call(&self, action: &str, params: Vec<(String, String)>) -> Result<Xml> {
        let mut all = vec![("Action".to_string(), action.to_string()), ("Version".to_string(), "2010-03-31".to_string())];
        all.extend(params);
        let req = Request::new("POST", self.aws.endpoint("sns")?)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=utf-8")
            .body(encode_form(&all).into_bytes());
        let resp = self.aws.send(req, "sns", kinds::SNS)?;
        Xml::parse(&resp.text())
    }

    /// The topic ARN (path, cached, created or looked up).
    pub fn topic_arn(&self) -> Result<String> {
        if self.topic.starts_with("arn:") {
            return Ok(self.topic.clone());
        }
        if let Some(a) = self.topic_arn.lock().unwrap().clone() {
            return Ok(a);
        }
        let arn = if self.auto_create_topic {
            let mut p = vec![("Name".to_string(), self.topic.clone())];
            if self.topic.ends_with(".fifo") {
                p.push(("Attributes.entry.1.key".into(), "FifoTopic".into()));
                p.push(("Attributes.entry.1.value".into(), "true".into()));
            }
            self.call("CreateTopic", p)?.find_text("TopicArn").unwrap_or_default()
        } else {
            let mut token: Option<String> = None;
            loop {
                let p = token.iter().map(|t| ("NextToken".to_string(), t.clone())).collect();
                let x = self.call("ListTopics", p)?;
                if let Some(a) = x.descendants("TopicArn").into_iter().map(|i| x.text(i)).find(|a| a.rsplit(':').next() == Some(self.topic.as_str())) {
                    break a;
                }
                token = x.find_text("NextToken").filter(|t| !t.is_empty());
                if token.is_none() {
                    return Err(cloud_error(kinds::SNS, format!("Topic {} does not exist (set autoCreateTopic=true to create it) (Service: sns, Status Code: 404, Error Code: NotFound)", self.topic)));
                }
            }
        };
        *self.topic_arn.lock().unwrap() = Some(arn.clone());
        Ok(arn)
    }
}

impl CloudService for SnsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let message = ex.body_as::<String>()?;
        let mut p = vec![("TopicArn".to_string(), self.topic_arn()?), ("Message".to_string(), message.clone())];
        if let Some(s) = header_str(ex, headers::SUBJECT).or_else(|| self.subject.clone()) {
            p.push(("Subject".into(), s));
        }
        if let Some(s) = header_str(ex, headers::MESSAGE_STRUCTURE).or_else(|| self.message_structure.clone()) {
            p.push(("MessageStructure".into(), s));
        }
        for (i, (k, v)) in user_headers(ex).into_iter().enumerate() {
            let n = i + 1;
            let dt = if matches!(v, Value::Int(_) | Value::Float(_)) { "Number" } else { "String" };
            p.push((format!("MessageAttributes.entry.{}.Name", n), k));
            p.push((format!("MessageAttributes.entry.{}.Value.DataType", n), dt.into()));
            p.push((format!("MessageAttributes.entry.{}.Value.StringValue", n), v.to_string()));
        }
        if self.topic.ends_with(".fifo") {
            let group = match self.message_group_id_strategy.as_deref() {
                Some("useExchangeId") => ex.exchange_id.clone(),
                Some("usePropertyValue") => ex.property(headers::MESSAGE_GROUP_ID_PROPERTY).map(|v| v.to_string()).unwrap_or_default(),
                _ => "CamelSingleMessageGroup".to_string(),
            };
            p.push(("MessageGroupId".into(), group));
            if self.message_deduplication_id_strategy != "useContentBasedDeduplication" {
                p.push(("MessageDeduplicationId".into(), ex.exchange_id.clone()));
            }
        }
        let x = self.call("Publish", p)?;
        ex.set_header(headers::MESSAGE_ID, x.find_text("MessageId").unwrap_or_default());
        Ok(())
    }

    fn on_start(&self) -> Result<()> {
        if self.auto_create_topic {
            self.topic_arn()?;
        }
        Ok(())
    }
}
