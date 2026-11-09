//! The `dicom` and `dicomJson` data formats.
//!
//! - `dicom`: `unmarshal` reads Part 10 bytes into the Value tree ([`crate::model`]);
//!   `marshal` writes a Value tree (or Part 10 bytes) as Part 10, optionally in another native
//!   transfer syntax (`transferSyntax`).
//! - `dicomJson`: `marshal` writes the DICOM JSON model (PS3.18 Annex F) of a Value tree or of
//!   Part 10 bytes; `unmarshal` reads DICOM JSON into a Value tree (an array for a JSON array).

use std::collections::BTreeMap;

use cameleer_core::dataformat::{base64, DataFormat};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::codec::{DicomFile, ReadOptions};
use crate::headers;
use crate::model;
use crate::ts::TransferSyntax;

fn invalid(msg: impl Into<String>) -> CamelError {
    CamelError::new("InvalidPayloadException", msg)
}

/// Part 10 bytes of a body: bytes as they are, a Value tree marshalled.
pub fn body_bytes(body: &Value) -> Result<Vec<u8>> {
    match body {
        Value::Bytes(b) => Ok(b.clone()),
        Value::Object(_) => model::value_to_file(body)?.to_bytes(),
        Value::Null => Err(invalid("No DICOM object in the message body (it is null)")),
        other => Err(invalid(format!("Expected Part 10 bytes or a dicom Value tree in the body, got {}", other.type_name()))),
    }
}

/// Re-encode a file in another transfer syntax. Only native syntaxes can be converted into
/// each other; anything involving a compressed syntax needs a codec, which this build does not
/// have, and fails naming both syntaxes.
pub fn transcode(f: &mut DicomFile, target: &str) -> Result<()> {
    let from = f.transfer_syntax();
    if from.uid == target {
        return Ok(());
    }
    let to = TransferSyntax::from_uid(target);
    if from.encapsulated || to.encapsulated {
        return Err(CamelError::new(
            "DicomTranscodeException",
            format!("Cannot convert {} to {}: pixel data codecs are not part of this build; only Implicit/Explicit VR Little Endian, Deflated and Explicit VR Big Endian convert into each other", from.name(), to.name()),
        ));
    }
    f.set_transfer_syntax(target);
    Ok(())
}

/// The `dicom` data format.
#[derive(Clone, Debug, Default)]
pub struct DicomDataFormat {
    /// `unmarshal` stops at Pixel Data and leaves it out.
    pub stop_before_pixel_data: bool,
    /// `marshal` writes in this transfer syntax (default: the one of the object).
    pub transfer_syntax: Option<String>,
}

impl DicomDataFormat {
    /// From data format options (`stopBeforePixelData`, `transferSyntax`).
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<DicomDataFormat> {
        let mut p = Parameters::new(o.clone());
        let df = DicomDataFormat { stop_before_pixel_data: p.take_bool("stopBeforePixelData", false)?, transfer_syntax: p.take("transferSyntax").filter(|s| !s.is_empty()) };
        reject_rest(p, "dicom")?;
        Ok(df)
    }
}

fn reject_rest(p: Parameters, name: &str) -> Result<()> {
    if !p.is_empty() {
        return Err(CamelError::new("IllegalArgumentException", format!("Unknown option(s) for the {} data format: {:?}", name, p)));
    }
    Ok(())
}

/// Set the identifying headers of an object.
pub fn set_object_headers(ex: &mut Exchange, f: &DicomFile) {
    if let Some(v) = f.dataset.sop_class_uid().or_else(|| f.meta.string(crate::tag::tags::MEDIA_STORAGE_SOP_CLASS_UID)) {
        ex.set_header(headers::SOP_CLASS_UID, v);
    }
    if let Some(v) = f.dataset.sop_instance_uid().or_else(|| f.meta.string(crate::tag::tags::MEDIA_STORAGE_SOP_INSTANCE_UID)) {
        ex.set_header(headers::SOP_INSTANCE_UID, v);
    }
    ex.set_header(headers::TRANSFER_SYNTAX_UID, f.transfer_syntax_uid());
}

