//! The Value tree: a data set as a Cameleer [`Value`], in the shape of the DICOM JSON model
//! (PS3.18 Annex F).
//!
//! A data set is an object keyed by the eight-hex-digit tag (`"00100020"`); each element is an
//! object with `vr` and, when it has a value, one of:
//! - `Value`: an array of strings, numbers, person names (`{"Alphabetic": ..., "Ideographic":
//!   ..., "Phonetic": ...}`) or, for SQ, item objects (empty values are `null`);
//! - `InlineBinary`: the bytes of OB, OD, OF, OL, OV, OW and UN (a `Value::Bytes` in the tree,
//!   base64 text in JSON);
//! - `Fragments`: encapsulated pixel data, the Basic Offset Table item first (Cameleer
//!   extension).
//!
//! DS and IS values are numbers when the number prints back as the same text, and strings
//! otherwise (`"1.50"`), so nothing is lost. Keys that start with `_` are Cameleer extensions
//! that keep the encoding exact, and are left out of the JSON model:
//! - `_undefinedLength: true` on a sequence element or an item written with undefined length;
//! - `_raw`: the original value bytes, kept only when re-encoding the value would not give the
//!   same bytes (extra padding, unusual spacing); used again only while the value is unchanged;
//! - top level only: `_preamble` (the 128 bytes when not all zero, `null` for a file that had
//!   none) and `_transferSyntax` (for a bare data set without file meta information).
//!
//! The file meta information (group 0002) is part of the tree, so a Part 10 file read and
//! written again is byte-exact.

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

use crate::charset::CharacterSet;
use crate::codec::DicomFile;
use crate::object::{self, latin1_string, trim_value, DataSet, Element, Item};
use crate::tag::{self, tags};
use crate::vr::Vr;

fn err(msg: impl Into<String>) -> CamelError {
    CamelError::new("IllegalArgumentException", msg)
}

/// A Part 10 file (or bare data set) as a Value tree.
pub fn file_to_value(f: &DicomFile) -> Value {
    let mut m = ObjectMap::new();
    match &f.preamble {
        Some(p) if p.iter().any(|&b| b != 0) => {
            m.insert("_preamble".to_string(), Value::Bytes(p.clone()));
        }
        Some(_) => {}
        None if !f.meta.is_empty() => {
            m.insert("_preamble".to_string(), Value::Null);
        }
        None => {}
    }
    if f.meta.is_empty() {
        m.insert("_transferSyntax".to_string(), Value::String(f.transfer_syntax_uid()));
    }
    let cs = CharacterSet::default();
    for e in f.meta.iter() {
        m.insert(tag::format_hex(e.tag), element_to_value(e, &cs));
    }
    let dsv = dataset_to_value(&f.dataset, &cs);
    if let Value::Object(d) = dsv {
        for (k, v) in d {
            m.insert(k, v);
        }
    }
    Value::Object(m)
}

/// A Value tree back to a file: with group 0002 keys it is a Part 10 file, otherwise a bare
/// data set (written in `_transferSyntax`, default Explicit VR Little Endian, as a new Part 10
/// file when `_transferSyntax` is absent).
pub fn value_to_file(v: &Value) -> Result<DicomFile> {
    let Value::Object(m) = v else { return Err(err("A DICOM Value tree must be an object keyed by tag")) };
    let mut meta = DataSet::new();
    let mut rest = ObjectMap::new();
    for (k, val) in m.iter() {
        match tag::try_parse(k) {
            Some(t) if tag::group(t) == 0x0002 => {
                meta.insert(value_to_element(t, val, &CharacterSet::default())?);
            }
            _ => {
                rest.insert(k.clone(), val.clone());
            }
        }
    }
    let dataset = value_to_dataset(&Value::Object(rest), &CharacterSet::default())?;
    if meta.is_empty() {
        return Ok(match m.get("_transferSyntax").and_then(|v| v.as_str()) {
            Some(ts) => DicomFile { preamble: None, meta, dataset, bare_transfer_syntax: Some(ts.to_string()) },
            None => DicomFile::new(dataset, crate::ts::EXPLICIT_VR_LITTLE_ENDIAN),
        });
    }
    let preamble = match m.get("_preamble") {
        Some(Value::Null) => None,
        Some(Value::Bytes(b)) => Some(b.clone()),
        Some(Value::String(s)) => Some(cameleer_core::dataformat::base64::decode(s)?),
        _ => Some(vec![0; 128]),
    };
    Ok(DicomFile { preamble, meta, dataset, bare_transfer_syntax: None })
}

