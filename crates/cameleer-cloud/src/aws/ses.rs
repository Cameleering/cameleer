//! The `aws2-ses` component (Camel `camel-aws2-ses`, producer) over the SES (v1) Query API.
//!
//! `aws2-ses://from@example.com?to=a@example.com,b@example.com&subject=...`
//!
//! Sends the body with `SendEmail` (text, or HTML when `CamelAwsSesHtmlEmail=true` / option
//! `htmlEmail`), or with `SendRawEmail` when the body is bytes (a complete MIME message).
//! Headers (override the options): `CamelAwsSesFrom`, `CamelAwsSesTo`, `CamelAwsSesCc`,
//! `CamelAwsSesBcc` (lists or comma separated), `CamelAwsSesSubject`, `CamelAwsSesReturnPath`,
//! `CamelAwsSesReplyToAddresses`, `CamelAwsSesConfigurationSet`; the message id is returned in
//! `CamelAwsSesMessageId`.

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use super::AwsConfig;
use crate::endpoint::CloudService;
use crate::util::{base64_encode, header_list, header_str, kinds, str_list, Opts};

/// Header names (Camel `Ses2Constants`).
pub mod headers {
    pub const FROM: &str = "CamelAwsSesFrom";
    pub const MESSAGE_ID: &str = "CamelAwsSesMessageId";
    pub const REPLY_TO_ADDRESSES: &str = "CamelAwsSesReplyToAddresses";
    pub const RETURN_PATH: &str = "CamelAwsSesReturnPath";
    pub const SUBJECT: &str = "CamelAwsSesSubject";
    pub const TO: &str = "CamelAwsSesTo";
    pub const CC: &str = "CamelAwsSesCc";
    pub const BCC: &str = "CamelAwsSesBcc";
    pub const HTML_EMAIL: &str = "CamelAwsSesHtmlEmail";
    pub const CONFIGURATION_SET: &str = "CamelAwsSesConfigurationSet";
}

/// The SES endpoint configuration and client.
pub struct SesService {
    pub aws: AwsConfig,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub return_path: Option<String>,
    pub reply_to: Vec<String>,
    pub configuration_set: Option<String>,
    pub html_email: bool,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Sender must be specified: aws2-ses://from@example.com"));
    }
    let aws = AwsConfig::from_opts(o)?;
    let list = |o: &mut Opts, k: &str| o.get(k).map(|v| str_list(&Value::from(v))).unwrap_or_default();
    let s = SesService {
        from: crate::util::percent_decode(remaining, false),
        to: list(o, "to"),
        cc: list(o, "cc"),
        bcc: list(o, "bcc"),
        subject: o.get("subject"),
        return_path: o.get("returnPath"),
        reply_to: list(o, "replyToAddresses"),
        configuration_set: o.get("configurationSet"),
        html_email: o.bool("htmlEmail", false)?,
        aws,
    };
    o.ignore(&["amazonSESClient"]);
    Ok(Arc::new(s))
}

fn members(out: &mut Vec<(String, String)>, prefix: &str, list: &[String]) {
    for (i, v) in list.iter().enumerate() {
        out.push((format!("{}.member.{}", prefix, i + 1), v.clone()));
    }
}

impl SesService {
    fn list(&self, ex: &Exchange, header: &str, default: &[String]) -> Vec<String> {
        let h = header_list(ex, header);
        if h.is_empty() {
            default.to_vec()
        } else {
            h
        }
    }
}

impl CloudService for SesService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let from = header_str(ex, headers::FROM).unwrap_or_else(|| self.from.clone());
        let to = self.list(ex, headers::TO, &self.to);
        let cc = self.list(ex, headers::CC, &self.cc);
        let bcc = self.list(ex, headers::BCC, &self.bcc);
        let mut p = vec![("Source".to_string(), from)];
        let raw = matches!(ex.body(), Value::Bytes(_));
        let action = if raw {
            p.push(("RawMessage.Data".into(), base64_encode(&ex.body().to_bytes())));
            let all: Vec<String> = to.iter().chain(cc.iter()).chain(bcc.iter()).cloned().collect();
            members(&mut p, "Destinations", &all);
            "SendRawEmail"
        } else {
            if to.is_empty() {
                return Err(CamelError::illegal_argument("No recipients: set the to option or the CamelAwsSesTo header"));
            }
            members(&mut p, "Destination.ToAddresses", &to);
            members(&mut p, "Destination.CcAddresses", &cc);
            members(&mut p, "Destination.BccAddresses", &bcc);
            let subject = header_str(ex, headers::SUBJECT).or_else(|| self.subject.clone()).unwrap_or_default();
            p.push(("Message.Subject.Data".into(), subject));
            p.push(("Message.Subject.Charset".into(), "UTF-8".into()));
            let html = ex.header(headers::HTML_EMAIL).map(|v| v.is_truthy()).unwrap_or(self.html_email);
            let part = if html { "Html" } else { "Text" };
            p.push((format!("Message.Body.{}.Data", part), ex.body_as::<String>()?));
            p.push((format!("Message.Body.{}.Charset", part), "UTF-8".into()));
            members(&mut p, "ReplyToAddresses", &self.list(ex, headers::REPLY_TO_ADDRESSES, &self.reply_to));
            if let Some(rp) = header_str(ex, headers::RETURN_PATH).or_else(|| self.return_path.clone()) {
                p.push(("ReturnPath".into(), rp));
            }
            "SendEmail"
        };
        if let Some(cs) = header_str(ex, headers::CONFIGURATION_SET).or_else(|| self.configuration_set.clone()) {
            p.push(("ConfigurationSetName".into(), cs));
        }
        let x = self.aws.query_call(self.aws.endpoint("email")?, "ses", &self.aws.region.clone(), "2010-12-01", action, p, kinds::SES)?;
        ex.set_header(headers::MESSAGE_ID, x.find_text("MessageId").unwrap_or_default());
        Ok(())
    }
}
