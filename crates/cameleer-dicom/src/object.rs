//! The in-memory data set: elements in tag order, sequences of items, encapsulated
//! fragments.
//!
//! Values keep the bytes as they were encoded (strings in their character set, padding
//! included; binary numbers little endian), so a data set read and written again in the same
//! transfer syntax is byte-exact.

use cameleer_core::error::{CamelError, Result};

use crate::charset::CharacterSet;
use crate::tag::{self, tags};
use crate::vr::Vr;

/// One data element.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub tag: u32,
    pub vr: Vr,
    pub value: Value,
}

/// The value of an element.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The encoded value bytes (little endian for binary VRs), padding included.
    Bytes(Vec<u8>),
    /// A sequence (SQ, or UN of undefined length, whose items are implicit VR little endian).
    Sequence { items: Vec<Item>, undefined_length: bool },
    /// Encapsulated pixel data: the Basic Offset Table item first (possibly empty), then the
    /// fragments, each kept as opaque bytes.
    Fragments(Vec<Vec<u8>>),
}

/// A sequence item.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Item {
    pub dataset: DataSet,
    pub undefined_length: bool,
}

impl Item {
    /// An item with an explicit length.
    pub fn new(dataset: DataSet) -> Item {
        Item { dataset, undefined_length: false }
    }
}

impl Element {
    /// An element with the given value bytes (no padding is added).
    pub fn bytes(tag: u32, vr: Vr, bytes: Vec<u8>) -> Element {
        Element { tag, vr, value: Value::Bytes(bytes) }
    }

    /// A string element: the values joined with `\`, encoded in `cs` where the VR uses the
    /// character set, and padded to an even length.
    pub fn strings(tag: u32, vr: Vr, values: &[&str], cs: &CharacterSet) -> Result<Element> {
        let joined = values.join("\\");
        let mut b = if vr.uses_charset() { cs.encode(&joined, vr)? } else { latin1_bytes(&joined, vr)? };
        if b.len() % 2 == 1 {
            b.push(vr.pad_byte());
        }
        Ok(Element::bytes(tag, vr, b))
    }

    /// A string element in the default character set.
    pub fn string(tag: u32, vr: Vr, value: &str) -> Element {
        Element::strings(tag, vr, &[value], &CharacterSet::default()).unwrap_or_else(|_| Element::bytes(tag, vr, Vec::new()))
    }

    /// A binary number element (US, SS, UL, SL, SV, UV, FL, FD, AT) from integers or floats.
    pub fn numbers(tag: u32, vr: Vr, values: &[f64]) -> Element {
        let mut b = Vec::with_capacity(values.len() * vr.number_size());
        for &v in values {
            push_number(&mut b, vr, v);
        }
        Element::bytes(tag, vr, b)
    }

    /// A US element.
    pub fn us(tag: u32, v: u16) -> Element {
        Element::bytes(tag, Vr::US, v.to_le_bytes().to_vec())
    }

    /// A UL element.
    pub fn ul(tag: u32, v: u32) -> Element {
        Element::bytes(tag, Vr::UL, v.to_le_bytes().to_vec())
    }

    /// A sequence element of items with explicit lengths.
    pub fn sequence(tag: u32, items: Vec<DataSet>) -> Element {
        Element { tag, vr: Vr::SQ, value: Value::Sequence { items: items.into_iter().map(Item::new).collect(), undefined_length: false } }
    }