/// A data set as a Value tree (`inherited` is the character set of the enclosing data set).
pub fn dataset_to_value(ds: &DataSet, inherited: &CharacterSet) -> Value {
    let cs = ds.charset_or(inherited);
    let mut m = ObjectMap::new();
    for e in ds.iter() {
        m.insert(tag::format_hex(e.tag), element_to_value(e, &cs));
    }
    Value::Object(m)
}

/// A Value tree (an object keyed by tag or keyword) to a data set.
pub fn value_to_dataset(v: &Value, inherited: &CharacterSet) -> Result<DataSet> {
    let Value::Object(m) = v else { return Err(err("A DICOM data set must be an object keyed by tag")) };
    // the character set first: the other values are encoded in it
    let mut cs = inherited.clone();
    let mut ds = DataSet::new();
    for (k, val) in m.iter() {
        if let Some(t) = key_tag(k) {
            if t == tags::SPECIFIC_CHARACTER_SET {
                let e = value_to_element(t, val, inherited)?;
                cs = CharacterSet::from_value(&latin1_string(e.raw().unwrap_or(&[])));
                ds.insert(e);
            }
        }
    }
    for (k, val) in m.iter() {
        if k.starts_with('_') {
            continue;
        }
        let t = key_tag(k).ok_or_else(|| err(format!("Not a DICOM tag or keyword: {}", k)))?;
        if t == tags::SPECIFIC_CHARACTER_SET {
            continue;
        }
        ds.insert(value_to_element(t, val, &cs)?);
    }
    Ok(ds)
}

/// The tag for a key of the tree: eight hex digits, `(gggg,eeee)` or a dictionary keyword.
pub fn key_tag(k: &str) -> Option<u32> {
    if k.starts_with('_') {
        return None;
    }
    tag::try_parse(k).or_else(|| crate::dict::by_keyword(k).map(|e| e.tag))
}

/// One element as a Value (`{"vr": ..., "Value": [...]}`).
pub fn element_to_value(e: &Element, cs: &CharacterSet) -> Value {
    let mut m = ObjectMap::new();
    m.insert("vr".to_string(), Value::String(e.vr.as_str().to_string()));
    match &e.value {
        object::Value::Sequence { items, undefined_length } => {
            if *undefined_length {
                m.insert("_undefinedLength".to_string(), Value::Bool(true));
            }
            if !items.is_empty() {
                let arr = items
                    .iter()
                    .map(|it| {
                        let mut v = dataset_to_value(&it.dataset, cs);
                        if it.undefined_length {
                            if let Value::Object(o) = &mut v {
                                o.insert("_undefinedLength".to_string(), Value::Bool(true));
                            }
                        }
                        v
                    })
                    .collect();
                m.insert("Value".to_string(), Value::Array(arr));
            }
        }
        object::Value::Fragments(frags) => {
            m.insert("Fragments".to_string(), Value::Array(frags.iter().map(|f| Value::Bytes(f.clone())).collect()));
        }
        object::Value::Bytes(b) => {
            if b.is_empty() {
                return Value::Object(m);
            }
            let (key, val) = bytes_to_value(e.vr, b, cs);
            // keep the original bytes when re-encoding would differ
            let canonical = encode_value(e.tag, e.vr, &key, &val, cs).ok();
            m.insert(key.to_string(), val);
            if canonical.as_deref() != Some(b.as_slice()) {
                m.insert("_raw".to_string(), Value::Bytes(b.clone()));
            }
        }
    }
    Value::Object(m)
}

