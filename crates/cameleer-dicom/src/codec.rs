//! Reading and writing data sets and Part 10 files (PS3.5 section 7, PS3.10 section 7).

use cameleer_core::error::{CamelError, Result};

use crate::object::{DataSet, Element, Item, Value};
use crate::tag::{self, tags};
use crate::ts::{self, Encoding, TransferSyntax};
use crate::vr::Vr;

const UNDEFINED: u32 = 0xFFFF_FFFF;

/// Exception kind for malformed DICOM input.
pub const DICOM_PARSE_EXCEPTION: &str = "DicomParseException";

fn perr(msg: impl Into<String>) -> CamelError {
    CamelError::new(DICOM_PARSE_EXCEPTION, msg)
}

/// What to read.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOptions {
    /// Stop at Pixel Data (7FE0,0010) of the top-level data set and leave it out.
    pub stop_before_pixel_data: bool,
}

/// The VR of an element in an implicit VR data set: from the dictionary, LO for private
/// creators, UL for group lengths, UN otherwise. Values that PS3.6 lists as `OB or OW` are OW
/// in implicit VR little endian (PS3.5 A.1).
pub fn implicit_vr(t: u32) -> Vr {
    if tag::is_group_length(t) {
        return Vr::UL;
    }
    if tag::is_private_creator(t) {
        return Vr::LO;
    }
    if tag::is_private(t) {
        return Vr::UN;
    }
    match crate::dict::lookup(t) {
        Some(e) if e.vr == Vr::OB && e.alt_vr == Some(Vr::OW) => Vr::OW,
        Some(e) => e.vr,
        None => Vr::UN,
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    opts: ReadOptions,
}

impl<'a> Reader<'a> {
    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.buf.len() {
            Err(perr(format!("Unexpected end of data at offset {} (needed {} more bytes)", self.pos, n)))
        } else {
            Ok(())
        }
    }

    fn u16(&mut self, be: bool) -> Result<u16> {
        self.need(2)?;
        let b = [self.buf[self.pos], self.buf[self.pos + 1]];
        self.pos += 2;
        Ok(if be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32(&mut self, be: bool) -> Result<u32> {
        self.need(4)?;
        let b = [self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3]];
        self.pos += 4;
        Ok(if be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn tag(&mut self, enc: Encoding) -> Result<u32> {
        let g = self.u16(enc.big_endian)?;
        let e = self.u16(enc.big_endian)?;
        Ok(tag::tag(g, e))
    }

    /// Read elements until `end` (an offset), the end of the buffer (`None`), or an item
    /// delimitation item when `in_undefined_item`.
    fn dataset(&mut self, enc: Encoding, end: Option<usize>, in_undefined_item: bool, top: bool) -> Result<DataSet> {
        let mut ds = DataSet::new();
        loop {
            let limit = end.unwrap_or(self.buf.len());
            if self.pos >= limit {
                if in_undefined_item && end.is_none() {
                    return Err(perr("Missing item delimitation item"));
                }
                break;
            }
            // tolerate trailing padding shorter than a header
            if end.is_none() && !in_undefined_item && self.buf.len() - self.pos < 8 && self.buf[self.pos..].iter().all(|&b| b == 0) {
                self.pos = self.buf.len();
                break;
            }
            let start = self.pos;
            let t = self.tag(enc)?;
            if t == tags::ITEM_DELIMITATION {
                self.u32(enc.big_endian)?;
                if in_undefined_item {
                    return Ok(ds);
                }
                continue; // stray delimiter: skip
            }
            if t == tags::SEQUENCE_DELIMITATION {
                // a stray sequence delimiter at the end of a data set (seen in the wild)
                self.u32(enc.big_endian)?;
                continue;
            }
            if top && self.opts.stop_before_pixel_data && t == tags::PIXEL_DATA {
                self.pos = start;
                break;
            }
            let el = self.element(t, enc)?;
            ds.push(el);
        }
        Ok(ds)
    }

    fn element(&mut self, t: u32, enc: Encoding) -> Result<Element> {
        let (vr, len) = if enc.explicit_vr {
            self.need(2)?;
            let vb = [self.buf[self.pos], self.buf[self.pos + 1]];
            match Vr::from_bytes(&vb) {
                Some(vr) => {
                    self.pos += 2;
                    if vr.has_long_length() {
                        self.take(2)?;
                        (vr, self.u32(enc.big_endian)?)
                    } else {
                        (vr, self.u16(enc.big_endian)? as u32)
                    }
                }
                // not a VR: a writer that switched to implicit VR (tolerated, as most readers do)
                None => (implicit_vr(t), self.u32(enc.big_endian)?),
            }
        } else {
            (implicit_vr(t), self.u32(enc.big_endian)?)
        };
        if len == UNDEFINED {
            if t == tags::PIXEL_DATA && vr != Vr::SQ {
                return Ok(Element { tag: t, vr, value: Value::Fragments(self.fragments(enc)?) });
            }
            // SQ, or UN / unknown of undefined length: a sequence whose items are implicit VR
            // little endian for UN (PS3.5 6.2.2)
            let item_enc = if vr == Vr::SQ { enc } else { Encoding::IMPLICIT_LE };
            let items = self.items(item_enc, None)?;
            return Ok(Element { tag: t, vr, value: Value::Sequence { items, undefined_length: true } });
        }
        let len = len as usize;
        if vr == Vr::SQ {
            let end = self.pos + len;
            self.need(len)?;
            let items = self.items(enc, Some(end))?;
            self.pos = end;
            return Ok(Element { tag: t, vr, value: Value::Sequence { items, undefined_length: false } });
        }
        let raw = self.take(len)?;
        let mut bytes = raw.to_vec();
        if enc.big_endian {
            swap_in_place(&mut bytes, vr.swap_size());
        }
        Ok(Element { tag: t, vr, value: Value::Bytes(bytes) })
    }

    fn items(&mut self, enc: Encoding, end: Option<usize>) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        loop {
            if let Some(e) = end {
                if self.pos >= e {
                    break;
                }
            }
            let t = self.tag(enc)?;
            let len = self.u32(enc.big_endian)?;
            match t {
                tags::SEQUENCE_DELIMITATION => {
                    if end.is_none() {
                        break;
                    }
                }
                tags::ITEM => {
                    if len == UNDEFINED {
                        let ds = self.dataset(enc, None, true, false)?;
                        items.push(Item { dataset: ds, undefined_length: true });
                    } else {
                        let item_end = self.pos + len as usize;
                        self.need(len as usize)?;
                        let ds = self.dataset(enc, Some(item_end), false, false)?;
                        self.pos = item_end;
                        items.push(Item { dataset: ds, undefined_length: false });
                    }
                }
                other => return Err(perr(format!("Expected an item tag in a sequence, found {} at offset {}", tag::format(other), self.pos - 8))),
            }
        }
        Ok(items)
    }

    fn fragments(&mut self, enc: Encoding) -> Result<Vec<Vec<u8>>> {
        let mut frags = Vec::new();
        loop {
            let t = self.tag(enc)?;
            let len = self.u32(enc.big_endian)?;
            match t {
                tags::SEQUENCE_DELIMITATION => break,
                tags::ITEM => {
                    if len == UNDEFINED {
                        return Err(perr("Encapsulated pixel data fragment of undefined length"));
                    }
                    frags.push(self.take(len as usize)?.to_vec());
                }
                other => return Err(perr(format!("Expected a fragment item in encapsulated pixel data, found {}", tag::format(other)))),
            }
        }
        Ok(frags)
    }
}

fn swap_in_place(b: &mut [u8], size: usize) {
    if size > 1 {
        for c in b.chunks_exact_mut(size) {
            c.reverse();
        }
    }
}

/// Read a data set encoded with `encoding` (no file meta information, no preamble).
pub fn read_dataset(bytes: &[u8], encoding: Encoding, opts: ReadOptions) -> Result<DataSet> {
    let mut r = Reader { buf: bytes, pos: 0, opts };
    r.dataset(encoding, None, false, true)
}

/// Read a data set encoded in a transfer syntax (inflating a deflated one).
pub fn read_dataset_ts(bytes: &[u8], ts: &TransferSyntax, opts: ReadOptions) -> Result<DataSet> {
    if ts.deflated {
        let inflated = inflate(bytes)?;
        read_dataset(&inflated, ts.encoding, opts)
    } else {
        read_dataset(bytes, ts.encoding, opts)
    }
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    cameleer_core::dataformat::compress::inflate(bytes).map_err(|e| perr(format!("Cannot inflate the deflated data set: {}", e.message)))
}

// ---------------------------------------------------------------------------------------
// Writing

fn put_u16(out: &mut Vec<u8>, v: u16, be: bool) {
    out.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put_u32(out: &mut Vec<u8>, v: u32, be: bool) {
    out.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put_tag(out: &mut Vec<u8>, t: u32, be: bool) {
    put_u16(out, tag::group(t), be);
    put_u16(out, tag::element(t), be);
}

fn put_header(out: &mut Vec<u8>, t: u32, vr: Vr, len: u32, enc: Encoding) -> Result<()> {
    put_tag(out, t, enc.big_endian);
    if enc.explicit_vr {
        out.extend_from_slice(vr.as_str().as_bytes());
        if vr.has_long_length() {
            out.extend_from_slice(&[0, 0]);
            put_u32(out, len, enc.big_endian);
        } else {
            if len > 0xFFFF {
                return Err(CamelError::new("IllegalArgumentException", format!("Value of {} ({}) is {} bytes, more than a {} value can hold in explicit VR", tag::format(t), vr, len, vr)));
            }
            put_u16(out, len as u16, enc.big_endian);
        }
    } else {
        put_u32(out, len, enc.big_endian);
    }
    Ok(())
}

/// Write a data set in an encoding. The Command Group Length (0000,0000) and File Meta
/// Information Group Length (0002,0000) are recomputed when present.
pub fn write_dataset(out: &mut Vec<u8>, ds: &DataSet, enc: Encoding) -> Result<()> {
    let els = ds.elements();
    let mut i = 0;
    while i < els.len() {
        let el = &els[i];
        // the command and file meta group lengths are recomputed; the retired ones of other
        // groups are written as they were (byte-exact; readers ignore them)
        if el.tag == tags::COMMAND_GROUP_LENGTH || el.tag == tags::FILE_META_GROUP_LENGTH {
            let g = tag::group(el.tag);
            let mut body = Vec::new();
            let mut j = i + 1;
            while j < els.len() && tag::group(els[j].tag) == g {
                write_element(&mut body, &els[j], enc)?;
                j += 1;
            }
            let gl = Element::bytes(el.tag, Vr::UL, (body.len() as u32).to_le_bytes().to_vec());
            write_element(out, &gl, enc)?;
            out.extend_from_slice(&body);
            i = j;
        } else {
            write_element(out, el, enc)?;
            i += 1;
        }
    }
    Ok(())
}

/// Write one element.
pub fn write_element(out: &mut Vec<u8>, el: &Element, enc: Encoding) -> Result<()> {
    match &el.value {
        Value::Bytes(b) => {
            put_header(out, el.tag, el.vr, b.len() as u32, enc)?;
            if enc.big_endian && el.vr.swap_size() > 1 {
                let mut c = b.clone();
                swap_in_place(&mut c, el.vr.swap_size());
                out.extend_from_slice(&c);
            } else {
                out.extend_from_slice(b);
            }
        }
        Value::Sequence { items, undefined_length } => {
            // UN sequences carry implicit VR little endian items
            let item_enc = if el.vr == Vr::SQ { enc } else { Encoding::IMPLICIT_LE };
            if *undefined_length || el.vr != Vr::SQ {
                put_header(out, el.tag, el.vr, UNDEFINED, enc)?;
                for it in items {
                    write_item(out, it, item_enc, enc.big_endian)?;
                }
                put_tag(out, tags::SEQUENCE_DELIMITATION, enc.big_endian);
                put_u32(out, 0, enc.big_endian);
            } else {
                let mut body = Vec::new();
                for it in items {
                    write_item(&mut body, it, item_enc, enc.big_endian)?;
                }
                put_header(out, el.tag, el.vr, body.len() as u32, enc)?;
                out.extend_from_slice(&body);
            }
        }
        Value::Fragments(frags) => {
            put_header(out, el.tag, el.vr, UNDEFINED, enc)?;
            for f in frags {
                put_tag(out, tags::ITEM, enc.big_endian);
                put_u32(out, f.len() as u32, enc.big_endian);
                out.extend_from_slice(f);
            }
            put_tag(out, tags::SEQUENCE_DELIMITATION, enc.big_endian);
            put_u32(out, 0, enc.big_endian);
        }
    }
    Ok(())
}

fn write_item(out: &mut Vec<u8>, it: &Item, enc: Encoding, outer_be: bool) -> Result<()> {
    let be = outer_be && enc.big_endian;
    if it.undefined_length {
        put_tag(out, tags::ITEM, be);
        put_u32(out, UNDEFINED, be);
        write_dataset(out, &it.dataset, enc)?;
        put_tag(out, tags::ITEM_DELIMITATION, be);
        put_u32(out, 0, be);
    } else {
        let mut body = Vec::new();
        write_dataset(&mut body, &it.dataset, enc)?;
        put_tag(out, tags::ITEM, be);
        put_u32(out, body.len() as u32, be);
        out.extend_from_slice(&body);
    }
    Ok(())
}

/// Encode a data set in a transfer syntax (deflating a deflated one).
pub fn dataset_to_bytes(ds: &DataSet, ts: &TransferSyntax) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_dataset(&mut out, ds, ts.encoding)?;
    if ts.deflated {
        out = cameleer_core::dataformat::compress::deflate(&out, 6);
        // an odd-length deflated bitstream gets a trailing zero byte (PS3.5 A.5)
        if out.len() % 2 == 1 {
            out.push(0);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Part 10

/// The Implementation Class UID written in the file meta information of new files.
/// A UUID-derived UID (ISO/IEC 9834-8, root `2.25`), fixed for Cameleer.
pub const IMPLEMENTATION_CLASS_UID: &str = "2.25.201273755793964741390560873578799652301";
/// The Implementation Version Name written with it.
pub const IMPLEMENTATION_VERSION_NAME: &str = concat!("CAMELEER_", env!("CARGO_PKG_VERSION_MAJOR"), env!("CARGO_PKG_VERSION_MINOR"), env!("CARGO_PKG_VERSION_PATCH"));

/// A DICOM file: the 128-byte preamble, the file meta information (group 0002) and the data
/// set.
#[derive(Clone, Debug, PartialEq)]
pub struct DicomFile {
    /// The preamble; `None` when the input had none (a bare `DICM` prefix or no meta at all).
    pub preamble: Option<Vec<u8>>,
    /// The file meta information; empty for a bare data set.
    pub meta: DataSet,
    pub dataset: DataSet,
    /// The transfer syntax of a bare data set without meta information (guessed when read).
    pub bare_transfer_syntax: Option<String>,
}

impl DicomFile {
    /// A new Part 10 file for a data set: the meta information is built from its SOP Class
    /// and Instance UIDs and the transfer syntax.
    pub fn new(dataset: DataSet, transfer_syntax: &str) -> DicomFile {
        let mut meta = DataSet::new();
        meta.insert(Element::ul(tags::FILE_META_GROUP_LENGTH, 0));
        meta.insert(Element::bytes(tags::FILE_META_VERSION, Vr::OB, vec![0, 1]));
        let uid_el = |t, v: &str| {
            let mut b = v.as_bytes().to_vec();
            if b.len() % 2 == 1 {
                b.push(0);
            }
            Element::bytes(t, Vr::UI, b)
        };
        meta.insert(uid_el(tags::MEDIA_STORAGE_SOP_CLASS_UID, &dataset.sop_class_uid().unwrap_or_default()));
        meta.insert(uid_el(tags::MEDIA_STORAGE_SOP_INSTANCE_UID, &dataset.sop_instance_uid().unwrap_or_default()));
        meta.insert(uid_el(tags::TRANSFER_SYNTAX_UID, transfer_syntax));
        meta.insert(uid_el(tags::IMPLEMENTATION_CLASS_UID, IMPLEMENTATION_CLASS_UID));
        meta.insert(Element::string(tags::IMPLEMENTATION_VERSION_NAME, Vr::SH, IMPLEMENTATION_VERSION_NAME));
        DicomFile { preamble: Some(vec![0; 128]), meta, dataset, bare_transfer_syntax: None }
    }

    /// The transfer syntax UID of the data set.
    pub fn transfer_syntax_uid(&self) -> String {
        self.meta
            .string(tags::TRANSFER_SYNTAX_UID)
            .filter(|s| !s.is_empty())
            .or_else(|| self.bare_transfer_syntax.clone())
            .unwrap_or_else(|| ts::IMPLICIT_VR_LITTLE_ENDIAN.to_string())
    }

    /// The transfer syntax of the data set.
    pub fn transfer_syntax(&self) -> TransferSyntax {
        TransferSyntax::from_uid(&self.transfer_syntax_uid())
    }

    /// Set the transfer syntax UID in the meta information (the data set is written in it).
    pub fn set_transfer_syntax(&mut self, uid: &str) {
        let mut b = uid.as_bytes().to_vec();
        if b.len() % 2 == 1 {
            b.push(0);
        }
        if self.meta.is_empty() {
            self.bare_transfer_syntax = Some(uid.to_string());
        } else {
            self.meta.insert(Element::bytes(tags::TRANSFER_SYNTAX_UID, Vr::UI, b));
        }
    }

    /// Parse a Part 10 file. Also accepts a file without a preamble (starting with `DICM` or
    /// with group 0002 directly) and a bare data set (implicit or explicit VR little endian,
    /// guessed from the first element).
    pub fn parse(bytes: &[u8], opts: ReadOptions) -> Result<DicomFile> {
        let (preamble, meta_start) = if bytes.len() >= 132 && &bytes[128..132] == b"DICM" {
            (Some(bytes[..128].to_vec()), Some(132))
        } else if bytes.len() >= 4 && &bytes[..4] == b"DICM" {
            (None, Some(4))
        } else if bytes.len() >= 8 && bytes[0] == 2 && bytes[1] == 0 && Vr::from_bytes(&bytes[4..6]).is_some() {
            (None, Some(0))
        } else {
            (None, None)
        };
        let Some(start) = meta_start else {
            if bytes.len() < 8 {
                return Err(perr("Not DICOM: too short"));
            }
            let explicit = Vr::from_bytes(&bytes[4..6]).is_some();
            // a big endian group number has its high byte first: 00 08 rather than 08 00
            let big = explicit && bytes[0] == 0 && bytes[1] != 0;
            let enc = match (explicit, big) {
                (true, true) => Encoding::EXPLICIT_BE,
                (true, false) => Encoding::EXPLICIT_LE,
                _ => Encoding::IMPLICIT_LE,
            };
            let ds = read_dataset(bytes, enc, opts)?;
            let uid = match (explicit, big) {
                (true, true) => ts::EXPLICIT_VR_BIG_ENDIAN,
                (true, false) => ts::EXPLICIT_VR_LITTLE_ENDIAN,
                _ => ts::IMPLICIT_VR_LITTLE_ENDIAN,
            };
            return Ok(DicomFile { preamble: None, meta: DataSet::new(), dataset: ds, bare_transfer_syntax: Some(uid.to_string()) });
        };
        // file meta: explicit VR little endian elements of group 0002
        let mut r = Reader { buf: bytes, pos: start, opts: ReadOptions::default() };
        let mut meta = DataSet::new();
        while r.pos + 6 <= bytes.len() {
            let g = u16::from_le_bytes([bytes[r.pos], bytes[r.pos + 1]]);
            if g != 0x0002 {
                break;
            }
            let t = r.tag(Encoding::EXPLICIT_LE)?;
            meta.push(r.element(t, Encoding::EXPLICIT_LE)?);
        }
        let rest = &bytes[r.pos..];
        let uid = meta.string(tags::TRANSFER_SYNTAX_UID).unwrap_or_else(|| ts::IMPLICIT_VR_LITTLE_ENDIAN.to_string());
        let mut tsx = TransferSyntax::from_uid(&uid);
        // a file whose meta says explicit but whose data set is implicit (seen in the wild)
        if !tsx.deflated && tsx.encoding.explicit_vr && !tsx.encoding.big_endian && rest.len() >= 8 && Vr::from_bytes(&rest[4..6]).is_none() {
            tsx.encoding = Encoding::IMPLICIT_LE;
        }
        let dataset = read_dataset_ts(rest, &tsx, opts)?;
        Ok(DicomFile { preamble, meta, dataset, bare_transfer_syntax: None })
    }

    /// Encode as Part 10 (the meta group length, when present, is recomputed). A file read without meta
    /// information is written as a bare data set.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let tsx = self.transfer_syntax();
        let mut out = Vec::new();
        if !self.meta.is_empty() {
            match &self.preamble {
                Some(p) => {
                    let mut p = p.clone();
                    p.resize(128, 0);
                    out.extend_from_slice(&p);
                    out.extend_from_slice(b"DICM");
                }
                None => out.extend_from_slice(b"DICM"),
            }
            // a meta group without its group length (non-conformant, seen in the wild) stays so
            write_dataset(&mut out, &self.meta, Encoding::EXPLICIT_LE)?;
        }
        if tsx.encoding.big_endian && tsx.uid != ts::EXPLICIT_VR_BIG_ENDIAN {
            return Err(CamelError::new("IllegalArgumentException", "Big endian writing needs the Explicit VR Big Endian transfer syntax"));
        }
        out.extend_from_slice(&dataset_to_bytes(&self.dataset, &tsx)?);
        Ok(out)
    }
}

impl DicomFile {
    /// The preamble, `DICM` and the file meta information, without the data set (empty for a
    /// bare data set).
    pub fn header_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if !self.meta.is_empty() {
            let mut p = self.preamble.clone().unwrap_or_default();
            if self.preamble.is_some() {
                p.resize(128, 0);
                out.extend_from_slice(&p);
            }
            out.extend_from_slice(b"DICM");
            write_dataset(&mut out, &self.meta, Encoding::EXPLICIT_LE)?;
        }
        Ok(out)
    }
}

/// Read only the file meta information: the meta group and the offset where the data set
/// starts (`None` for a bare data set without meta information).
pub fn split_part10(bytes: &[u8]) -> Result<(DataSet, Option<usize>)> {
    let start = if bytes.len() >= 132 && &bytes[128..132] == b"DICM" {
        132
    } else if bytes.len() >= 4 && &bytes[..4] == b"DICM" {
        4
    } else if bytes.len() >= 8 && bytes[0] == 2 && bytes[1] == 0 && Vr::from_bytes(&bytes[4..6]).is_some() {
        0
    } else {
        return Ok((DataSet::new(), None));
    };
    let mut r = Reader { buf: bytes, pos: start, opts: ReadOptions::default() };
    let mut meta = DataSet::new();
    while r.pos + 6 <= bytes.len() {
        let g = u16::from_le_bytes([bytes[r.pos], bytes[r.pos + 1]]);
        if g != 0x0002 {
            break;
        }
        let t = r.tag(Encoding::EXPLICIT_LE)?;
        meta.push(r.element(t, Encoding::EXPLICIT_LE)?);
    }
    Ok((meta, Some(r.pos)))
}

/// Whether the bytes look like DICOM (a Part 10 file or `DICM` without preamble).
pub fn is_part10(bytes: &[u8]) -> bool {
    (bytes.len() >= 132 && &bytes[128..132] == b"DICM") || (bytes.len() >= 4 && &bytes[..4] == b"DICM")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DataSet {
        let mut ds = DataSet::new();
        ds.insert(Element::string(tags::SOP_CLASS_UID, Vr::UI, "1.2.840.10008.5.1.4.1.1.7"));
        ds.insert(Element::string(tags::SOP_INSTANCE_UID, Vr::UI, "1.2.3.4.5"));
        ds.insert(Element::string(tags::PATIENT_NAME, Vr::PN, "Doe^Jane"));
        ds.insert(Element::string(tags::PATIENT_ID, Vr::LO, "PID-7"));
        ds.insert(Element::numbers(0x0028_0010, Vr::US, &[2.0]));
        let mut item = DataSet::new();
        item.insert(Element::string(tags::REFERENCED_SOP_INSTANCE_UID, Vr::UI, "9.8.7"));
        let mut item2 = DataSet::new();
        item2.insert(Element::string(0x0008_0100, Vr::SH, "CODE"));
        ds.insert(Element { tag: 0x0008_1140, vr: Vr::SQ, value: Value::Sequence { items: vec![Item { dataset: item, undefined_length: true }, Item::new(item2)], undefined_length: true } });
        ds.insert(Element::bytes(0x0009_0010, Vr::LO, b"ACME".to_vec()));
        ds.insert(Element::bytes(0x0009_1001, Vr::UN, vec![1, 2, 3, 4]));
        ds.insert(Element::bytes(tags::PIXEL_DATA, Vr::OW, vec![1, 0, 2, 0, 3, 0, 4, 0]));
        ds
    }

    #[test]
    fn round_trips_in_every_native_syntax() {
        for uid in [ts::IMPLICIT_VR_LITTLE_ENDIAN, ts::EXPLICIT_VR_LITTLE_ENDIAN, ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN, ts::EXPLICIT_VR_BIG_ENDIAN] {
            let f = DicomFile::new(sample(), uid);
            let bytes = f.to_bytes().unwrap();
            let back = DicomFile::parse(&bytes, ReadOptions::default()).unwrap();
            assert_eq!(back.transfer_syntax_uid(), uid);
            assert_eq!(back.to_bytes().unwrap(), bytes, "{uid}");
            assert_eq!(back.dataset.string(tags::PATIENT_NAME).unwrap(), "Doe^Jane");
            let seq = back.dataset.get(0x0008_1140).unwrap().items().unwrap();
            assert_eq!(seq.len(), 2);
            assert_eq!(seq[0].dataset.string(tags::REFERENCED_SOP_INSTANCE_UID).unwrap(), "9.8.7");
            assert_eq!(back.dataset.get(tags::PIXEL_DATA).unwrap().raw().unwrap(), &[1, 0, 2, 0, 3, 0, 4, 0]);
        }
    }

    #[test]
    fn deflated_data_sets_have_even_length() {
        // found against DCMTK: an odd deflated stream makes an odd PDV, which DCMTK aborts on
        let tsx = TransferSyntax::from_uid(ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN);
        for n in 0..12 {
            let mut ds = sample();
            ds.insert(Element::string(0x0008_1030, Vr::LO, &"x".repeat(n * 7)));
            let b = dataset_to_bytes(&ds, &tsx).unwrap();
            assert_eq!(b.len() % 2, 0);
            assert_eq!(read_dataset_ts(&b, &tsx, ReadOptions::default()).unwrap(), ds);
        }
    }

    #[test]
    fn encapsulated_fragments_and_stop_before_pixel_data() {
        let mut ds = sample();
        ds.insert(Element { tag: tags::PIXEL_DATA, vr: Vr::OB, value: Value::Fragments(vec![vec![], vec![0xFF, 0xD8, 0xFF, 0xD9]]) });
        let f = DicomFile::new(ds, ts::JPEG_BASELINE);
        let bytes = f.to_bytes().unwrap();
        let back = DicomFile::parse(&bytes, ReadOptions::default()).unwrap();
        assert_eq!(back.to_bytes().unwrap(), bytes);
        match &back.dataset.get(tags::PIXEL_DATA).unwrap().value {
            Value::Fragments(fr) => assert_eq!(fr[1], vec![0xFF, 0xD8, 0xFF, 0xD9]),
            v => panic!("{v:?}"),
        }
        let head = DicomFile::parse(&bytes, ReadOptions { stop_before_pixel_data: true }).unwrap();
        assert!(!head.dataset.contains(tags::PIXEL_DATA) && head.dataset.contains(tags::PATIENT_ID));
    }

    #[test]
    fn bare_dataset_and_truncation() {
        let mut out = Vec::new();
        write_dataset(&mut out, &sample(), Encoding::EXPLICIT_LE).unwrap();
        let f = DicomFile::parse(&out, ReadOptions::default()).unwrap();
        assert_eq!(f.transfer_syntax_uid(), ts::EXPLICIT_VR_LITTLE_ENDIAN);
        assert_eq!(f.to_bytes().unwrap(), out);
        let err = DicomFile::parse(&out[..out.len() - 3], ReadOptions::default()).unwrap_err();
        assert_eq!(err.kind, DICOM_PARSE_EXCEPTION);
    }
}