    /// The raw value bytes (`None` for sequences and fragments).
    pub fn raw(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The items of a sequence.
    pub fn items(&self) -> Option<&[Item]> {
        match &self.value {
            Value::Sequence { items, .. } => Some(items),
            _ => None,
        }
    }

    /// The items of a sequence, mutable.
    pub fn items_mut(&mut self) -> Option<&mut Vec<Item>> {
        match &mut self.value {
            Value::Sequence { items, .. } => Some(items),
            _ => None,
        }
    }

    /// The string values (split at `\` for multi-valued VRs), decoded with `cs`, with the
    /// padding and the insignificant spaces removed (PS3.5 6.2: trailing spaces for every text
    /// VR, leading ones too for the VRs where they are insignificant).
    pub fn str_values(&self, cs: &CharacterSet) -> Vec<String> {
        let Some(b) = self.raw() else { return Vec::new() };
        if !self.vr.is_string() {
            return self.number_strings();
        }
        let text = if self.vr.uses_charset() { cs.decode(b, self.vr) } else { latin1_string(b) };
        let parts: Vec<&str> = if self.vr.is_multi_valued_string() { text.split('\\').collect() } else { vec![text.as_str()] };
        parts.into_iter().map(|p| trim_value(p, self.vr).to_string()).collect()
    }

    /// The value as one string: the values joined with `\` (numbers in decimal).
    pub fn to_string_value(&self, cs: &CharacterSet) -> String {
        self.str_values(cs).join("\\")
    }

    /// The binary numbers as `f64` (US, SS, UL, SL, SV, UV, FL, FD), or the parsed numbers of
    /// an IS or DS value.
    pub fn numbers_f64(&self) -> Vec<f64> {
        let Some(b) = self.raw() else { return Vec::new() };
        match self.vr {
            Vr::IS | Vr::DS => latin1_string(b).split('\\').filter_map(|s| s.trim().parse::<f64>().ok()).collect(),
            vr if vr.number_size() > 0 && vr != Vr::AT => b.chunks_exact(vr.number_size()).map(|c| read_number(c, vr)).collect(),
            _ => Vec::new(),
        }
    }

    /// The integer values (binary integer VRs, or IS).
    pub fn ints(&self) -> Vec<i64> {
        let Some(b) = self.raw() else { return Vec::new() };
        match self.vr {
            Vr::US => b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as i64).collect(),
            Vr::SS => b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as i64).collect(),
            Vr::UL => b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i64).collect(),
            Vr::SL => b.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i64).collect(),
            Vr::SV => b.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect(),
            Vr::UV => b.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap()) as i64).collect(),
            Vr::IS | Vr::DS => latin1_string(b).split('\\').filter_map(|s| s.trim().parse::<f64>().ok()).map(|f| f as i64).collect(),
            _ => Vec::new(),
        }
    }

    /// AT values as tags.
    pub fn tags(&self) -> Vec<u32> {
        match (self.vr, self.raw()) {
            (Vr::AT, Some(b)) => b.chunks_exact(4).map(|c| tag::tag(u16::from_le_bytes([c[0], c[1]]), u16::from_le_bytes([c[2], c[3]]))).collect(),
            _ => Vec::new(),
        }
    }

    fn number_strings(&self) -> Vec<String> {
        match self.vr {
            Vr::AT => self.tags().into_iter().map(tag::format_hex).collect(),
            Vr::FL | Vr::FD => self.numbers_f64().into_iter().map(format_float).collect(),
            Vr::US | Vr::SS | Vr::UL | Vr::SL | Vr::SV => self.ints().into_iter().map(|i| i.to_string()).collect(),
            Vr::UV => self.raw().unwrap_or(&[]).chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap()).to_string()).collect(),
            _ => Vec::new(),
        }
    }
}

/// Format a float the way DICOM JSON and the string accessors show it (integers without a
/// fraction).
pub fn format_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", f)
    }
}

pub(crate) fn read_number(c: &[u8], vr: Vr) -> f64 {
    match vr {
        Vr::US => u16::from_le_bytes([c[0], c[1]]) as f64,
        Vr::SS => i16::from_le_bytes([c[0], c[1]]) as f64,
        Vr::UL => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
        Vr::SL => i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
        Vr::FL => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
        Vr::FD => f64::from_le_bytes(c.try_into().unwrap()),
        Vr::SV => i64::from_le_bytes(c.try_into().unwrap()) as f64,
        Vr::UV => u64::from_le_bytes(c.try_into().unwrap()) as f64,
        _ => 0.0,
    }
}

