//! XML 1.0 (fifth edition) parser with Namespaces in XML 1.0, producing a [`Document`].
//!
//! * XML declaration (version, encoding, standalone); byte input honours a UTF-8 / UTF-16
//!   BOM and the declared encoding (`UTF-8`, `ISO-8859-1`/`latin1`, `US-ASCII`,
//!   `windows-1252`, `UTF-16`).
//! * Elements, attributes (normalization, also of DTD-typed non-CDATA attributes),
//!   character data, CDATA sections, comments, processing instructions, character
//!   references, predefined entities.
//! * DOCTYPE with internal subset: general and parameter entity declarations (internal
//!   entities are expanded, also when they contain markup), `ATTLIST` default values and
//!   `ID` attribute types (for the XPath `id()` function), unparsed entities (`NDATA`).
//!   `ELEMENT` and `NOTATION` declarations are skipped (the parser is non-validating).
//!   External entities are **not** loaded (references fail, to prevent XXE); the
//!   external DTD subset is ignored.
//! * Entity expansion is limited (recursion is detected, total expanded size is capped)
//!   to defeat "billion laughs" documents.
//! * Well-formedness errors carry line and column numbers.

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::dom::{Document, NodeId, QName, XMLNS_NS, XML_NS};

/// A well-formedness (or encoding) error.
#[derive(Clone, Debug, PartialEq)]
pub struct XmlError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "lineNumber: {}; columnNumber: {}; {}", self.line, self.col, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for XmlError {}

impl From<XmlError> for cameleer_core::error::CamelError {
    fn from(e: XmlError) -> Self {
        crate::register_kinds();
        cameleer_core::error::CamelError::new(crate::SAX_PARSE_EXCEPTION, e.to_string())
    }
}

/// Parser options.
#[derive(Clone, Debug)]
pub struct ParseOptions {
    /// Namespace processing (default true).
    pub namespaces: bool,
    /// Maximum total number of characters produced by entity expansion.
    pub max_entity_expansion: usize,
    /// Base URI recorded on the document.
    pub base_uri: Option<String>,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions { namespaces: true, max_entity_expansion: 1_000_000, base_uri: None }
    }
}

/// Parse XML text.
pub fn parse(text: &str) -> Result<Document, XmlError> {
    parse_with(text, &ParseOptions::default())
}

/// Parse XML text with options.
pub fn parse_with(text: &str, opts: &ParseOptions) -> Result<Document, XmlError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Parser::new(text, opts.clone()).run()
}

/// Parse XML bytes, detecting the encoding from the BOM or the XML declaration.
pub fn parse_bytes(bytes: &[u8]) -> Result<Document, XmlError> {
    parse(&decode_bytes(bytes)?)
}

fn enc_err(m: impl Into<String>) -> XmlError {
    XmlError { message: m.into(), line: 0, col: 0 }
}

/// Decode XML bytes to text (BOM, then the encoding declaration; default UTF-8).
pub fn decode_bytes(bytes: &[u8]) -> Result<String, XmlError> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return utf8(rest);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return utf16(&bytes[2..], false);
    }
    if bytes.starts_with(&[0x3C, 0x00, 0x3F, 0x00]) {
        return utf16(bytes, true);
    }
    if bytes.starts_with(&[0x00, 0x3C, 0x00, 0x3F]) {
        return utf16(bytes, false);
    }
    let enc = declared_encoding(bytes).unwrap_or_else(|| "UTF-8".into());
    match normalize_encoding(&enc).as_str() {
        "UTF8" => utf8(bytes),
        "ISO88591" | "LATIN1" | "ISO8859_1" | "L1" | "CP819" | "IBM819" => Ok(bytes.iter().map(|&b| b as char).collect()),
        "USASCII" | "ASCII" => {
            if let Some(p) = bytes.iter().position(|&b| b >= 0x80) {
                return Err(enc_err(format!("Invalid byte {} at offset {} for encoding US-ASCII", bytes[p], p)));
            }
            Ok(bytes.iter().map(|&b| b as char).collect())
        }
        "WINDOWS1252" | "CP1252" => Ok(bytes.iter().map(|&b| cp1252(b)).collect()),
        "UTF16" | "UTF16LE" => utf16(bytes, true),
        "UTF16BE" => utf16(bytes, false),
        _ => Err(enc_err(format!("Unsupported encoding: {}", enc))),
    }
}

fn normalize_encoding(e: &str) -> String {
    e.chars().filter(|c| *c != '-' && *c != ' ').collect::<String>().to_ascii_uppercase().replace("ISO_", "ISO")
}

fn utf8(b: &[u8]) -> Result<String, XmlError> {
    String::from_utf8(b.to_vec()).map_err(|e| {
        let off = e.utf8_error().valid_up_to();
        enc_err(format!("Invalid byte 1 of 1-byte UTF-8 sequence (offset {})", off))
    })
}

fn utf16(b: &[u8], le: bool) -> Result<String, XmlError> {
    if b.len() % 2 != 0 {
        return Err(enc_err("Truncated UTF-16 input"));
    }
    let units: Vec<u16> = b.chunks(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
    String::from_utf16(&units).map_err(|_| enc_err("Invalid UTF-16 sequence"))
}

fn cp1252(b: u8) -> char {
    const HI: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018,
        0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) {
        char::from_u32(HI[(b - 0x80) as usize] as u32).unwrap_or('\u{fffd}')
    } else {
        b as char
    }
}

