//! The `aws2-translate` component (Camel `camel-aws2-translate`, producer) over the Amazon
//! Translate AWS JSON 1.1 protocol (signing name `translate`).
//!
//! `aws2-translate://label?sourceLanguage=en&targetLanguage=it`
//!
//! `translateText` (the only operation, `operation` / `CamelAwsTranslateOperation`): the body
//! text is translated from `sourceLanguage` / `CamelAwsTranslateSourceLanguage` (or `auto`
//! with `autodetectSourceLanguage=true`) to `targetLanguage` /
//! `CamelAwsTranslateTargetLanguage`, with the optional `CamelAwsTranslateTerminologyNames`
//! (list). Body = translated text; the detected source language is set in
//! `CamelAwsTranslateSourceLanguage` when auto-detected.

use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::{ObjectMap, Value};

use super::{hdr, AwsConfig, JsonApi};
use crate::endpoint::{operation, unsupported, CloudService};
use crate::util::{header_list, kinds, Opts};

/// Header names (Camel `Translate2Constants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAwsTranslateOperation";
    pub const SOURCE_LANGUAGE: &str = "CamelAwsTranslateSourceLanguage";
    pub const TARGET_LANGUAGE: &str = "CamelAwsTranslateTargetLanguage";
    pub const TERMINOLOGY_NAMES: &str = "CamelAwsTranslateTerminologyNames";
}

const API: JsonApi = JsonApi { host: "translate", service: "translate", target: "AWSShineFrontendService_20170701", version: "1.1", kind: kinds::TRANSLATE };

/// The Translate endpoint configuration and client.
pub struct TranslateService {
    pub aws: AwsConfig,
    pub operation: Option<String>,
    pub source_language: Option<String>,
    pub target_language: Option<String>,
    pub autodetect: bool,
}

/// Factory for the component.
pub fn create(_remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let aws = AwsConfig::from_opts(o)?;
    o.ignore(&["translateClient", "pojoRequest"]);
    Ok(Arc::new(TranslateService {
        operation: o.get("operation"),
        source_language: o.get("sourceLanguage"),
        target_language: o.get("targetLanguage"),
        autodetect: o.bool("autodetectSourceLanguage", false)?,
        aws,
    }))
}

impl CloudService for TranslateService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "translateText");
        if op != "translateText" {
            return Err(unsupported("Translate", &op));
        }
        let source = if self.autodetect { Some("auto".to_string()) } else { hdr(ex, headers::SOURCE_LANGUAGE, &self.source_language) };
        let source = source.ok_or_else(|| CamelError::illegal_argument("A source language is required (sourceLanguage, CamelAwsTranslateSourceLanguage or autodetectSourceLanguage=true)"))?;
        let target = hdr(ex, headers::TARGET_LANGUAGE, &self.target_language).ok_or_else(|| CamelError::illegal_argument("A target language is required (targetLanguage or CamelAwsTranslateTargetLanguage)"))?;
        let mut m = ObjectMap::new();
        m.insert("Text".into(), Value::from(ex.body().to_string()));
        m.insert("SourceLanguageCode".into(), Value::from(source.clone()));
        m.insert("TargetLanguageCode".into(), Value::from(target));
        let terms = header_list(ex, headers::TERMINOLOGY_NAMES);
        if !terms.is_empty() {
            m.insert("TerminologyNames".into(), Value::from(terms));
        }
        let r = self.aws.api(&API, "TranslateText", &Value::Object(m))?;
        if source == "auto" {
            if let Some(s) = r.get("SourceLanguageCode") {
                ex.set_header(headers::SOURCE_LANGUAGE, s.clone());
            }
        }
        ex.set_body(r.get("TranslatedText").cloned().unwrap_or_default());
        Ok(())
    }
}