pub(crate) fn push_number(b: &mut Vec<u8>, vr: Vr, v: f64) {
    match vr {
        Vr::US => b.extend_from_slice(&(v as u16).to_le_bytes()),
        Vr::SS => b.extend_from_slice(&(v as i16).to_le_bytes()),
        Vr::UL => b.extend_from_slice(&(v as u32).to_le_bytes()),
        Vr::SL => b.extend_from_slice(&(v as i32).to_le_bytes()),
        Vr::FL => b.extend_from_slice(&(v as f32).to_le_bytes()),
        Vr::FD => b.extend_from_slice(&v.to_le_bytes()),
        Vr::SV => b.extend_from_slice(&(v as i64).to_le_bytes()),
        Vr::UV => b.extend_from_slice(&(v as u64).to_le_bytes()),
        Vr::AT => {
            let t = v as u32;
            b.extend_from_slice(&tag::group(t).to_le_bytes());
            b.extend_from_slice(&tag::element(t).to_le_bytes());
        }
        _ => {}
    }
}

/// Remove padding and insignificant spaces from one string value (PS3.5 Table 6.2-1).
pub fn trim_value(s: &str, vr: Vr) -> &str {
    match vr {
        Vr::UI => s.trim_end_matches(['\0', ' ']),
        // leading spaces are significant in these
        Vr::LT | Vr::ST | Vr::UT | Vr::UR => s.trim_end_matches([' ', '\0']),
        _ => s.trim_matches([' ', '\0']),
    }
}

/// Bytes to a string, one char per byte (ISO 8859-1), for the VRs outside the character set.
pub fn latin1_string(b: &[u8]) -> String {
    if b.is_ascii() {
        // SAFETY-free fast path: ASCII is valid UTF-8
        return String::from_utf8(b.to_vec()).unwrap_or_default();
    }
    b.iter().map(|&c| c as char).collect()
}

fn latin1_bytes(s: &str, vr: Vr) -> Result<Vec<u8>> {
    s.chars()
        .map(|c| if (c as u32) < 256 { Ok(c as u8) } else { Err(CamelError::new("IllegalArgumentException", format!("Character '{}' is not allowed in a {} value", c, vr))) })
        .collect()
}

/// A data set: elements kept in ascending tag order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DataSet {
    elements: Vec<Element>,
}

impl DataSet {
    /// An empty data set.
    pub fn new() -> DataSet {
        DataSet::default()
    }

    /// Build from elements in any order (later duplicates replace earlier ones).
    pub fn from_elements(elements: Vec<Element>) -> DataSet {
        let mut ds = DataSet::new();
        for e in elements {
            ds.insert(e);
        }
        ds
    }

    /// Append an element known to come after the last one (the parser's fast path); falls back
    /// to a sorted insert.
    pub fn push(&mut self, e: Element) {
        match self.elements.last() {
            Some(last) if last.tag >= e.tag => {
                self.insert(e);
            }
            _ => self.elements.push(e),
        }
    }

    /// Insert or replace an element, keeping tag order.
    pub fn insert(&mut self, e: Element) -> Option<Element> {
        match self.elements.binary_search_by_key(&e.tag, |x| x.tag) {
            Ok(i) => Some(std::mem::replace(&mut self.elements[i], e)),
            Err(i) => {
                self.elements.insert(i, e);
                None
            }
        }
    }

    /// Remove an element.
    pub fn remove(&mut self, tag: u32) -> Option<Element> {
        self.elements.binary_search_by_key(&tag, |x| x.tag).ok().map(|i| self.elements.remove(i))
    }

    /// Keep only the elements for which `f` returns true.
    pub fn retain(&mut self, f: impl FnMut(&Element) -> bool) {
        self.elements.retain(f)
    }

    /// The element with a tag.
    pub fn get(&self, tag: u32) -> Option<&Element> {
        self.elements.binary_search_by_key(&tag, |x| x.tag).ok().map(|i| &self.elements[i])
    }

    /// The element with a tag, mutable.
    pub fn get_mut(&mut self, tag: u32) -> Option<&mut Element> {
        match self.elements.binary_search_by_key(&tag, |x| x.tag) {
            Ok(i) => Some(&mut self.elements[i]),
            Err(_) => None,
        }
    }

    /// Whether the data set has the tag.
    pub fn contains(&self, tag: u32) -> bool {
        self.get(tag).is_some()
    }

    /// The elements in tag order.
    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    /// The elements, mutable (keep the tags unchanged or the order breaks).
    pub fn elements_mut(&mut self) -> &mut [Element] {
        &mut self.elements
    }