impl DataFormat for DicomDataFormat {
    fn name(&self) -> &str {
        "dicom"
    }

    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let bytes = match (ex.body(), &self.transfer_syntax) {
            (Value::Bytes(b), None) => b.clone(),
            (Value::Bytes(b), Some(ts)) => {
                let mut f = DicomFile::parse(b, ReadOptions::default())?;
                transcode(&mut f, ts)?;
                f.to_bytes()?
            }
            (body @ Value::Object(_), ts) => {
                let mut f = model::value_to_file(body)?;
                if let Some(ts) = ts {
                    transcode(&mut f, ts)?;
                }
                f.to_bytes()?
            }
            (other, _) => return Err(invalid(format!("dicom marshal expects a dicom Value tree or Part 10 bytes, got {}", other.type_name()))),
        };
        ex.set_body(Value::Bytes(bytes));
        Ok(())
    }

    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let f = match ex.body() {
            Value::Bytes(b) => DicomFile::parse(b, ReadOptions { stop_before_pixel_data: self.stop_before_pixel_data })?,
            Value::Object(_) => return Ok(()), // already a tree
            other => return Err(invalid(format!("dicom unmarshal expects Part 10 bytes, got {}", other.type_name()))),
        };
        set_object_headers(ex, &f);
        ex.set_body(model::file_to_value(&f));
        Ok(())
    }
}

/// The `dicomJson` data format.
#[derive(Clone, Debug, Default)]
pub struct DicomJsonDataFormat {
    /// Keep the file meta information (group 0002), normally left out of DICOM JSON.
    pub include_file_meta: bool,
    /// Wrap a single object in an array, as QIDO-RS and WADO-RS metadata responses are.
    pub array: bool,
    /// Bulk values (OB, OW, UN... and encapsulated pixel data) larger than this many bytes are
    /// left out, or written as `BulkDataURI` when `bulkDataUri` is set (default: no limit;
    /// encapsulated pixel data is always left out unless a URI is set).
    pub bulk_data_threshold: Option<usize>,
    /// URI template for left-out bulk data: `{tag}` is replaced by the tag, `{sopInstanceUid}`
    /// by the instance UID.
    pub bulk_data_uri: Option<String>,
    pub pretty: bool,
}

impl DicomJsonDataFormat {
    /// From options (`includeFileMeta`, `array`, `bulkDataThreshold`, `bulkDataUri`, `pretty`).
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<DicomJsonDataFormat> {
        let mut p = Parameters::new(o.clone());
        let threshold = p.take("bulkDataThreshold").map(|s| s.parse::<usize>().map_err(|_| CamelError::new("IllegalArgumentException", format!("bulkDataThreshold must be a number: {}", s)))).transpose()?;
        let df = DicomJsonDataFormat {
            include_file_meta: p.take_bool("includeFileMeta", false)?,
            array: p.take_bool("array", false)?,
            bulk_data_threshold: threshold,
            bulk_data_uri: p.take("bulkDataUri").filter(|s| !s.is_empty()),
            pretty: p.take_bool("pretty", false)?,
        };
        reject_rest(p, "dicomJson")?;
        Ok(df)
    }