fn bytes_to_value(vr: Vr, b: &[u8], cs: &CharacterSet) -> (&'static str, Value) {
    if vr.is_bulk() {
        return ("InlineBinary", Value::Bytes(b.to_vec()));
    }
    let el = Element::bytes(0, vr, b.to_vec());
    let vals: Vec<Value> = match vr {
        Vr::US | Vr::SS | Vr::UL | Vr::SL | Vr::SV => el.ints().into_iter().map(Value::Int).collect(),
        Vr::UV => b
            .chunks_exact(8)
            .map(|c| {
                let u = u64::from_le_bytes(c.try_into().unwrap());
                if u <= i64::MAX as u64 {
                    Value::Int(u as i64)
                } else {
                    Value::String(u.to_string())
                }
            })
            .collect(),
        Vr::FL | Vr::FD => el.numbers_f64().into_iter().map(Value::Float).collect(),
        Vr::AT => el.tags().into_iter().map(|t| Value::String(tag::format_hex(t))).collect(),
        _ => {
            let text = if vr.uses_charset() { cs.decode(b, vr) } else { latin1_string(b) };
            let parts: Vec<&str> = if vr.is_multi_valued_string() { text.split('\\').collect() } else { vec![text.as_str()] };
            parts.into_iter().map(|p| string_component(vr, trim_value(p, vr))).collect()
        }
    };
    ("Value", Value::Array(vals))
}

fn string_component(vr: Vr, s: &str) -> Value {
    if s.is_empty() {
        return Value::Null;
    }
    match vr {
        Vr::PN => {
            let mut o = ObjectMap::new();
            for (i, g) in s.split('=').enumerate().take(3) {
                if !g.is_empty() {
                    o.insert(["Alphabetic", "Ideographic", "Phonetic"][i].to_string(), Value::String(g.to_string()));
                }
            }
            Value::Object(o)
        }
        Vr::IS => match s.parse::<i64>() {
            Ok(i) if i.to_string() == s => Value::Int(i),
            _ => Value::String(s.to_string()),
        },
        Vr::DS => match s.parse::<f64>() {
            Ok(f) if format_ds(f) == s => {
                if f.fract() == 0.0 && !s.contains(['.', 'e', 'E']) {
                    Value::Int(f as i64)
                } else {
                    Value::Float(f)
                }
            }
            _ => Value::String(s.to_string()),
        },
        _ => Value::String(s.to_string()),
    }
}

/// Format a number as a DS value (at most 16 characters).
pub fn format_ds(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{}", f);
    if s.len() <= 16 {
        return s;
    }
    for p in (1..=15).rev() {
        let e = format!("{:.*e}", p, f).replace("e", "E");
        if e.len() <= 16 {
            return e;
        }
    }
    format!("{:.0e}", f)
}

