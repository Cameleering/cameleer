//! The `fhirJson` and `fhirXml` data formats.
//!
//! *Unmarshal* parses FHIR JSON (resp. XML) into a resource [`Value`] in the FHIR JSON model
//! (`resourceType`, arrays for repeating elements, `_name` for primitive extensions, numbers
//! and booleans typed). *Marshal* serializes a resource `Value` — or FHIR JSON / XML text, so
//! `marshal().fhirXml()` after `unmarshal().fhirJson()` or directly on JSON text converts
//! between the two encodings — to JSON (resp. XML) and sets `Content-Type`
//! (`application/fhir+json` / `application/fhir+xml`).
//!
//! Options: `fhirVersion` (`R4` default; `R4B`, `R5`, `DSTU3` use the same serialization
//! rules), `prettyPrint`, `suppressNarratives` (drop `text`), `omitResourceId`,
//! `encodeElements` / `dontEncodeElements` (comma separated, `Patient.name` or `name`),
//! `stripVersionsFromReferences`, `contentTypeHeader` (default true); `summaryMode`,
//! `serverBaseUrl`, `parserOptions`, `preferTypes`, `forceResourceId`,
//! `overrideResourceIdWithBundleEntryFullUrl`, `encodeElementsAppliesToChildResourcesOnly`
//! and `dontStripVersionsFromReferencesAtPaths` are accepted.
//!
//! Decimals go through `f64` in the `Value` model (`1.50` becomes `1.5`); the text-level
//! conversions [`super::json_to_xml`] / [`super::xml_to_json`] keep lexemes exactly.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;

use super::convert::{parse_json, tree_to_xml, xml_to_tree};
use super::json::{self, J};
use super::DATA_FORMAT_EXCEPTION;

/// A FHIR data format.
pub struct FhirDataFormat {
    xml: bool,
    pretty: bool,
    suppress_narratives: bool,
    omit_id: bool,
    encode_elements: Vec<String>,
    dont_encode: Vec<String>,
    strip_versions: bool,
    content_type_header: bool,
}

fn opt<'a>(o: &'a BTreeMap<String, String>, k: &str) -> Option<&'a str> {
    o.get(k).map(|s| s.as_str()).filter(|s| !s.is_empty())
}

fn flag(o: &BTreeMap<String, String>, k: &str, d: bool) -> Result<bool> {
    match opt(o, k).map(|s| s.to_ascii_lowercase()) {
        None => Ok(d),
        Some(v) if v == "true" => Ok(true),
        Some(v) if v == "false" => Ok(false),
        Some(v) => Err(CamelError::illegal_argument(format!("Option {} must be true or false but was: {}", k, v))),
    }
}

/// Check a `fhirVersion` option.
pub fn check_version(v: Option<&str>) -> Result<()> {
    match v.map(|s| s.trim().to_ascii_uppercase()) {
        None => Ok(()),
        Some(v) if matches!(v.as_str(), "R4" | "R4B" | "R5" | "DSTU3") => Ok(()),
        Some(v) => Err(CamelError::illegal_argument(format!("Unsupported fhirVersion {} (R4; R4B, R5 and DSTU3 share its serialization rules)", v))),
    }
}

impl FhirDataFormat {
    /// Create from options; `xml` selects `fhirXml`.
    pub fn from_options(o: &BTreeMap<String, String>, xml: bool) -> Result<FhirDataFormat> {
        super::register_kinds();
        check_version(opt(o, "fhirVersion"))?;
        let list = |k: &str| opt(o, k).map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        Ok(FhirDataFormat {
            xml,
            pretty: flag(o, "prettyPrint", false)?,
            suppress_narratives: flag(o, "suppressNarratives", false)?,
            omit_id: flag(o, "omitResourceId", false)?,
            encode_elements: list("encodeElements"),
            dont_encode: list("dontEncodeElements"),
            strip_versions: flag(o, "stripVersionsFromReferences", false)?,
            content_type_header: flag(o, "contentTypeHeader", true)?,
        })
    }

    fn filter(&self, j: &mut J, top: bool) {
        let rt = j.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
        let J::Obj(m) = j else { return };
        if self.suppress_narratives {
            m.retain(|(k, _)| k != "text");
        }
        if top {
            if self.omit_id {
                m.retain(|(k, _)| k != "id");
            }
            let matches = |list: &[String], k: &str| list.iter().any(|e| e == k || *e == format!("{}.{}", rt, k) || *e == format!("*.{}", k));
            if !self.encode_elements.is_empty() {
                m.retain(|(k, _)| matches!(k.as_str(), "resourceType" | "id" | "meta") || matches(&self.encode_elements, k.trim_start_matches('_')));
            }
            if !self.dont_encode.is_empty() {
                m.retain(|(k, _)| !matches(&self.dont_encode, k.trim_start_matches('_')));
            }
        }
        for (k, v) in m.iter_mut() {
            match v {
                J::Obj(_) => self.filter(v, false),
                J::Arr(a) => {
                    for x in a.iter_mut() {
                        self.filter(x, false);
                    }
                }
                J::Str(s) if self.strip_versions && k == "reference" => {
                    if let Some(i) = s.find("/_history/") {
                        s.truncate(i);
                    }
                }
                _ => {}
            }
        }
    }

    /// Serialize a body (resource value, or FHIR JSON / XML text).
    pub fn encode(&self, body: &Value) -> Result<String> {
        let mut j = body_tree(body)?;
        self.filter(&mut j, true);
        if self.xml {
            tree_to_xml(&j, self.pretty)
        } else {
            Ok(json::write(&j, self.pretty))
        }
    }

    /// Parse text of this format.
    pub fn decode(&self, text: &str) -> Result<Value> {
        let j = if self.xml { xml_to_tree(text)? } else { parse_json(text)? };
        Ok(j.to_value())
    }
}

/// A resource tree from a body: a resource value, or FHIR JSON / XML text.
pub fn body_tree(body: &Value) -> Result<J> {
    let text = match body {
        Value::String(s) => Some(s.clone()),
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    };
    let j = match text {
        Some(t) if crate::support::strip_xml_decl(&t).starts_with('<') => xml_to_tree(&t)?,
        Some(t) => parse_json(&t)?,
        None => J::from_value(body),
    };
    match j.get("resourceType") {
        Some(J::Str(_)) => Ok(j),
        _ => Err(CamelError::new(DATA_FORMAT_EXCEPTION, format!("Not a FHIR resource (no resourceType): {}", body.type_name()))),
    }
}

impl DataFormat for FhirDataFormat {
    fn name(&self) -> &str {
        if self.xml {
            "fhirXml"
        } else {
            "fhirJson"
        }
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let s = self.encode(&ex.message.body)?;
        ex.message.body = Value::String(s);
        if self.content_type_header {
            ex.message.headers.set("Content-Type", if self.xml { "application/fhir+xml" } else { "application/fhir+json" });
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match &ex.message.body {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            other => other.to_string(),
        };
        ex.message.body = self.decode(&text)?;
        Ok(())
    }
}

/// Factory of `fhirJson`.
pub fn json_factory(o: &BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(FhirDataFormat::from_options(o, false)?))
}

/// Factory of `fhirXml`.
pub fn xml_factory(o: &BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(FhirDataFormat::from_options(o, true)?))
}
