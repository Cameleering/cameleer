//! The `aws2-comprehend` component (Camel `camel-aws-comprehend`, producer) over the Amazon
//! Comprehend AWS JSON 1.1 protocol (signing name `comprehend`). Also registered as
//! `aws-comprehend`.
//!
//! `aws2-comprehend://label?operation=detectSentiment&languageCode=en`
//!
//! Operations (`operation` / `CamelAwsComprehendOperation`) on the body text:
//! `detectDominantLanguage` (body = list of `{LanguageCode, Score}`, the best one in
//! `CamelAwsComprehendDetectedLanguage`), `detectEntities` / `detectKeyPhrases` /
//! `detectPiiEntities` / `detectSyntax` (body = list of entities / key phrases / tokens),
//! `detectSentiment` (body = sentiment, `CamelAwsComprehendSentimentScore` = score map),
//! `containsPiiEntities` (body = labels), `detectToxicContent` (body = result list),
//! `classifyDocument` (`endpointArn` / `CamelAwsComprehendEndpointArn`; body = classes).
//! The language comes from `languageCode` / `CamelAwsComprehendLanguageCode` (default `en`).

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use super::{hdr, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{kinds, obj, Opts};

/// Header names.
pub mod headers {
    pub const OPERATION: &str = "CamelAwsComprehendOperation";
    pub const LANGUAGE_CODE: &str = "CamelAwsComprehendLanguageCode";
    pub const ENDPOINT_ARN: &str = "CamelAwsComprehendEndpointArn";
    pub const DETECTED_LANGUAGE: &str = "CamelAwsComprehendDetectedLanguage";
    pub const SENTIMENT_SCORE: &str = "CamelAwsComprehendSentimentScore";
}

const API: JsonApi = JsonApi { host: "comprehend", service: "comprehend", target: "Comprehend_20171127", version: "1.1", kind: kinds::COMPREHEND };

/// The Comprehend endpoint configuration and client.
pub struct ComprehendService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub language_code: Option<String>,
    pub endpoint_arn: Option<String>,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["comprehendClient", "pojoRequest"]);
    Ok(Arc::new(ComprehendService { operation: o.get("operation"), language_code: o.get("languageCode"), endpoint_arn: o.get("endpointArn"), aws }))
}

impl CloudService for ComprehendService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "detectSentiment");
        let text = Value::from(ex.body().to_string());
        let lang = Value::from(hdr(ex, headers::LANGUAGE_CODE, &self.language_code).unwrap_or_else(|| "en".into()));
        let call = |action: &str, body: Value| self.aws.api(&API, action, &body);
        let with_lang = |action: &str| call(action, obj([("Text", text.clone()), ("LanguageCode", lang.clone())]));
        let field = |r: &Value, f: &str| r.get(f).cloned().unwrap_or(Value::Array(vec![]));
        match op.as_str() {
            "detectDominantLanguage" => {
                let r = call("DetectDominantLanguage", obj([("Text", text.clone())]))?;
                let langs = field(&r, "Languages");
                let best = langs
                    .as_array()
                    .and_then(|a| a.iter().max_by(|x, y| x.get("Score").and_then(|s| s.as_f64()).partial_cmp(&y.get("Score").and_then(|s| s.as_f64())).unwrap_or(std::cmp::Ordering::Equal)))
                    .and_then(|l| l.get("LanguageCode").cloned());
                if let Some(b) = best {
                    ex.set_header(headers::DETECTED_LANGUAGE, b);
                }
                ex.set_body(langs);
            }
            "detectEntities" => ex.set_body(field(&with_lang("DetectEntities")?, "Entities")),
            "detectKeyPhrases" => ex.set_body(field(&with_lang("DetectKeyPhrases")?, "KeyPhrases")),
            "detectPiiEntities" => ex.set_body(field(&with_lang("DetectPiiEntities")?, "Entities")),
            "detectSyntax" => ex.set_body(field(&with_lang("DetectSyntax")?, "SyntaxTokens")),
            "containsPiiEntities" => ex.set_body(field(&with_lang("ContainsPiiEntities")?, "Labels")),
            "detectSentiment" => {
                let r = with_lang("DetectSentiment")?;
                ex.set_header(headers::SENTIMENT_SCORE, r.get("SentimentScore").cloned().unwrap_or_default());
                ex.set_body(r.get("Sentiment").cloned().unwrap_or_default());
            }
            "detectToxicContent" => {
                let r = call("DetectToxicContent", obj([("TextSegments", Value::Array(vec![obj([("Text", text.clone())])])), ("LanguageCode", lang.clone())]))?;
                ex.set_body(field(&r, "ResultList"));
            }
            "classifyDocument" => {
                let arn = hdr(ex, headers::ENDPOINT_ARN, &self.endpoint_arn).ok_or_else(|| CamelError::illegal_argument("classifyDocument requires endpointArn"))?;
                let r = call("ClassifyDocument", obj([("Text", text.clone()), ("EndpointArn", Value::from(arn))]))?;
                ex.set_body(field(&r, "Classes"));
            }
            other => return Err(unsupported("Comprehend", other)),
        }
        Ok(())
    }
}