/// An element from its Value (`{"vr": ..., "Value": [...]}`, or a bare scalar or array, whose
/// VR then comes from the dictionary).
pub fn value_to_element(t: u32, v: &Value, cs: &CharacterSet) -> Result<Element> {
    let (m, bare);
    let obj = match v {
        Value::Object(o) if o.contains_key("vr") || o.contains_key("Value") || o.contains_key("InlineBinary") || o.contains_key("Fragments") || o.is_empty() => o,
        other => {
            // shorthand: "00100020": "PID" or ["A", "B"]
            bare = other.clone();
            let mut o = ObjectMap::new();
            match &bare {
                Value::Null => {}
                Value::Array(_) => {
                    o.insert("Value".to_string(), bare.clone());
                }
                Value::Bytes(_) => {
                    o.insert("InlineBinary".to_string(), bare.clone());
                }
                _ => {
                    o.insert("Value".to_string(), Value::Array(vec![bare.clone()]));
                }
            }
            m = o;
            &m
        }
    };
    let vr = match obj.get("vr").and_then(|v| v.as_str()) {
        Some(s) => Vr::parse(s).ok_or_else(|| err(format!("Unknown VR '{}' for {}", s, tag::format(t))))?,
        None => default_vr(t),
    };
    if let Some(frags) = obj.get("Fragments") {
        let Value::Array(a) = frags else { return Err(err("Fragments must be an array")) };
        let frags = a.iter().map(binary).collect::<Result<Vec<_>>>()?;
        return Ok(Element { tag: t, vr, value: object::Value::Fragments(frags) });
    }
    let undefined = matches!(obj.get("_undefinedLength"), Some(Value::Bool(true)));
    if vr == Vr::SQ || (vr == Vr::UN && undefined && matches!(obj.get("Value"), Some(Value::Array(_)))) {
        let items = match obj.get("Value") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|iv| {
                    let ds = value_to_dataset(iv, cs)?;
                    let u = matches!(iv, Value::Object(o) if matches!(o.get("_undefinedLength"), Some(Value::Bool(true))));
                    Ok(Item { dataset: ds, undefined_length: u })
                })
                .collect::<Result<Vec<_>>>()?,
            Some(Value::Null) | None => Vec::new(),
            Some(_) => return Err(err(format!("The Value of sequence {} must be an array of items", tag::format(t)))),
        };
        return Ok(Element { tag: t, vr, value: object::Value::Sequence { items, undefined_length: undefined } });
    }
    if obj.contains_key("BulkDataURI") {
        return Err(err(format!("{} has a BulkDataURI; retrieve the bulk data before marshalling", tag::format(t))));
    }
    let (key, val) = if let Some(b) = obj.get("InlineBinary") {
        ("InlineBinary", b.clone())
    } else if let Some(val) = obj.get("Value") {
        ("Value", val.clone())
    } else {
        return Ok(Element::bytes(t, vr, Vec::new()));
    };
    // the original bytes, while the value is unchanged
    if let Some(Value::Bytes(raw)) = obj.get("_raw") {
        let (_, orig) = bytes_to_value(vr, raw, cs);
        if values_same(&orig, &val) {
            return Ok(Element::bytes(t, vr, raw.clone()));
        }
    }
    Ok(Element::bytes(t, vr, encode_value(t, vr, key, &val, cs)?))
}

fn values_same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bytes(x), Value::String(s)) | (Value::String(s), Value::Bytes(x)) => cameleer_core::dataformat::base64::decode(s).map(|d| &d == x).unwrap_or(false),
        _ => a == b,
    }
}

/// The VR of an element without one: the dictionary's, LO for private creators, UN otherwise.
pub fn default_vr(t: u32) -> Vr {
    if tag::is_private_creator(t) {
        Vr::LO
    } else if tag::is_group_length(t) {
        Vr::UL
    } else {
        crate::dict::lookup(t).map(|e| e.vr).unwrap_or(Vr::UN)
    }
}

fn binary(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        Value::String(s) => cameleer_core::dataformat::base64::decode(s),
        Value::Null => Ok(Vec::new()),
        other => Err(err(format!("Expected binary data, got {}", other))),
    }
}

