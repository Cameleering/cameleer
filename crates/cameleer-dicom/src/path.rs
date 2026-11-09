//! Tag paths: `PatientID`, `(0010,0020)`, `00100020`, `ReferencedSeriesSequence[0].SeriesInstanceUID`,
//! `ImageType[1]`. Used by `${dicom.<path>}` in Simple and by the `dicom` language.

use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::Value;

use crate::charset::CharacterSet;
use crate::codec::{DicomFile, ReadOptions};
use crate::object::{DataSet, Element};
use crate::tag::{self, tags};

/// One step of a path.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub tag: u32,
    pub index: Option<usize>,
}

/// A parsed path.
#[derive(Clone, Debug, PartialEq)]
pub struct TagPath {
    pub steps: Vec<Step>,
}

impl TagPath {
    /// Parse a path; keywords are looked up in the dictionary.
    pub fn parse(text: &str) -> Result<TagPath> {
        let bad = |m: &str| CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid DICOM tag path '{}': {}", text, m));
        let mut steps = Vec::new();
        let s = text.trim();
        if s.is_empty() {
            return Err(bad("empty"));
        }
        // split at dots outside parentheses
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0, 0);
        for (i, c) in s.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                '.' if depth == 0 => {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&s[start..]);
        for p in parts {
            let p = p.trim();
            let (name, index) = match p.rfind('[') {
                Some(k) if p.ends_with(']') => {
                    let idx: usize = p[k + 1..p.len() - 1].trim().parse().map_err(|_| bad("index must be a number"))?;
                    (&p[..k], Some(idx))
                }
                _ => (p, None),
            };
            let t = tag::try_parse(name).or_else(|| crate::dict::by_keyword(name).map(|e| e.tag)).ok_or_else(|| bad(&format!("unknown keyword or tag '{}'", name)))?;
            steps.push(Step { tag: t, index });
        }
        Ok(TagPath { steps })
    }

    /// Whether the path reaches into Pixel Data (so the parse must not stop before it).
    pub fn needs_pixel_data(&self) -> bool {
        self.steps.iter().any(|s| s.tag == tags::PIXEL_DATA)
    }

    /// Find the element the path names (an index on the last step selects a value, not an
    /// item, unless the element is a sequence).
    pub fn find<'a>(&self, ds: &'a DataSet) -> Option<(&'a Element, Option<usize>, CharacterSet)> {
        let mut cur = ds;
        let mut cs = ds.charset();
        for (i, st) in self.steps.iter().enumerate() {
            let el = cur.get(st.tag)?;
            let last = i + 1 == self.steps.len();
            if last {
                return Some((el, st.index, cs));
            }
            let items = el.items()?;
            let it = items.get(st.index.unwrap_or(0))?;
            cs = it.dataset.charset_or(&cs);
            cur = &it.dataset;
        }
        None
    }

    /// The value at the path: a string (values joined with `\`), the selected value with an
    /// index, the items (as keyword maps) for a sequence, the size for a bulk value; `Null` when
    /// absent.
    pub fn evaluate(&self, ds: &DataSet) -> Value {
        let Some((el, index, cs)) = self.find(ds) else { return Value::Null };
        if let Some(items) = el.items() {
            return match index {
                Some(i) => items.get(i).map(|it| crate::model::simplified(&it.dataset)).unwrap_or(Value::Null),
                None => Value::Array(items.iter().map(|it| crate::model::simplified(&it.dataset)).collect()),
            };
        }
        match el.raw() {
            Some(b) if el.vr.is_bulk() => Value::Bytes(b.to_vec()),
            Some(_) => {
                let vals = el.str_values(&cs);
                match index {
                    Some(i) => vals.get(i).cloned().map(Value::String).unwrap_or(Value::Null),
                    None => Value::String(vals.join("\\")),
                }
            }
            None => Value::Null,
        }
    }
}

/// The DICOM object in a message body: Part 10 bytes or a Value tree. File meta elements
/// (group 0002) are merged into the returned data set.
pub fn body_dataset(body: &Value, full: bool) -> Result<DataSet> {
    let f = match body {
        Value::Bytes(b) => DicomFile::parse(b, ReadOptions { stop_before_pixel_data: !full })?,
        Value::Object(_) => crate::model::value_to_file(body)?,
        Value::Null => return Err(CamelError::new("InvalidPayloadException", "No DICOM object in the message body (it is null)")),
        other => return Err(CamelError::new("InvalidPayloadException", format!("The message body is not DICOM (Part 10 bytes or a dicom Value tree), but {}", support_type(other)))),
    };
    let mut ds = f.dataset;
    for e in f.meta.iter() {
        ds.insert(e.clone());
    }
    Ok(ds)
}

fn support_type(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Int(_) | Value::Float(_) => "a number",
        Value::Bool(_) => "a boolean",
        _ => "something else",
    }
}

/// Evaluate a path against the body of an exchange.
pub fn evaluate_on_exchange(ex: &Exchange, path: &TagPath) -> Result<Value> {
    let ds = body_dataset(ex.body(), path.needs_pixel_data())?;
    Ok(path.evaluate(&ds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vr::Vr;

    #[test]
    fn paths() {
        let mut ds = DataSet::new();
        ds.insert(Element::string(tags::PATIENT_ID, Vr::LO, "P1"));
        ds.insert(Element::string(tags::IMAGE_TYPE, Vr::CS, "ORIGINAL\\PRIMARY"));
        let item = DataSet::from_elements(vec![Element::string(tags::SERIES_INSTANCE_UID, Vr::UI, "1.2.3")]);
        ds.insert(Element::sequence(0x0008_1115, vec![item]));
        let ev = |p: &str| TagPath::parse(p).unwrap().evaluate(&ds);
        assert_eq!(ev("PatientID"), Value::from("P1"));
        assert_eq!(ev("(0010,0020)"), Value::from("P1"));
        assert_eq!(ev("00100020"), Value::from("P1"));
        assert_eq!(ev("ImageType"), Value::from("ORIGINAL\\PRIMARY"));
        assert_eq!(ev("ImageType[1]"), Value::from("PRIMARY"));
        assert_eq!(ev("ReferencedSeriesSequence[0].SeriesInstanceUID"), Value::from("1.2.3"));
        assert_eq!(ev("(0008,1115)[0].(0020,000E)"), Value::from("1.2.3"));
        assert_eq!(ev("PatientName"), Value::Null);
        assert!(TagPath::parse("NoSuchKeyword").is_err());
    }
}