    /// Iterate over the elements in tag order.
    pub fn iter(&self) -> std::slice::Iter<'_, Element> {
        self.elements.iter()
    }

    /// Number of elements (not counting nested ones).
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Whether there are no elements.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The character set named by (0008,0005), or the default repertoire.
    pub fn charset(&self) -> CharacterSet {
        self.charset_or(&CharacterSet::default())
    }

    /// The character set named by (0008,0005), or `inherited` (for items of a sequence).
    pub fn charset_or(&self, inherited: &CharacterSet) -> CharacterSet {
        match self.get(tags::SPECIFIC_CHARACTER_SET).and_then(|e| e.raw()) {
            Some(b) => CharacterSet::from_value(&latin1_string(b)),
            None => inherited.clone(),
        }
    }

    /// The value of a string (or number) element as one string, decoded with this data set's
    /// character set; `None` when absent.
    pub fn string(&self, tag: u32) -> Option<String> {
        self.get(tag).map(|e| e.to_string_value(&self.charset()))
    }

    /// The first integer value of an element.
    pub fn int(&self, tag: u32) -> Option<i64> {
        self.get(tag).and_then(|e| e.ints().first().copied())
    }

    /// Set a string element using this data set's character set (the VR from the dictionary
    /// when `vr` is `None`).
    pub fn put_string(&mut self, tag: u32, vr: Option<Vr>, value: &str) -> Result<()> {
        let vr = match vr {
            Some(v) => v,
            None => crate::dict::lookup(tag).map(|e| e.vr).unwrap_or(Vr::LO),
        };
        let cs = self.charset();
        let values: Vec<&str> = if vr.is_multi_valued_string() { value.split('\\').collect() } else { vec![value] };
        self.insert(Element::strings(tag, vr, &values, &cs)?);
        Ok(())
    }

    /// Set an element to zero length, keeping its VR (the dictionary VR when absent).
    pub fn put_empty(&mut self, tag: u32) {
        let vr = self.get(tag).map(|e| e.vr).or_else(|| crate::dict::lookup(tag).map(|e| e.vr)).unwrap_or(Vr::UN);
        let value = if vr == Vr::SQ { Value::Sequence { items: Vec::new(), undefined_length: false } } else { Value::Bytes(Vec::new()) };
        self.insert(Element { tag, vr, value });
    }

    /// The SOP Class UID (0008,0016).
    pub fn sop_class_uid(&self) -> Option<String> {
        self.string(tags::SOP_CLASS_UID).filter(|s| !s.is_empty())
    }

    /// The SOP Instance UID (0008,0018).
    pub fn sop_instance_uid(&self) -> Option<String> {
        self.string(tags::SOP_INSTANCE_UID).filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_keeps_order_and_accessors_trim() {
        let mut ds = DataSet::new();
        ds.insert(Element::string(tags::PATIENT_ID, Vr::LO, "ID1"));
        ds.insert(Element::string(tags::PATIENT_NAME, Vr::PN, "Doe^John"));
        ds.insert(Element::bytes(tags::SOP_INSTANCE_UID, Vr::UI, b"1.2.3\0".to_vec()));
        assert_eq!(ds.elements().iter().map(|e| e.tag).collect::<Vec<_>>(), vec![tags::SOP_INSTANCE_UID, tags::PATIENT_NAME, tags::PATIENT_ID]);
        assert_eq!(ds.get(tags::PATIENT_ID).unwrap().raw().unwrap(), b"ID1 ");
        assert_eq!(ds.string(tags::PATIENT_ID).unwrap(), "ID1");
        assert_eq!(ds.sop_instance_uid().unwrap(), "1.2.3");
        ds.insert(Element::numbers(0x0028_0010, Vr::US, &[512.0]));
        assert_eq!(ds.int(0x0028_0010), Some(512));
        assert_eq!(ds.string(0x0028_0010).unwrap(), "512");
        ds.insert(Element::string(0x0018_0050, Vr::DS, "1.25\\2"));
        assert_eq!(ds.get(0x0018_0050).unwrap().numbers_f64(), vec![1.25, 2.0]);
        assert!(ds.remove(tags::PATIENT_ID).is_some() && !ds.contains(tags::PATIENT_ID));
    }
}