fn encode_value(t: u32, vr: Vr, key: &str, val: &Value, cs: &CharacterSet) -> Result<Vec<u8>> {
    if key == "InlineBinary" {
        let mut b = binary(val)?;
        if b.len() % 2 == 1 {
            b.push(0);
        }
        return Ok(b);
    }
    let vals: Vec<Value> = match val {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    };
    if vr.number_size() > 0 {
        let mut b = Vec::new();
        for v in &vals {
            let f = match (vr, v) {
                (Vr::AT, Value::String(s)) => tag::parse(s)? as f64,
                (Vr::UV, Value::String(s)) => {
                    let u: u64 = s.trim().parse().map_err(|_| err(format!("Not an unsigned number for {}: {}", tag::format(t), s)))?;
                    b.extend_from_slice(&u.to_le_bytes());
                    continue;
                }
                (_, Value::Int(i)) => *i as f64,
                (_, Value::Float(f)) => *f,
                (_, Value::String(s)) => s.trim().parse::<f64>().map_err(|_| err(format!("Not a number for {} ({}): {}", tag::format(t), vr, s)))?,
                (_, Value::Bool(x)) => *x as i64 as f64,
                (_, other) => return Err(err(format!("Not a number for {} ({}): {}", tag::format(t), vr, other))),
            };
            if matches!(vr, Vr::SV) {
                if let Value::Int(i) = v {
                    b.extend_from_slice(&i.to_le_bytes());
                    continue;
                }
            }
            object::push_number(&mut b, vr, f);
        }
        return Ok(b);
    }
    if !vr.is_string() {
        return Err(err(format!("{} ({}) needs InlineBinary, not Value", tag::format(t), vr)));
    }
    let parts: Vec<String> = vals.iter().map(|v| component_string(vr, v)).collect();
    let joined = parts.join("\\");
    let mut b = if vr.uses_charset() {
        cs.encode(&joined, vr)?
    } else {
        joined.chars().map(|c| if (c as u32) < 256 { Ok(c as u8) } else { Err(err(format!("Character '{}' is not allowed in {} ({})", c, tag::format(t), vr))) }).collect::<Result<Vec<u8>>>()?
    };
    if b.len() % 2 == 1 {
        b.push(vr.pad_byte());
    }
    Ok(b)
}

fn component_string(vr: Vr, v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if vr == Vr::DS => format_ds(*f),
        Value::Float(f) => format!("{}", f),
        Value::Object(o) if vr == Vr::PN => {
            let groups: Vec<String> = ["Alphabetic", "Ideographic", "Phonetic"].iter().map(|k| o.get(*k).map(|v| v.to_string()).unwrap_or_default()).collect();
            let mut n = 3;
            while n > 1 && groups[n - 1].is_empty() {
                n -= 1;
            }
            groups[..n].join("=")
        }
        other => other.to_string(),
    }
}