    /// The DICOM JSON model of one Value tree (base64 for binary, extension keys dropped).
    pub fn to_json_model(&self, tree: &Value) -> Value {
        let sop = tree
            .get("00080018")
            .and_then(|e| e.get("Value"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.clean(tree, true, &sop)
    }

    fn clean(&self, v: &Value, top: bool, sop: &str) -> Value {
        let Value::Object(m) = v else { return v.clone() };
        let mut out = ObjectMap::new();
        for (k, ev) in m.iter() {
            if k.starts_with('_') {
                continue;
            }
            if top && !self.include_file_meta && k.starts_with("0002") {
                continue;
            }
            let Value::Object(e) = ev else { continue };
            let mut ne = ObjectMap::new();
            if let Some(vr) = e.get("vr") {
                ne.insert("vr".into(), vr.clone());
            }
            let uri = || self.bulk_data_uri.as_ref().map(|u| u.replace("{tag}", k).replace("{sopInstanceUid}", sop));
            if let Some(Value::Array(items)) = e.get("Value").filter(|_| e.get("vr").and_then(|v| v.as_str()) == Some("SQ")) {
                ne.insert("Value".into(), Value::Array(items.iter().map(|it| self.clean(it, false, sop)).collect()));
            } else if let Some(val) = e.get("Value") {
                ne.insert("Value".into(), val.clone());
            } else if let Some(b) = e.get("InlineBinary") {
                let len = match b {
                    Value::Bytes(b) => b.len(),
                    Value::String(s) => s.len() * 3 / 4,
                    _ => 0,
                };
                if self.bulk_data_threshold.is_some_and(|t| len > t) {
                    match uri() {
                        Some(u) => {
                            ne.insert("BulkDataURI".into(), Value::String(u));
                        }
                        None => continue,
                    }
                } else {
                    let s = match b {
                        Value::Bytes(b) => base64::encode(b, false),
                        other => other.to_string(),
                    };
                    ne.insert("InlineBinary".into(), Value::String(s));
                }
            } else if e.contains_key("Fragments") {
                match uri() {
                    Some(u) => {
                        ne.insert("BulkDataURI".into(), Value::String(u));
                    }
                    None => continue,
                }
            } else if let Some(u) = e.get("BulkDataURI") {
                ne.insert("BulkDataURI".into(), u.clone());
            }
            out.insert(k.clone(), Value::Object(ne));
        }
        Value::Object(out)
    }

    fn tree_of(body: &Value) -> Result<Value> {
        match body {
            Value::Bytes(b) => Ok(model::file_to_value(&DicomFile::parse(b, ReadOptions::default())?)),
            Value::Object(_) => Ok(body.clone()),
            other => Err(invalid(format!("dicomJson marshal expects a dicom Value tree or Part 10 bytes, got {}", other.type_name()))),
        }
    }
}

/// Decode DICOM JSON (one object) into a Value tree: base64 `InlineBinary` becomes bytes.
pub fn json_model_to_tree(v: &Value) -> Result<Value> {
    let Value::Object(m) = v else { return Err(invalid("A DICOM JSON data set must be an object")) };
    let mut out = ObjectMap::new();
    for (k, ev) in m.iter() {
        let Value::Object(e) = ev else { return Err(invalid(format!("DICOM JSON element {} must be an object", k))) };
        let mut ne = e.clone();
        if let Some(Value::String(s)) = e.get("InlineBinary") {
            ne.insert("InlineBinary".into(), Value::Bytes(base64::decode(s)?));
        }
        if e.get("vr").and_then(|v| v.as_str()) == Some("SQ") {
            if let Some(Value::Array(items)) = e.get("Value") {
                ne.insert("Value".into(), Value::Array(items.iter().map(json_model_to_tree).collect::<Result<Vec<_>>>()?));
            }
        }
        out.insert(k.clone(), Value::Object(ne));
    }
    Ok(Value::Object(out))
}

impl DataFormat for DicomJsonDataFormat {
    fn name(&self) -> &str {
        "dicomJson"
    }

    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let model = match ex.body() {
            Value::Array(a) => Value::Array(a.iter().map(|b| Ok(self.to_json_model(&Self::tree_of(b)?))).collect::<Result<Vec<_>>>()?),
            body => {
                let m = self.to_json_model(&Self::tree_of(body)?);
                if self.array {
                    Value::Array(vec![m])
                } else {
                    m
                }
            }
        };
        let text = if self.pretty { model.to_json_pretty() } else { model.to_json() };
        ex.set_body(Value::String(text));
        ex.set_header("Content-Type", "application/dicom+json");
        Ok(())
    }

    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let parsed = match ex.body() {
            Value::String(s) => cameleer_core::json::parse(s)?,
            Value::Bytes(b) => cameleer_core::json::parse(&String::from_utf8_lossy(b))?,
            other => other.clone(),
        };
        let tree = match &parsed {
            Value::Array(a) => Value::Array(a.iter().map(json_model_to_tree).collect::<Result<Vec<_>>>()?),
            other => json_model_to_tree(other)?,
        };
        ex.set_body(tree);
        Ok(())
    }
}