/// The `encoding` pseudo-attribute of an XML declaration at the start of `bytes`.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.iter().position(|&b| b == b'>')?;
    let decl = std::str::from_utf8(&bytes[..end]).ok()?;
    let i = decl.find("encoding")?;
    let rest = decl[i + 8..].trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let rest = &rest[1..];
    Some(rest[..rest.find(q)?].to_string())
}

// ---------------------------------------------------------------------------
// character classes
// ---------------------------------------------------------------------------

pub(crate) fn is_name_start_char(c: char) -> bool {
    matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z')
        || matches!(c as u32, 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
}

pub(crate) fn is_name_char(c: char) -> bool {
    is_name_start_char(c) || matches!(c, '-' | '.' | '0'..='9') || matches!(c as u32, 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
}

pub(crate) fn is_xml_char(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Whether `s` is a valid XML Name.
pub fn is_name(s: &str) -> bool {
    let mut it = s.chars();
    matches!(it.next(), Some(c) if is_name_start_char(c)) && it.all(is_name_char)
}

/// Whether `s` is a valid NCName (a Name without colons).
pub fn is_ncname(s: &str) -> bool {
    is_name(s) && !s.contains(':')
}

// ---------------------------------------------------------------------------
// DTD model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum EntityDef {
    Internal(Rc<Vec<char>>),
    External { system: String, ndata: Option<String> },
}

#[derive(Clone, Debug)]
struct AttDef {
    name: String,
    ty: String,
    default: Option<String>,
}

#[derive(Default)]
struct Dtd {
    general: HashMap<String, EntityDef>,
    parameter: HashMap<String, EntityDef>,
    attlists: HashMap<String, Vec<AttDef>>,
}

// ---------------------------------------------------------------------------
// parser
// ---------------------------------------------------------------------------

struct Frame {
    chars: Rc<Vec<char>>,
    pos: usize,
    /// Name of the entity being expanded (None for the document itself).
    entity: Option<String>,
    /// Element depth when the frame was entered.
    depth: usize,
}

struct RawAttr {
    qname: String,
    value: String,
}

struct Parser {
    frames: Vec<Frame>,
    line: u32,
    col: u32,
    doc: Document,
    dtd: Dtd,
    opts: ParseOptions,
    expanded: usize,
    /// Namespace bindings stack (prefix, uri) and marks per open element.
    ns: Vec<(String, String)>,
    ns_marks: Vec<usize>,
    open: Vec<NodeId>,
    open_names: Vec<String>,
}

type R<T> = Result<T, XmlError>;

impl Parser {
    fn new(text: &str, opts: ParseOptions) -> Parser {
        let mut chars: Vec<char> = Vec::with_capacity(text.len());
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\r' {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                chars.push('\n');
            } else {
                chars.push(c);
            }
        }
        let mut doc = Document::new();
        doc.base_uri = opts.base_uri.clone();
        Parser {
            frames: vec![Frame { chars: Rc::new(chars), pos: 0, entity: None, depth: 0 }],
            line: 1,
            col: 1,
            doc,
            dtd: Dtd::default(),
            opts,
            expanded: 0,
            ns: vec![("xml".into(), XML_NS.into()), ("xmlns".into(), XMLNS_NS.into())],
            ns_marks: Vec::new(),
            open: Vec::new(),
            open_names: Vec::new(),
        }
    }

    fn err<T>(&self, m: impl Into<String>) -> R<T> {
        Err(XmlError { message: m.into(), line: self.line, col: self.col })
    }

    fn top(&self) -> &Frame {
        self.frames.last().unwrap()
    }

    fn peek(&self) -> Option<char> {
        let f = self.top();
        f.chars.get(f.pos).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        let f = self.top();
        f.chars.get(f.pos + k).copied()
    }

    fn at_end(&self) -> bool {
        let f = self.top();
        f.pos >= f.chars.len()
    }

    fn bump(&mut self) -> Option<char> {
        let base = self.frames.len() == 1;
        let f = self.frames.last_mut().unwrap();
        let c = f.chars.get(f.pos).copied()?;
        f.pos += 1;
        if base {
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        let f = self.top();
        let mut i = f.pos;
        for c in s.chars() {
            if f.chars.get(i) != Some(&c) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            for _ in s.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str, what: &str) -> R<()> {
        if self.eat(s) {
            Ok(())
        } else {
            self.err(format!("Expected '{}' {}", s, what))
        }
    }

    fn skip_ws(&mut self) -> bool {
        let mut any = false;
        while let Some(c) = self.peek() {
            if is_ws(c) {
                self.bump();
                any = true;
            } else {
                break;
            }
        }
        any
    }

    fn require_ws(&mut self, what: &str) -> R<()> {
        if self.skip_ws() {
            Ok(())
        } else {
            self.err(format!("White space is required {}", what))
        }
    }

    fn name(&mut self) -> R<String> {
        match self.peek() {
            Some(c) if is_name_start_char(c) => {}
            Some(c) => return self.err(format!("Invalid name start character '{}'", c)),
            None => return self.err("Unexpected end of input, a name was expected"),
        }
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if is_name_char(c) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(s)
    }

    fn quoted_raw(&mut self) -> R<String> {
        let q = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.err("A quoted string was expected"),
        };
        self.bump();
        let mut s = String::new();
        loop {
            match self.bump() {
                Some(c) if c == q => return Ok(s),
                Some(c) => s.push(c),
                None => return self.err("Unterminated quoted string"),
            }
        }
    }

    fn run(mut self) -> R<Document> {
        if self.starts_with("<?xml") && self.peek_at(5).map(is_ws).unwrap_or(false) {
            self.xml_decl()?;
        }
        let mut seen_doctype = false;
        let mut seen_root = false;
        loop {
            self.skip_ws();
            if self.at_end() {
                break;
            }
            if self.starts_with("<?") {
                self.pi(0)?;
            } else if self.starts_with("<!--") {
                self.comment(0)?;
            } else if self.starts_with("<!DOCTYPE") {
                if seen_doctype || seen_root {
                    return self.err("DOCTYPE is not allowed here");
                }
                seen_doctype = true;
                self.doctype()?;
            } else if self.peek() == Some('<') {
                if seen_root {
                    return self.err("The markup in the document following the root element must be well-formed.");
                }
                seen_root = true;
                self.element_tree()?;
            } else if seen_root {
                return self.err("Content is not allowed in trailing section.");
            } else {
                return self.err("Content is not allowed in prolog.");
            }
        }
        if !seen_root {
            return self.err("Premature end of file: no root element.");
        }
        Ok(self.doc)
    }

    fn xml_decl(&mut self) -> R<()> {
        self.eat("<?xml");
        let mut seen_version = false;
        loop {
            let had_ws = self.skip_ws();
            if self.eat("?>") {
                break;
            }
            if !had_ws {
                return self.err("White space is required in the XML declaration");
            }
            let n = self.name()?;
            self.skip_ws();
            self.expect("=", "in the XML declaration")?;
            self.skip_ws();
            let v = self.quoted_raw()?;
            match n.as_str() {
                "version" => {
                    if !(v.starts_with("1.") && v.len() > 2 && v[2..].chars().all(|c| c.is_ascii_digit())) {
                        return self.err(format!("XML version \"{}\" is not supported", v));
                    }
                    seen_version = true;
                }
                "encoding" if seen_version => {
                    if !v.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                        return self.err(format!("Invalid encoding name \"{}\"", v));
                    }
                }
                "standalone" if seen_version => {
                    if v != "yes" && v != "no" {
                        return self.err("standalone must be \"yes\" or \"no\"");
                    }
                }
                _ => return self.err(format!("Invalid pseudo-attribute '{}' in the XML declaration", n)),
            }
        }
        if !seen_version {
            return self.err("The version is required in the XML declaration.");
        }
        Ok(())
    }

    fn pi(&mut self, parent: NodeId) -> R<()> {
        self.eat("<?");
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            return self.err("The processing instruction target matching \"[xX][mM][lL]\" is not allowed.");
        }
        if self.opts.namespaces && target.contains(':') {
            return self.err("Processing instruction targets must not contain colons");
        }
        let mut data = String::new();
        if !self.eat("?>") {
            self.require_ws("after the processing instruction target")?;
            loop {
                if self.eat("?>") {
                    break;
                }
                match self.bump() {
                    Some(c) if is_xml_char(c) => data.push(c),
                    Some(c) => return self.err(format!("Invalid XML character (Unicode: 0x{:x})", c as u32)),
                    None => return self.err("Unterminated processing instruction"),
                }
            }
        }
        self.doc.append_pi(parent, &target, &data);
        Ok(())
    }

    fn comment_text(&mut self) -> R<String> {
        self.eat("<!--");
        let mut s = String::new();
        loop {
            if self.starts_with("--") {
                if self.eat("-->") {
                    return Ok(s);
                }
                return self.err("The string \"--\" is not permitted within comments.");
            }
            match self.bump() {
                Some(c) if is_xml_char(c) => s.push(c),
                Some(c) => return self.err(format!("Invalid XML character (Unicode: 0x{:x}) in comment", c as u32)),
                None => return self.err("Unterminated comment"),
            }
        }
    }

    fn comment(&mut self, parent: NodeId) -> R<()> {
        let s = self.comment_text()?;
        self.doc.append_comment(parent, &s);
        Ok(())
    }

    // ---- DTD ----

    fn doctype(&mut self) -> R<()> {
        self.eat("<!DOCTYPE");
        self.require_ws("after DOCTYPE")?;
        self.name()?;
        self.skip_ws();
        if self.starts_with("SYSTEM") || self.starts_with("PUBLIC") {
            self.external_id(false)?;
            self.skip_ws();
        }
        if self.eat("[") {
            self.int_subset(false)?;
            self.skip_ws();
        }
        self.expect(">", "to end the DOCTYPE declaration")?;
        Ok(())
    }

    /// Returns the system literal.
    fn external_id(&mut self, allow_public_only: bool) -> R<String> {
        if self.eat("SYSTEM") {
            self.require_ws("after SYSTEM")?;
            return self.quoted_raw();
        }
        self.expect("PUBLIC", "")?;
        self.require_ws("after PUBLIC")?;
        self.quoted_raw()?;
        let ws = self.skip_ws();
        if allow_public_only && !matches!(self.peek(), Some('"' | '\'')) {
            return Ok(String::new());
        }
        if !ws {
            return self.err("White space is required between the public and system identifiers");
        }
        self.quoted_raw()
    }

    /// Parse internal subset declarations until `]` (or the end of a PE frame).
    fn int_subset(&mut self, in_pe: bool) -> R<()> {
        loop {
            self.skip_ws();
            if self.at_end() {
                if in_pe {
                    return Ok(());
                }
                return self.err("Unterminated DOCTYPE internal subset");
            }
            if !in_pe && self.eat("]") {
                return Ok(());
            }
            if self.starts_with("<!--") {
                self.comment_text()?;
            } else if self.starts_with("<?") {
                // PIs in the DTD are not part of the tree
                let mut tmp = Document::new();
                std::mem::swap(&mut tmp, &mut self.doc);
                let r = self.pi(0);
                std::mem::swap(&mut tmp, &mut self.doc);
                r?;
            } else if self.starts_with("<!ENTITY") {
                self.entity_decl()?;
            } else if self.starts_with("<!ATTLIST") {
                self.attlist_decl()?;
            } else if self.starts_with("<!ELEMENT") || self.starts_with("<!NOTATION") {
                self.skip_decl()?;
            } else if self.peek() == Some('%') {
                self.bump();
                let n = self.name()?;
                self.expect(";", "after a parameter entity reference")?;
                match self.dtd.parameter.get(&n).cloned() {
                    Some(EntityDef::Internal(text)) => {
                        self.count_expansion(text.len())?;
                        if self.frames.iter().any(|f| f.entity.as_deref() == Some(&format!("%{}", n))) {
                            return self.err(format!("Recursive entity reference \"%{}\"", n));
                        }
                        self.frames.push(Frame { chars: text, pos: 0, entity: Some(format!("%{}", n)), depth: 0 });
                        let r = self.int_subset(true);
                        self.frames.pop();
                        r?;
                    }
                    Some(EntityDef::External { .. }) => {} // external parameter entities are not read
                    None => return self.err(format!("The entity \"{}\" was referenced, but not declared.", n)),
                }
            } else {
                return self.err("The markup declarations contained or pointed to by the document type declaration must be well-formed.");
            }
        }
    }

    fn skip_decl(&mut self) -> R<()> {
        loop {
            match self.peek() {
                Some('"') | Some('\'') => {
                    self.quoted_raw()?;
                }
                Some('>') => {
                    self.bump();
                    return Ok(());
                }
                Some(_) => {
                    self.bump();
                }
                None => return self.err("Unterminated markup declaration"),
            }
        }
    }

    fn count_expansion(&mut self, n: usize) -> R<()> {
        self.expanded += n;
        if self.expanded > self.opts.max_entity_expansion {
            return self.err(format!(
                "The parser has encountered more than \"{}\" entity expansions in this document; this is the limit imposed by the application.",
                self.opts.max_entity_expansion
            ));
        }
        Ok(())
    }

    fn entity_decl(&mut self) -> R<()> {
        self.eat("<!ENTITY");
        self.require_ws("after ENTITY")?;
        let pe = if self.peek() == Some('%') {
            self.bump();
            self.require_ws("after '%'")?;
            true
        } else {
            false
        };
        let name = self.name()?;
        if self.opts.namespaces && name.contains(':') {
            return self.err("Entity names must not contain colons");
        }
        self.require_ws("after the entity name")?;
        let def = if matches!(self.peek(), Some('"' | '\'')) {
            EntityDef::Internal(Rc::new(self.entity_value()?))
        } else {
            let system = self.external_id(false)?;
            let ws = self.skip_ws();
            let mut ndata = None;
            if self.starts_with("NDATA") {
                if !ws || pe {
                    return self.err("NDATA is not allowed here");
                }
                self.eat("NDATA");
                self.require_ws("after NDATA")?;
                ndata = Some(self.name()?);
            }
            EntityDef::External { system, ndata }
        };
        self.skip_ws();
        self.expect(">", "to end the ENTITY declaration")?;
        let table = if pe { &mut self.dtd.parameter } else { &mut self.dtd.general };
        if let EntityDef::External { system, ndata: Some(_) } = &def {
            self.doc.unparsed_entities.entry(name.clone()).or_insert_with(|| system.clone());
        }
        table.entry(name).or_insert(def); // the first declaration is binding
        Ok(())
    }

    /// An entity value literal: character and parameter entity references are expanded,
    /// general entity references are kept.
    fn entity_value(&mut self) -> R<Vec<char>> {
        let q = self.bump().unwrap();
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return self.err("Unterminated entity value"),
                Some(c) if c == q => {
                    self.bump();
                    return Ok(out);
                }
                Some('&') if self.peek_at(1) == Some('#') => {
                    let c = self.char_ref()?;
                    out.push(c);
                }
                Some('&') => {
                    self.bump();
                    let n = self.name()?;
                    self.expect(";", "after an entity reference")?;
                    out.push('&');
                    out.extend(n.chars());
                    out.push(';');
                }
                Some('%') => {
                    if self.frames.len() == 1 {
                        return self.err("Parameter entity references are not allowed within markup declarations in the internal subset");
                    }
                    self.bump();
                    let n = self.name()?;
                    self.expect(";", "after a parameter entity reference")?;
                    if let Some(EntityDef::Internal(t)) = self.dtd.parameter.get(&n).cloned() {
                        self.count_expansion(t.len())?;
                        out.extend(t.iter());
                    }
                }
                Some(c) => {
                    if !is_xml_char(c) {
                        return self.err(format!("Invalid XML character (Unicode: 0x{:x})", c as u32));
                    }
                    self.bump();
                    out.push(c);
                }
            }
        }
    }

    fn attlist_decl(&mut self) -> R<()> {
        self.eat("<!ATTLIST");
        self.require_ws("after ATTLIST")?;
        let el = self.name()?;
        loop {
            let ws = self.skip_ws();
            if self.eat(">") {
                return Ok(());
            }
            if !ws {
                return self.err("White space is required in the ATTLIST declaration");
            }
            let att = self.name()?;
            self.require_ws("after the attribute name")?;
            let ty = if self.peek() == Some('(') {
                self.skip_parens()?;
                "ENUMERATION".to_string()
            } else {
                let t = self.name()?;
                if t == "NOTATION" {
                    self.require_ws("after NOTATION")?;
                    self.skip_parens()?;
                }
                t
            };
            self.require_ws("after the attribute type")?;
            let default = if self.eat("#REQUIRED") || self.eat("#IMPLIED") {
                None
            } else {
                if self.eat("#FIXED") {
                    self.require_ws("after #FIXED")?;
                }
                let raw = self.attr_value_literal()?;
                Some(raw)
            };
            let list = self.dtd.attlists.entry(el.clone()).or_default();
            if !list.iter().any(|a| a.name == att) {
                list.push(AttDef { name: att, ty, default });
            }
        }
    }

    fn skip_parens(&mut self) -> R<()> {
        self.expect("(", "")?;
        loop {
            match self.bump() {
                Some(')') => return Ok(()),
                Some(_) => {}
                None => return self.err("Unterminated enumeration"),
            }
        }
    }

    // ---- references ----

    fn char_ref(&mut self) -> R<char> {
        self.eat("&#");
        let hex = self.eat("x");
        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if c == ';' {
                break;
            }
            digits.push(c);
            self.bump();
            if digits.len() > 10 {
                break;
            }
        }
        if !self.eat(";") {
            return self.err("The character reference must end with the ';' delimiter.");
        }
        let v = if hex { u32::from_str_radix(&digits, 16) } else { digits.parse::<u32>() };
        match v.ok().and_then(char::from_u32) {
            Some(c) if is_xml_char(c) && !digits.is_empty() && !digits.starts_with('+') => Ok(c),
            _ => self.err(format!("Character reference \"&#{}{}\" is an invalid XML character.", if hex { "x" } else { "" }, digits)),
        }
    }

    fn predefined(name: &str) -> Option<char> {
        match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => None,
        }
    }

    /// Expand a general entity for use inside an attribute value (recursively).
    fn expand_entity_in_attr(&mut self, name: &str, out: &mut String, stack: &mut Vec<String>) -> R<()> {
        if let Some(c) = Self::predefined(name) {
            out.push(c);
            return Ok(());
        }
        let def = match self.dtd.general.get(name) {
            Some(d) => d.clone(),
            None => return self.err(format!("The entity \"{}\" was referenced, but not declared.", name)),
        };
        let text = match def {
            EntityDef::Internal(t) => t,
            EntityDef::External { .. } => {
                return self.err(format!("The external entity reference \"&{};\" is not permitted in an attribute value.", name))
            }
        };
        if stack.iter().any(|s| s == name) {
            return self.err(format!("Recursive entity reference \"{}\". (Reference path: {})", name, stack.join(" -> ")));
        }
        self.count_expansion(text.len())?;
        stack.push(name.to_string());
        let mut i = 0;
        while i < text.len() {
            let c = text[i];
            match c {
                '<' => return self.err(format!("The value of attribute contains '<' through entity \"{}\"", name)),
                '&' => {
                    let end = text[i..].iter().position(|&c| c == ';').map(|p| p + i);
                    let Some(end) = end else { return self.err("Unterminated reference in entity value") };
                    let r: String = text[i + 1..end].iter().collect();
                    if let Some(num) = r.strip_prefix('#') {
                        let v = if let Some(h) = num.strip_prefix('x') { u32::from_str_radix(h, 16).ok() } else { num.parse().ok() };
                        match v.and_then(char::from_u32) {
                            Some(c) if is_xml_char(c) => out.push(c),
                            _ => return self.err(format!("Invalid character reference &{};", r)),
                        }
                    } else {
                        self.expand_entity_in_attr(&r, out, stack)?;
                    }
                    i = end + 1;
                }
                '\t' | '\n' | '\r' => {
                    out.push(' ');
                    i += 1;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        stack.pop();
        Ok(())
    }

    /// An attribute value literal, normalized (CDATA normalization).
    fn attr_value_literal(&mut self) -> R<String> {
        let q = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.err("Open quote is expected for attribute value"),
        };
        self.bump();
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.err("The value of the attribute must end with the matching quote character."),
                Some(c) if c == q => {
                    self.bump();
                    return Ok(out);
                }
                Some('<') => return self.err("The value of attribute must not contain the '<' character."),
                Some('&') if self.peek_at(1) == Some('#') => {
                    let c = self.char_ref()?;
                    out.push(c);
                }
                Some('&') => {
                    self.bump();
                    let n = self.name()?;
                    self.expect(";", "after an entity reference")?;
                    let mut stack = Vec::new();
                    self.expand_entity_in_attr(&n, &mut out, &mut stack)?;
                }
                Some(c) => {
                    if !is_xml_char(c) {
                        return self.err(format!("An invalid XML character (Unicode: 0x{:x}) was found in the value of an attribute.", c as u32));
                    }
                    self.bump();
                    out.push(if is_ws(c) { ' ' } else { c });
                }
            }
        }
    }

    // ---- elements ----

    fn lookup_ns(&self, prefix: &str) -> Option<&str> {
        self.ns.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
    }

    fn split_qname<'a>(&self, q: &'a str) -> R<(&'a str, &'a str)> {
        match q.split_once(':') {
            Some((p, l)) => {
                if p.is_empty() || l.is_empty() || l.contains(':') {
                    return self.err(format!("The qualified name \"{}\" is not a valid QName.", q));
                }
                Ok((p, l))
            }
            None => Ok(("", q)),
        }
    }

    /// Parse a start tag at `<`; returns true for an empty-element tag.
    fn start_tag(&mut self) -> R<bool> {
        let (line, col) = (self.line, self.col);
        self.bump(); // <
        let qname = self.name()?;
        let mut raw: Vec<RawAttr> = Vec::new();
        let empty;
        loop {
            let ws = self.skip_ws();
            if self.eat("/>") {
                empty = true;
                break;
            }
            if self.eat(">") {
                empty = false;
                break;
            }
            if self.at_end() {
                return self.err(format!("XML document structures must start and end within the same entity (element \"{}\").", qname));
            }
            if !ws {
                return self.err(format!("Element type \"{}\" must be followed by either attribute specifications, \">\" or \"/>\".", qname));
            }
            let an = self.name()?;
            self.skip_ws();
            if !self.eat("=") {
                return self.err(format!("Attribute name \"{}\" associated with an element type \"{}\" must be followed by the ' = ' character.", an, qname));
            }
            self.skip_ws();
            let v = self.attr_value_literal()?;
            if raw.iter().any(|a| a.qname == an) {
                return self.err(format!("Attribute \"{}\" was already specified for element \"{}\".", an, qname));
            }
            raw.push(RawAttr { qname: an, value: v });
        }
        // DTD attribute defaults and normalization
        let mut id_attrs: Vec<String> = Vec::new();
        if let Some(defs) = self.dtd.attlists.get(&qname) {
            for d in defs {
                if d.ty == "ID" {
                    id_attrs.push(d.name.clone());
                }
                if let Some(a) = raw.iter_mut().find(|a| a.qname == d.name) {
                    if d.ty != "CDATA" {
                        a.value = a.value.split(' ').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                    }
                } else if let Some(def) = &d.default {
                    let mut v = def.clone();
                    if d.ty != "CDATA" {
                        v = v.split(' ').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                    }
                    raw.push(RawAttr { qname: d.name.clone(), value: v });
                }
            }
        }
        let parent = *self.open.last().unwrap_or(&0);
        self.ns_marks.push(self.ns.len());
        let el;
        if self.opts.namespaces {
            let mut decls: Vec<(String, String)> = Vec::new();
            for a in &raw {
                if a.qname == "xmlns" {
                    if a.value == XML_NS || a.value == XMLNS_NS {
                        return self.err(format!("The namespace \"{}\" cannot be the default namespace", a.value));
                    }
                    decls.push((String::new(), a.value.clone()));
                } else if let Some(p) = a.qname.strip_prefix("xmlns:") {
                    if p == "xmlns" {
                        return self.err("The prefix \"xmlns\" cannot be bound to any namespace explicitly");
                    }
                    if p == "xml" && a.value != XML_NS {
                        return self.err("The prefix \"xml\" cannot be bound to any namespace other than its usual namespace");
                    }
                    if p != "xml" && (a.value == XML_NS || a.value == XMLNS_NS) {
                        return self.err(format!("The namespace \"{}\" cannot be bound to prefix \"{}\"", a.value, p));
                    }
                    if a.value.is_empty() {
                        return self.err(format!("The value of the attribute \"prefix=\"xmlns\",localpart=\"{}\",rawname=\"{}\"\" is invalid. Prefixed namespace bindings may not be empty.", p, a.qname));
                    }
                    if !is_ncname(p) {
                        return self.err(format!("Invalid namespace prefix \"{}\"", p));
                    }
                    decls.push((p.to_string(), a.value.clone()));
                }
            }
            for (p, u) in &decls {
                self.ns.push((p.clone(), u.clone()));
            }
            let (pfx, local) = self.split_qname(&qname)?;
            let uri = if pfx.is_empty() {
                self.lookup_ns("").unwrap_or("").to_string()
            } else {
                match self.lookup_ns(pfx) {
                    Some(u) if !u.is_empty() => u.to_string(),
                    _ => return self.err(format!("The prefix \"{}\" for element \"{}\" is not bound.", pfx, qname)),
                }
            };
            el = self.doc.append_element(parent, QName::ns(pfx, local, uri));
            self.doc.nodes[el].ns_decls = decls;
            let mut seen: Vec<(String, String)> = Vec::new();
            for a in &raw {
                if a.qname == "xmlns" || a.qname.starts_with("xmlns:") {
                    continue;
                }
                let (ap, al) = self.split_qname(&a.qname)?;
                let au = if ap.is_empty() {
                    String::new()
                } else {
                    match self.lookup_ns(ap) {
                        Some(u) if !u.is_empty() => u.to_string(),
                        _ => {
                            return self.err(format!(
                                "The prefix \"{}\" for attribute \"{}\" associated with an element type \"{}\" is not bound.",
                                ap, a.qname, qname
                            ))
                        }
                    }
                };
                if seen.iter().any(|(u, l)| *u == au && l == al) {
                    return self.err(format!("Attribute \"{}\" was already specified for element \"{}\".", a.qname, qname));
                }
                seen.push((au.clone(), al.to_string()));
                let aid = self.doc.set_attribute(el, QName::ns(ap, al, au.clone()), a.value.clone());
                let _ = aid;
                if (au == XML_NS && al == "id") || id_attrs.iter().any(|n| *n == a.qname) {
                    self.doc.ids.entry(a.value.trim().to_string()).or_insert(el);
                }
            }
        } else {
            el = self.doc.append_element(parent, QName::local(qname.clone()));
            for a in &raw {
                self.doc.set_attribute(el, QName::local(a.qname.clone()), a.value.clone());
                if id_attrs.iter().any(|n| *n == a.qname) {
                    self.doc.ids.entry(a.value.trim().to_string()).or_insert(el);
                }
            }
        }
        self.doc.nodes[el].line = line;
        self.doc.nodes[el].col = col;
        if empty {
            let m = self.ns_marks.pop().unwrap();
            self.ns.truncate(m);
        } else {
            self.open.push(el);
            self.open_names.push(qname);
        }
        Ok(empty)
    }

    fn end_tag(&mut self) -> R<()> {
        self.eat("</");
        let n = self.name()?;
        self.skip_ws();
        if !self.eat(">") {
            return self.err(format!("The end-tag for element type \"{}\" must end with a '>' delimiter.", n));
        }
        match self.open_names.last() {
            Some(open) if *open == n => {}
            Some(open) => {
                let open = open.clone();
                return self.err(format!("The element type \"{}\" must be terminated by the matching end-tag \"</{}>\".", open, open));
            }
            None => return self.err(format!("Unexpected end tag </{}>", n)),
        }
        if self.top().depth > self.open.len() - 1 && self.frames.len() > 1 {
            return self.err("The end tag of an element started outside the entity was found inside the entity");
        }
        self.open.pop();
        self.open_names.pop();
        let m = self.ns_marks.pop().unwrap();
        self.ns.truncate(m);
        Ok(())
    }

    /// Parse the root element and all of its content.
    fn element_tree(&mut self) -> R<()> {
        let empty = self.start_tag()?;
        if empty {
            return Ok(());
        }
        let mut text = String::new();
        loop {
            if self.at_end() {
                if self.frames.len() > 1 {
                    let f = self.frames.last().unwrap();
                    if f.depth != self.open.len() {
                        let name = f.entity.clone().unwrap_or_default();
                        return self.err(format!("The entity \"{}\" must contain balanced markup", name));
                    }
                    self.frames.pop();
                    continue;
                }
                let open = self.open_names.last().cloned().unwrap_or_default();
                return self.err(format!("XML document structures must start and end within the same entity. Element \"{}\" is not closed.", open));
            }
            let c = self.peek().unwrap();
            if c == '<' {
                let parent = *self.open.last().unwrap();
                if !text.is_empty() {
                    self.doc.append_text(parent, &text);
                    text.clear();
                }
                if self.starts_with("</") {
                    self.end_tag()?;
                    if self.open.is_empty() {
                        return Ok(());
                    }
                } else if self.starts_with("<!--") {
                    self.comment(parent)?;
                } else if self.starts_with("<![CDATA[") {
                    self.eat("<![CDATA[");
                    let mut s = String::new();
                    loop {
                        if self.eat("]]>") {
                            break;
                        }
                        match self.bump() {
                            Some(c) if is_xml_char(c) => s.push(c),
                            Some(c) => return self.err(format!("Invalid XML character (Unicode: 0x{:x}) in CDATA section", c as u32)),
                            None => return self.err("The CDATA section must end with ']]>'"),
                        }
                    }
                    self.doc.append_text(parent, &s);
                } else if self.starts_with("<?") {
                    self.pi(parent)?;
                } else if self.starts_with("<!") {
                    return self.err("Markup declarations are not allowed in content");
                } else {
                    self.start_tag()?;
                }
            } else if c == '&' {
                if self.peek_at(1) == Some('#') {
                    let ch = self.char_ref()?;
                    text.push(ch);
                } else {
                    self.bump();
                    let n = self.name()?;
                    if !self.eat(";") {
                        return self.err(format!("The reference to entity \"{}\" must end with the ';' delimiter.", n));
                    }
                    if let Some(ch) = Self::predefined(&n) {
                        text.push(ch);
                        continue;
                    }
                    match self.dtd.general.get(&n).cloned() {
                        Some(EntityDef::Internal(t)) => {
                            if self.frames.iter().any(|f| f.entity.as_deref() == Some(n.as_str())) {
                                return self.err(format!("Recursive entity reference \"{}\"", n));
                            }
                            self.count_expansion(t.len())?;
                            if !t.contains(&'<') && !t.contains(&'&') {
                                text.extend(t.iter());
                            } else {
                                let parent = *self.open.last().unwrap();
                                if !text.is_empty() {
                                    self.doc.append_text(parent, &text);
                                    text.clear();
                                }
                                let depth = self.open.len();
                                self.frames.push(Frame { chars: t, pos: 0, entity: Some(n), depth });
                            }
                        }
                        Some(EntityDef::External { ndata: Some(_), .. }) => {
                            return self.err(format!("The entity \"{}\" is an unparsed entity and may not be referenced.", n))
                        }
                        Some(EntityDef::External { system, .. }) => {
                            return self.err(format!("External entity \"{}\" (\"{}\") is not supported: external entities are not resolved", n, system))
                        }
                        None => return self.err(format!("The entity \"{}\" was referenced, but not declared.", n)),
                    }
                }
            } else {
                if c == ']' && self.starts_with("]]>") {
                    return self.err("The character sequence \"]]>\" must not appear in content unless used to mark the end of a CDATA section.");
                }
                if !is_xml_char(c) {
                    return self.err(format!("An invalid XML character (Unicode: 0x{:x}) was found in the element content of the document.", c as u32));
                }
                self.bump();
                text.push(c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::NodeType;

    #[test]
    fn basic_document() {
        let d = parse("<?xml version='1.0' encoding='UTF-8'?>\n<!-- c --><a x='1' xmlns:p='urn:p'><p:b p:y='2'>t&amp;<![CDATA[<x>]]></p:b><?pi data?></a>").unwrap();
        let a = d.document_element().unwrap();
        assert_eq!(d.local_name(a), "a");
        let b = d.element_children(a)[0];
        assert_eq!(d.namespace_uri(b), "urn:p");
        assert_eq!(d.string_value(b), "t&<x>");
        assert_eq!(d.attribute_ns(b, "urn:p", "y"), Some("2"));
        assert_eq!(d.node(d.children(0)[0]).ty, NodeType::Comment);
    }

    #[test]
    fn dtd_entities_and_defaults() {
        let x = r#"<!DOCTYPE r [
            <!ENTITY name "World">
            <!ENTITY % pe "<!ENTITY fromPe 'pe-value'>">
            %pe;
            <!ENTITY markup "<b>bold &name;</b>">
            <!ATTLIST r version CDATA "1.0" key ID #IMPLIED>
        ]><r key="k1">Hello &name; &markup; &fromPe;</r>"#;
        let d = parse(x).unwrap();
        let r = d.document_element().unwrap();
        assert_eq!(d.string_value(r), "Hello World bold World pe-value");
        assert_eq!(d.attribute(r, "version"), Some("1.0"));
        assert_eq!(d.ids.get("k1"), Some(&r));
        assert_eq!(d.element_children(r).len(), 1);
    }

    #[test]
    fn errors() {
        assert!(parse("<a><b></a>").unwrap_err().message.contains("must be terminated"));
        assert!(parse("<a>").is_err());
        assert!(parse("<p:a/>").unwrap_err().message.contains("not bound"));
        assert!(parse("<a x='1' x='2'/>").is_err());
        assert!(parse("<a/><b/>").is_err());
        assert!(parse("<a>&undefined;</a>").is_err());
        assert!(parse("<!DOCTYPE a [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><a>&x;</a>").unwrap_err().message.contains("External entity"));
        let lol = "<!DOCTYPE a [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;\"><!ENTITY c \"&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;\"><!ENTITY d \"&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;\"><!ENTITY e \"&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;\"><!ENTITY f \"&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;\"><!ENTITY g \"&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;\">]><a>&g;</a>";
        assert!(parse(lol).unwrap_err().message.contains("entity expansions"));
        assert!(parse("<!DOCTYPE a [<!ENTITY r \"&r;\">]><a>&r;</a>").unwrap_err().message.contains("Recursive"));
        let e = parse("<a>\n<b>\n</c></a>").unwrap_err();
        assert_eq!(e.line, 3);
    }

    #[test]
    fn encodings() {
        let latin = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>caf\xe9</a>";
        let d = parse_bytes(latin).unwrap();
        assert_eq!(d.string_value(0), "caf\u{e9}");
        let mut u16 = vec![0xFF, 0xFE];
        for u in "<a>\u{e9}</a>".encode_utf16() {
            u16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse_bytes(&u16).unwrap().string_value(0), "\u{e9}");
        assert!(parse_bytes(b"<?xml version=\"1.0\" encoding=\"EBCDIC\"?><a/>").is_err());
    }
}