/// A short form of the tree for logs: keyword → value as text, sequences as arrays.
pub fn simplified(ds: &DataSet) -> Value {
    let cs = ds.charset();
    let mut m = ObjectMap::new();
    for e in ds.iter() {
        let key = crate::dict::lookup(e.tag).map(|d| d.keyword.to_string()).filter(|k| !k.is_empty()).unwrap_or_else(|| tag::format_hex(e.tag));
        let v = match &e.value {
            object::Value::Sequence { items, .. } => Value::Array(items.iter().map(|it| simplified(&it.dataset)).collect()),
            object::Value::Fragments(f) => Value::String(format!("<{} fragments>", f.len())),
            object::Value::Bytes(b) if e.vr.is_bulk() => Value::String(format!("<{} bytes>", b.len())),
            object::Value::Bytes(_) => Value::String(e.to_string_value(&cs)),
        };
        m.insert(key, v);
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::ReadOptions;

    fn sample() -> DataSet {
        let mut ds = DataSet::new();
        ds.insert(Element::string(tags::SOP_CLASS_UID, Vr::UI, "1.2.840.10008.5.1.4.1.1.7"));
        ds.insert(Element::string(tags::SOP_INSTANCE_UID, Vr::UI, "1.2.3.4.5"));
        ds.insert(Element::strings(tags::PATIENT_NAME, Vr::PN, &["Doe^Jane=ド^ジ"], &CharacterSet::from_value("ISO_IR 192")).unwrap());
        ds.insert(Element::bytes(tags::PATIENT_ID, Vr::LO, b"PID-7   ".to_vec())); // extra padding
        ds.insert(Element::string(tags::IMAGE_TYPE, Vr::CS, "ORIGINAL\\PRIMARY\\\\AXIAL"));
        ds.insert(Element::string(0x0018_0050, Vr::DS, "1.50"));
        ds.insert(Element::string(0x0028_0030, Vr::DS, "0.5\\0.5"));
        ds.insert(Element::string(0x0020_0013, Vr::IS, "7"));
        ds.insert(Element::numbers(0x0028_0010, Vr::US, &[512.0]));
        ds.insert(Element::numbers(0x0018_9087, Vr::FD, &[1000.25]));
        ds.insert(Element::numbers(0x0020_9165, Vr::AT, &[0x0010_0020 as f64]));
        ds.insert(Element::sequence(0x0008_1140, vec![DataSet::from_elements(vec![Element::string(tags::REFERENCED_SOP_INSTANCE_UID, Vr::UI, "9.8.7")])]));
        ds.insert(Element::bytes(tags::PIXEL_DATA, Vr::OW, vec![1, 0, 2, 0]));
        ds
    }

    #[test]
    fn tree_shape_follows_the_json_model() {
        let v = dataset_to_value(&sample(), &CharacterSet::from_value("ISO_IR 192"));
        let Value::Object(m) = &v else { panic!() };
        let pn = m.get("00100010").unwrap();
        assert_eq!(pn.get("vr").unwrap().as_str(), Some("PN"));
        let pnv = &pn.as_object().unwrap().get("Value").unwrap().as_array().unwrap()[0];
        assert_eq!(pnv.as_object().unwrap().get("Alphabetic").unwrap().as_str(), Some("Doe^Jane"));
        assert_eq!(pnv.as_object().unwrap().get("Ideographic").unwrap().as_str(), Some("ド^ジ"));
        let it = m.get("00080008").unwrap().as_object().unwrap().get("Value").unwrap().as_array().unwrap().clone();
        assert_eq!(it, vec![Value::from("ORIGINAL"), Value::from("PRIMARY"), Value::Null, Value::from("AXIAL")]);
        let ds = m.get("00180050").unwrap().as_object().unwrap().get("Value").unwrap().as_array().unwrap().clone();
        assert_eq!(ds, vec![Value::from("1.50")]);
        let ps = m.get("00280030").unwrap().as_object().unwrap().get("Value").unwrap().as_array().unwrap().clone();
        assert_eq!(ps, vec![Value::Float(0.5), Value::Float(0.5)]);
        assert!(m.get("00100020").unwrap().as_object().unwrap().contains_key("_raw"));
        assert!(!m.get("00280010").unwrap().as_object().unwrap().contains_key("_raw"));
        assert_eq!(m.get("00209165").unwrap().as_object().unwrap().get("Value").unwrap().as_array().unwrap()[0].as_str(), Some("00100020"));
    }

    #[test]
    fn tree_round_trip_is_byte_exact() {
        let mut ds = sample();
        ds.insert(Element::string(tags::SPECIFIC_CHARACTER_SET, Vr::CS, "ISO_IR 192"));
        let f = DicomFile::new(ds, crate::ts::EXPLICIT_VR_LITTLE_ENDIAN);
        let bytes = f.to_bytes().unwrap();
        let tree = file_to_value(&DicomFile::parse(&bytes, ReadOptions::default()).unwrap());
        let back = value_to_file(&tree).unwrap().to_bytes().unwrap();
        assert_eq!(back, bytes);
        // a changed value replaces _raw
        let mut tree = tree;
        if let Value::Object(m) = &mut tree {
            if let Some(Value::Object(e)) = m.get_mut("00100020") {
                e.insert("Value".into(), Value::Array(vec![Value::from("NEW")]));
            }
        }
        let f2 = value_to_file(&tree).unwrap();
        assert_eq!(f2.dataset.get(tags::PATIENT_ID).unwrap().raw().unwrap(), b"NEW ");
    }

    #[test]
    fn shorthand_keys_and_values() {
        let mut m = ObjectMap::new();
        m.insert("00100020".into(), Value::from("X1"));
        m.insert("00280010".into(), Value::Int(256));
        let ds = value_to_dataset(&Value::Object(m), &CharacterSet::default()).unwrap();
        assert_eq!(ds.string(tags::PATIENT_ID).unwrap(), "X1");
        assert_eq!(format_ds(0.1), "0.1");
        assert_eq!(format_ds(3.0), "3");
        assert!(format_ds(1.0 / 3.0).len() <= 16);
    }
}
