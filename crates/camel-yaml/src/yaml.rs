//! A std-only YAML 1.2 parser and emitter.
//!
//! The parser supports the subset of YAML used by real-world configuration files:
//!
//! - block mappings and sequences (including compact `- key: v` nested mappings and
//!   sequences of sequences), explicit `? key` / `: value` entries
//! - flow collections `{a: 1, b: [x, y]}` spanning several lines
//! - plain, single-quoted and double-quoted scalars (escapes, multi-line folding)
//! - literal `|` and folded `>` block scalars with chomping (`-`/`+`) and explicit
//!   indentation indicators
//! - comments, directives, multiple documents (`---` / `...`, see [`parse_all`])
//! - anchors `&a`, aliases `*a` and merge keys `<<`
//! - basic tags (`!!str`, `!!int`, `!!float`, `!!bool`, `!!null`, `!!binary`, ...)
//! - YAML 1.2 core schema scalar resolution
//!
//! Parsing happens in two steps: the text is parsed into a tree of [`Node`]s (which
//! keep line/column information) and the nodes are converted into
//! [`camel_core::Value`]s. Errors carry precise line/column positions ([`YamlError`]).
//!
//! [`to_yaml`] emits a [`Value`] as block-style YAML.

use std::collections::{HashMap, HashSet};
use std::fmt;

use camel_core::error::kinds;
use camel_core::{CamelError, Value, value::ObjectMap};

/// Maximum number of nodes that alias expansion may create (protection against
/// "billion laughs" documents).
const ALIAS_EXPANSION_LIMIT: usize = 1_000_000;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A YAML syntax or conversion error with a 1-based line and column.
#[derive(Clone, Debug, PartialEq)]
pub struct YamlError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl YamlError {
    pub fn new(message: impl Into<String>, line: usize, column: usize) -> Self {
        YamlError { message: message.into(), line, column }
    }
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at line {}, column {}", self.message, self.line, self.column)
    }
}

impl std::error::Error for YamlError {}

impl From<YamlError> for CamelError {
    fn from(e: YamlError) -> Self {
        CamelError::new(kinds::PARSE, format!("Invalid YAML: {}", e))
    }
}

type YResult<T> = std::result::Result<T, YamlError>;

// ---------------------------------------------------------------------------
// Node tree
// ---------------------------------------------------------------------------

/// How a scalar was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

/// Content of a node.
#[derive(Clone, Debug)]
pub enum NodeKind {
    /// A scalar: its (unescaped, folded) text and style. An empty plain scalar is null.
    Scalar(String, ScalarStyle),
    Sequence(Vec<Node>),
    /// Mapping entries in document order.
    Mapping(Vec<(Node, Node)>),
}

/// A parsed YAML node with its position (1-based line and column).
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub tag: Option<String>,
    pub anchor: Option<String>,
    pub line: usize,
    pub column: usize,
}

/// One segment of a path into a document (used to locate nodes for diagnostics).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Key(k) => write!(f, ".{}", k),
            PathSegment::Index(i) => write!(f, "[{}]", i),
        }
    }
}

impl Node {
    fn empty(line: usize, column: usize) -> Node {
        Node { kind: NodeKind::Scalar(String::new(), ScalarStyle::Plain), tag: None, anchor: None, line, column }
    }

    /// Whether this node resolves to null (empty or `~`/`null` plain scalar without tag).
    pub fn is_null(&self) -> bool {
        match &self.kind {
            NodeKind::Scalar(t, ScalarStyle::Plain) => self.tag.is_none() && matches!(t.as_str(), "" | "~" | "null" | "Null" | "NULL"),
            _ => false,
        }
    }

    /// Look up a mapping entry by key.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.kind {
            NodeKind::Mapping(pairs) => pairs.iter().find(|(k, _)| k.key_string().map(|s| s == key).unwrap_or(false)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Sequence item by index.
    pub fn index(&self, i: usize) -> Option<&Node> {
        match &self.kind {
            NodeKind::Sequence(items) => items.get(i),
            _ => None,
        }
    }

    /// Follow a path as far as possible; returns the deepest node found.
    pub fn lookup(&self, path: &[PathSegment]) -> &Node {
        let mut cur = self;
        for seg in path {
            let next = match seg {
                PathSegment::Key(k) => cur.get(k),
                PathSegment::Index(i) => cur.index(*i),
            };
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        cur
    }

    /// Position (line, column) of the node at `path`. When the path ends with a mapping
    /// key, the position of the key itself is returned. Falls back to the deepest node found.
    pub fn position(&self, path: &[PathSegment]) -> (usize, usize) {
        if let Some((PathSegment::Key(k), parent_path)) = path.split_last() {
            if let Some(NodeKind::Mapping(pairs)) = self.try_lookup(parent_path).map(|n| &n.kind) {
                if let Some((key, _)) = pairs.iter().find(|(kn, _)| kn.key_string().map(|s| &s == k).unwrap_or(false)) {
                    return (key.line, key.column);
                }
            }
        }
        let n = self.lookup(path);
        (n.line, n.column)
    }

    /// Follow a path exactly; None if some segment does not exist.
    pub fn try_lookup(&self, path: &[PathSegment]) -> Option<&Node> {
        let mut cur = self;
        for seg in path {
            cur = match seg {
                PathSegment::Key(k) => cur.get(k)?,
                PathSegment::Index(i) => cur.index(*i)?,
            };
        }
        Some(cur)
    }

    /// The string used when this node is a mapping key.
    pub fn key_string(&self) -> YResult<String> {
        match &self.kind {
            NodeKind::Scalar(t, _) => Ok(t.clone()),
            _ => Ok(self.to_value()?.to_json()),
        }
    }

    fn count(&self) -> usize {
        match &self.kind {
            NodeKind::Scalar(..) => 1,
            NodeKind::Sequence(items) => 1 + items.iter().map(Node::count).sum::<usize>(),
            NodeKind::Mapping(pairs) => 1 + pairs.iter().map(|(k, v)| k.count() + v.count()).sum::<usize>(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> YamlError {
        YamlError::new(msg, self.line, self.column)
    }

    /// Convert to a [`Value`], applying tags, core-schema resolution and merge keys.
    /// Duplicate mapping keys are an error.
    pub fn to_value(&self) -> YResult<Value> {
        match &self.kind {
            NodeKind::Scalar(text, style) => resolve_scalar(text, *style, self.tag.as_deref()).map_err(|m| self.err(m)),
            NodeKind::Sequence(items) => {
                if let Some(t) = self.tag.as_deref() {
                    if is_tag(t, "map") || is_tag(t, "str") {
                        return Err(self.err(format!("tag {} cannot be applied to a sequence", t)));
                    }
                }
                Ok(Value::Array(items.iter().map(Node::to_value).collect::<YResult<_>>()?))
            }
            NodeKind::Mapping(pairs) => {
                if let Some(t) = self.tag.as_deref() {
                    if is_tag(t, "seq") || is_tag(t, "str") {
                        return Err(self.err(format!("tag {} cannot be applied to a mapping", t)));
                    }
                }
                // Like SnakeYAML's `mergeNode`: keys keep source order, merged keys are spliced in
                // where `<<` appears (without overriding earlier keys), and an explicit key
                // overrides a merged one in place.
                let mut out = ObjectMap::new();
                let mut explicit: HashSet<String> = HashSet::new();
                for (k, v) in pairs {
                    let is_merge = matches!(&k.kind, NodeKind::Scalar(t, ScalarStyle::Plain) if t == "<<") && k.tag.is_none();
                    if is_merge {
                        let sources: Vec<&Node> = match &v.kind {
                            NodeKind::Mapping(_) => vec![v],
                            NodeKind::Sequence(items) => items.iter().collect(),
                            _ => return Err(v.err("merge key '<<' requires a mapping or a sequence of mappings")),
                        };
                        for src in sources {
                            match src.to_value()? {
                                Value::Object(m) => {
                                    for (mk, mv) in m {
                                        out.entry(mk).or_insert(mv);
                                    }
                                }
                                _ => return Err(src.err("merge key '<<' requires a mapping or a sequence of mappings")),
                            }
                        }
                    } else {
                        let key = k.key_string()?;
                        if !explicit.insert(key.clone()) {
                            return Err(k.err(format!("duplicate mapping key '{}'", key)));
                        }
                        out.insert(key, v.to_value()?);
                    }
                }
                Ok(Value::Object(out))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public parse API
// ---------------------------------------------------------------------------

/// Parse a single YAML document into a [`Value`]. An empty input yields `Null`.
/// Fails if the input contains more than one document.
pub fn parse(text: &str) -> YResult<Value> {
    let docs = parse_nodes(text)?;
    match docs.len() {
        0 => Ok(Value::Null),
        1 => docs[0].to_value(),
        _ => Err(docs[1].err("expected a single document but found several (use parse_all)")),
    }
}

/// Parse all documents of a YAML stream.
pub fn parse_all(text: &str) -> YResult<Vec<Value>> {
    parse_nodes(text)?.iter().map(Node::to_value).collect()
}

/// Parse all documents of a YAML stream into node trees (with positions).
pub fn parse_nodes(text: &str) -> YResult<Vec<Node>> {
    let mut p = Parser::new(text);
    p.parse_stream()
}

// ---------------------------------------------------------------------------
// Scalar resolution (core schema)
// ---------------------------------------------------------------------------

fn is_tag(tag: &str, name: &str) -> bool {
    tag == format!("!!{}", name) || tag == format!("tag:yaml.org,2002:{}", name)
}

fn resolve_scalar(text: &str, style: ScalarStyle, tag: Option<&str>) -> std::result::Result<Value, String> {
    let tag = match tag {
        None => {
            return Ok(if style == ScalarStyle::Plain { resolve_plain(text) } else { Value::String(text.to_string()) });
        }
        Some(t) => t,
    };
    if tag == "!" || is_tag(tag, "str") || is_tag(tag, "timestamp") {
        return Ok(Value::String(text.to_string()));
    }
    if is_tag(tag, "int") {
        return parse_int(text.trim()).map(Value::Int).ok_or_else(|| format!("invalid !!int value '{}'", text));
    }
    if is_tag(tag, "float") {
        let t = text.trim();
        return parse_float(t)
            .or_else(|| parse_int(t).map(|i| i as f64))
            .map(Value::Float)
            .ok_or_else(|| format!("invalid !!float value '{}'", text));
    }
    if is_tag(tag, "bool") {
        return match text.trim() {
            "true" | "True" | "TRUE" => Ok(Value::Bool(true)),
            "false" | "False" | "FALSE" => Ok(Value::Bool(false)),
            _ => Err(format!("invalid !!bool value '{}'", text)),
        };
    }
    if is_tag(tag, "null") {
        return match text.trim() {
            "" | "~" | "null" | "Null" | "NULL" => Ok(Value::Null),
            _ => Err(format!("invalid !!null value '{}'", text)),
        };
    }
    if is_tag(tag, "binary") {
        return base64_decode(text).map(Value::Bytes).ok_or_else(|| "invalid base64 in !!binary value".to_string());
    }
    if is_tag(tag, "map") || is_tag(tag, "seq") {
        if style == ScalarStyle::Plain && text.is_empty() {
            return Ok(if is_tag(tag, "map") { Value::Object(ObjectMap::new()) } else { Value::Array(Vec::new()) });
        }
        return Err(format!("tag {} cannot be applied to a scalar", tag));
    }
    // Unknown (application specific) tags: resolve as if untagged.
    Ok(if style == ScalarStyle::Plain { resolve_plain(text) } else { Value::String(text.to_string()) })
}

/// Resolve a plain scalar using the YAML 1.2 core schema.
pub fn resolve_plain(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if is_decimal_int(text) {
        return match text.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => text.parse::<f64>().map(Value::Float).unwrap_or_else(|_| Value::String(text.to_string())),
        };
    }
    if let Some(i) = parse_int(text) {
        return Value::Int(i);
    }
    if let Some(f) = parse_float(text) {
        return Value::Float(f);
    }
    Value::String(text.to_string())
}

fn is_decimal_int(t: &str) -> bool {
    let d = t.strip_prefix(['-', '+']).unwrap_or(t);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

fn parse_int(t: &str) -> Option<i64> {
    if is_decimal_int(t) {
        return t.parse::<i64>().ok();
    }
    if let Some(h) = t.strip_prefix("0x") {
        if !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return i64::from_str_radix(h, 16).ok();
        }
    }
    if let Some(o) = t.strip_prefix("0o") {
        if !o.is_empty() && o.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return i64::from_str_radix(o, 8).ok();
        }
    }
    None
}

fn parse_float(t: &str) -> Option<f64> {
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    match body {
        ".inf" | ".Inf" | ".INF" => return Some(if neg { f64::NEG_INFINITY } else { f64::INFINITY }),
        ".nan" | ".NaN" | ".NAN" if body.len() == t.len() => return Some(f64::NAN),
        _ => {}
    }
    // [0-9]* ( . [0-9]* )? ( [eE] [-+]? [0-9]+ )? with at least one digit in mantissa
    let b = body.as_bytes();
    let mut i = 0;
    let mut mant_digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        mant_digits += 1;
    }
    let mut has_dot = false;
    if i < b.len() && b[i] == b'.' {
        has_dot = true;
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            mant_digits += 1;
        }
    }
    if mant_digits == 0 {
        return None;
    }
    let mut has_exp = false;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        has_exp = true;
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == s {
            return None;
        }
    }
    if i != b.len() || (!has_dot && !has_exp) {
        return None;
    }
    t.parse::<f64>().ok()
}

// ---------------------------------------------------------------------------
// Base64 (for !!binary)
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let b = [chunk[0], if n > 1 { chunk[1] } else { 0 }, if n > 2 { chunk[2] } else { 0 }];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(v >> 18) as usize & 63] as char);
        out.push(B64[(v >> 12) as usize & 63] as char);
        out.push(if n > 1 { B64[(v >> 6) as usize & 63] as char } else { '=' });
        out.push(if n > 2 { B64[v as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    Root,
    MapValue,
    SeqEntry,
    ExplicitKey,
}

#[derive(Clone, Copy)]
struct Mark {
    pos: usize,
    line: usize,
    col: usize,
    tab_on_line: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chomp {
    Strip,
    Clip,
    Keep,
}

struct Parser {
    s: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    /// A tab character was seen in the leading whitespace of the current line.
    tab_on_line: bool,
    anchors: HashMap<String, Node>,
    expanded: usize,
}

fn is_blank_or_end(c: Option<char>) -> bool {
    matches!(c, None | Some(' ') | Some('\t') | Some('\n'))
}

fn is_flow_indicator(c: char) -> bool {
    matches!(c, ',' | '[' | ']' | '{' | '}')
}

impl Parser {
    fn new(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        Parser { s: normalized.chars().collect(), pos: 0, line: 1, col: 0, tab_on_line: false, anchors: HashMap::new(), expanded: 0 }
    }

    // --- low level cursor ---

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.pos + n).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.s.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 0;
            self.tab_on_line = false;
        } else {
            self.col += 1;
        }
        Some(c)
    }
    fn mark(&self) -> Mark {
        Mark { pos: self.pos, line: self.line, col: self.col, tab_on_line: self.tab_on_line }
    }
    fn reset(&mut self, m: Mark) {
        self.pos = m.pos;
        self.line = m.line;
        self.col = m.col;
        self.tab_on_line = m.tab_on_line;
    }
    fn err<T>(&self, msg: impl Into<String>) -> YResult<T> {
        Err(YamlError::new(msg, self.line, self.col + 1))
    }
    fn err_at<T>(&self, m: Mark, msg: impl Into<String>) -> YResult<T> {
        Err(YamlError::new(msg, m.line, m.col + 1))
    }
    fn at_eof(&self) -> bool {
        self.pos >= self.s.len()
    }
    fn at_eol(&self) -> bool {
        matches!(self.peek(), None | Some('\n'))
    }
    fn prev_is_blank(&self) -> bool {
        self.pos == 0 || matches!(self.s.get(self.pos - 1), Some(' ') | Some('\t') | Some('\n'))
    }
    fn is_dash_entry(&self) -> bool {
        self.peek() == Some('-') && is_blank_or_end(self.peek_at(1))
    }
    fn at_doc_marker(&self) -> bool {
        if self.col != 0 {
            return false;
        }
        let three = |c: char| self.peek() == Some(c) && self.peek_at(1) == Some(c) && self.peek_at(2) == Some(c);
        (three('-') || three('.')) && is_blank_or_end(self.peek_at(3))
    }
    fn at_doc_start(&self) -> bool {
        self.at_doc_marker() && self.peek() == Some('-')
    }
    fn at_doc_end(&self) -> bool {
        self.at_doc_marker() && self.peek() == Some('.')
    }
    fn line_prefix_is_blank(&self) -> bool {
        let mut i = self.pos;
        while i > 0 {
            match self.s[i - 1] {
                '\n' => return true,
                ' ' | '\t' => i -= 1,
                _ => return false,
            }
        }
        true
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') && self.prev_is_blank() {
            while !self.at_eol() {
                self.bump();
            }
        }
    }

    fn skip_inline_ws_and_comment(&mut self) {
        self.skip_inline_ws();
        self.skip_comment();
    }

    /// After a node: only whitespace and a comment may follow on the line.
    fn skip_trailing(&mut self) -> YResult<()> {
        if self.line_prefix_is_blank() && !self.at_eol() {
            // a nested block node already advanced to the next content line
            return Ok(());
        }
        self.skip_inline_ws_and_comment();
        if !self.at_eol() {
            let c = self.peek().unwrap();
            if c == ':' {
                return self.err("mapping values are not allowed in this context");
            }
            return self.err(format!("unexpected character '{}' after node", c));
        }
        Ok(())
    }

    /// Skip blank lines, comment lines and leading indentation until the next content
    /// character (or EOF). Tabs used as indentation before content are an error.
    fn skip_to_content(&mut self) -> YResult<()> {
        loop {
            match self.peek() {
                Some(' ') => {
                    self.bump();
                }
                Some('\t') => {
                    if self.line_prefix_is_blank() {
                        self.tab_on_line = true;
                    }
                    self.bump();
                }
                Some('#') if self.prev_is_blank() => self.skip_comment(),
                Some('\n') => {
                    self.bump();
                }
                None => return Ok(()),
                Some(_) => {
                    if self.tab_on_line && self.line_prefix_is_blank() {
                        return self.err("found a tab character where indentation is expected (tabs are not allowed for indentation)");
                    }
                    return Ok(());
                }
            }
        }
    }

    // --- stream / documents ---

    fn parse_stream(&mut self) -> YResult<Vec<Node>> {
        let mut docs = Vec::new();
        loop {
            self.skip_to_content()?;
            if self.at_eof() {
                break;
            }
            // directives
            let mut had_directive = false;
            while self.col == 0 && self.peek() == Some('%') {
                had_directive = true;
                while !self.at_eol() {
                    self.bump();
                }
                self.skip_to_content()?;
            }
            if had_directive && !self.at_doc_start() {
                return self.err("expected '---' after directives");
            }
            if self.at_doc_end() {
                for _ in 0..3 {
                    self.bump();
                }
                self.skip_trailing()?;
                continue;
            }
            if self.at_doc_start() {
                for _ in 0..3 {
                    self.bump();
                }
            }
            self.anchors.clear();
            let node = self.parse_node(-1, Ctx::Root)?;
            docs.push(node);
            self.skip_trailing()?;
            self.skip_to_content()?;
            if self.at_eof() {
                break;
            }
            if self.at_doc_end() {
                for _ in 0..3 {
                    self.bump();
                }
                self.skip_trailing()?;
                continue;
            }
            if self.at_doc_start() {
                continue;
            }
            if self.col == 0 && self.peek() == Some('%') {
                return self.err("directives must be preceded by a document end marker '...'");
            }
            return self.err("unexpected content after the end of the document (bad indentation?)");
        }
        Ok(docs)
    }

    // --- block nodes ---

    fn parse_node(&mut self, parent: i32, ctx: Ctx) -> YResult<Node> {
        self.skip_inline_ws_and_comment();
        let block_start;
        if self.at_eol() {
            let m = self.mark();
            self.skip_to_content()?;
            if self.at_eof() || self.at_doc_marker() {
                return Ok(Node::empty(m.line, m.col + 1));
            }
            let ind = self.col as i32;
            let ok = ind > parent || (ctx == Ctx::MapValue && ind == parent && self.is_dash_entry());
            if !ok {
                return Ok(Node::empty(m.line, m.col + 1));
            }
            block_start = true;
        } else {
            block_start = self.line_prefix_is_blank();
        }
        let allow_block = block_start || ctx == Ctx::SeqEntry || ctx == Ctx::ExplicitKey;
        let node_mark = self.mark();
        let node_col = self.col as i32;

        // node properties
        let (anchor, tag) = self.parse_properties()?;
        if anchor.is_some() || tag.is_some() {
            self.skip_inline_ws_and_comment();
            if self.at_eol() {
                let mut node = self.parse_node(parent, if ctx == Ctx::Root { Ctx::Root } else { ctx })?;
                if node.is_null() && matches!(&node.kind, NodeKind::Scalar(t, _) if t.is_empty()) {
                    node.line = node_mark.line;
                    node.column = node_mark.col + 1;
                }
                return Ok(self.finish_node(node, anchor, tag));
            }
        }

        let c = match self.peek() {
            Some(c) => c,
            None => return Ok(self.finish_node(Node::empty(node_mark.line, node_mark.col + 1), anchor, tag)),
        };
        let m = self.mark();
        let node = match c {
            '-' if is_blank_or_end(self.peek_at(1)) => {
                if !allow_block {
                    return self.err("block sequence entries are not allowed in this context");
                }
                self.parse_block_seq(self.col as i32)?
            }
            '?' if is_blank_or_end(self.peek_at(1)) => {
                if !allow_block {
                    return self.err("explicit mapping keys are not allowed in this context");
                }
                self.parse_block_map(node_col, None, m)?
            }
            '|' | '>' => self.parse_block_scalar(parent)?,
            '[' | '{' => {
                let n = self.parse_flow_collection()?;
                self.maybe_key(n, allow_block, node_col, m)?
            }
            '*' => {
                let n = self.parse_alias()?;
                self.maybe_key(n, allow_block, node_col, m)?
            }
            '"' | '\'' => {
                let n = self.parse_quoted()?;
                self.maybe_key(n, allow_block, node_col, m)?
            }
            '@' | '`' => return self.err(format!("found reserved character '{}' that cannot start a plain scalar", c)),
            '&' | '!' => return self.err("node properties specified twice"),
            _ => {
                let first = self.scan_plain_line(false);
                if first.is_empty() {
                    return self.err(format!("unexpected character '{}'", c));
                }
                let n = Node { kind: NodeKind::Scalar(first, ScalarStyle::Plain), tag: None, anchor: None, line: m.line, column: m.col + 1 };
                let save = self.mark();
                self.skip_inline_ws();
                if self.peek() == Some(':') && is_blank_or_end(self.peek_at(1)) {
                    self.reset(save);
                    self.maybe_key(n, allow_block, node_col, m)?
                } else {
                    self.reset(save);
                    let NodeKind::Scalar(first, _) = n.kind else { unreachable!() };
                    let text = self.continue_plain_block(parent, first)?;
                    Node { kind: NodeKind::Scalar(text, ScalarStyle::Plain), tag: None, anchor: None, line: m.line, column: m.col + 1 }
                }
            }
        };
        Ok(self.finish_node(node, anchor, tag))
    }

    fn finish_node(&mut self, mut node: Node, anchor: Option<String>, tag: Option<String>) -> Node {
        if tag.is_some() {
            node.tag = tag;
        }
        if let Some(a) = anchor {
            node.anchor = Some(a.clone());
            self.anchors.insert(a, node.clone());
        }
        node
    }

    /// If the node just parsed is followed by `: `, it is the first key of a block mapping.
    fn maybe_key(&mut self, key: Node, allow_block: bool, map_col: i32, m: Mark) -> YResult<Node> {
        let save = self.mark();
        self.skip_inline_ws();
        if self.peek() == Some(':') && is_blank_or_end(self.peek_at(1)) {
            if !allow_block {
                return self.err("mapping values are not allowed in this context");
            }
            if key.line != self.line {
                return self.err_at(m, "implicit mapping keys must be on a single line");
            }
            return self.parse_block_map(map_col, Some(key), m);
        }
        self.reset(save);
        Ok(key)
    }

    fn parse_properties(&mut self) -> YResult<(Option<String>, Option<String>)> {
        let mut anchor = None;
        let mut tag = None;
        loop {
            match self.peek() {
                Some('&') if anchor.is_none() => {
                    self.bump();
                    let name = self.scan_anchor_name();
                    if name.is_empty() {
                        return self.err("expected an anchor name after '&'");
                    }
                    anchor = Some(name);
                }
                Some('!') if tag.is_none() => {
                    tag = Some(self.scan_tag()?);
                }
                _ => break,
            }
            let save = self.mark();
            self.skip_inline_ws();
            if !matches!(self.peek(), Some('&') | Some('!')) {
                self.reset(save);
                break;
            }
        }
        Ok((anchor, tag))
    }

    fn scan_anchor_name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || is_flow_indicator(c) {
                break;
            }
            name.push(c);
            self.bump();
        }
        name
    }

    fn scan_tag(&mut self) -> YResult<String> {
        let mut t = String::new();
        self.bump(); // '!'
        t.push('!');
        if self.peek() == Some('<') {
            self.bump();
            let mut verbatim = String::new();
            loop {
                match self.bump() {
                    Some('>') => break,
                    Some(c) if c != '\n' => verbatim.push(c),
                    _ => return self.err("unterminated verbatim tag"),
                }
            }
            return Ok(verbatim);
        }
        while let Some(c) = self.peek() {
            if c.is_whitespace() || is_flow_indicator(c) {
                break;
            }
            t.push(c);
            self.bump();
        }
        Ok(t)
    }

    fn parse_alias(&mut self) -> YResult<Node> {
        let m = self.mark();
        self.bump(); // '*'
        let name = self.scan_anchor_name();
        if name.is_empty() {
            return self.err_at(m, "expected an alias name after '*'");
        }
        match self.anchors.get(&name) {
            Some(n) => {
                self.expanded += n.count();
                if self.expanded > ALIAS_EXPANSION_LIMIT {
                    return self.err_at(m, "too many alias expansions (possible billion laughs attack)");
                }
                let mut n = n.clone();
                n.anchor = None;
                Ok(n)
            }
            None => self.err_at(m, format!("unknown alias '*{}'", name)),
        }
    }

    fn parse_block_seq(&mut self, col: i32) -> YResult<Node> {
        let start = self.mark();
        let mut items = Vec::new();
        loop {
            self.bump(); // '-'
            let item = self.parse_node(col, Ctx::SeqEntry)?;
            items.push(item);
            self.skip_trailing()?;
            self.skip_to_content()?;
            if self.at_eof() || self.at_doc_marker() {
                break;
            }
            let ind = self.col as i32;
            if ind < col {
                break;
            }
            if ind > col {
                return self.err("bad indentation of a sequence entry");
            }
            if !self.is_dash_entry() {
                break;
            }
        }
        Ok(Node { kind: NodeKind::Sequence(items), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }

    fn parse_block_map(&mut self, col: i32, first_key: Option<Node>, start: Mark) -> YResult<Node> {
        let mut pairs: Vec<(Node, Node)> = Vec::new();
        let mut pending = first_key;
        loop {
            let key;
            let mut has_value = true;
            if let Some(k) = pending.take() {
                key = k;
                self.skip_inline_ws();
            } else if self.peek() == Some('?') && is_blank_or_end(self.peek_at(1)) {
                self.bump();
                key = self.parse_node(col, Ctx::ExplicitKey)?;
                self.skip_trailing()?;
                self.skip_to_content()?;
                if !(self.col as i32 == col && self.peek() == Some(':') && is_blank_or_end(self.peek_at(1)) && !self.at_eof()) {
                    has_value = false;
                }
            } else {
                key = self.parse_key_node()?;
                self.skip_inline_ws();
                if !(self.peek() == Some(':') && is_blank_or_end(self.peek_at(1))) {
                    if key.line == self.line && !self.at_eol() {
                        return self.err("expected ':' after mapping key");
                    }
                    return Err(YamlError::new("could not find expected ':' after mapping key", key.line, key.column));
                }
            }
            let value = if has_value {
                // `: value` on its own line (explicit entry) allows compact collections
                let ctx = if self.line_prefix_is_blank() { Ctx::SeqEntry } else { Ctx::MapValue };
                self.bump(); // ':'
                self.parse_node(col, ctx)?
            } else {
                Node::empty(self.line, self.col + 1)
            };
            pairs.push((key, value));
            if has_value {
                self.skip_trailing()?;
                self.skip_to_content()?;
            }
            if self.at_eof() || self.at_doc_marker() {
                break;
            }
            let ind = self.col as i32;
            if ind < col {
                break;
            }
            if ind > col {
                return self.err("bad indentation of a mapping entry");
            }
            if self.is_dash_entry() {
                return self.err("expected a mapping key but found a sequence entry");
            }
        }
        Ok(Node { kind: NodeKind::Mapping(pairs), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }

    fn parse_key_node(&mut self) -> YResult<Node> {
        let m = self.mark();
        let (anchor, tag) = self.parse_properties()?;
        if anchor.is_some() || tag.is_some() {
            self.skip_inline_ws();
        }
        let node = match self.peek() {
            Some('"') | Some('\'') => self.parse_quoted()?,
            Some('*') => self.parse_alias()?,
            Some('[') | Some('{') => self.parse_flow_collection()?,
            Some(c) => {
                let mk = self.mark();
                let t = self.scan_plain_line(false);
                if t.is_empty() && c != ':' {
                    return self.err(format!("unexpected character '{}' where a mapping key was expected", c));
                }
                Node { kind: NodeKind::Scalar(t, ScalarStyle::Plain), tag: None, anchor: None, line: mk.line, column: mk.col + 1 }
            }
            None => return self.err_at(m, "expected a mapping key"),
        };
        Ok(self.finish_node(node, anchor, tag))
    }

    /// Scan a plain scalar until the end of the current line (or an indicator that
    /// terminates it). Trailing whitespace is not included.
    fn scan_plain_line(&mut self, flow: bool) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                '\n' => break,
                ' ' | '\t' => {
                    // look ahead past the whitespace run
                    let mut j = self.pos;
                    while matches!(self.s.get(j), Some(' ') | Some('\t')) {
                        j += 1;
                    }
                    match self.s.get(j).copied() {
                        None | Some('\n') | Some('#') => break,
                        Some(':') if is_blank_or_end(self.s.get(j + 1).copied()) || (flow && self.s.get(j + 1).map(|&c| is_flow_indicator(c)).unwrap_or(false)) => {
                            break
                        }
                        Some(d) if flow && is_flow_indicator(d) => break,
                        _ => {}
                    }
                    while self.pos < j {
                        out.push(self.bump().unwrap());
                    }
                }
                ':' if is_blank_or_end(self.peek_at(1)) || (flow && self.peek_at(1).map(is_flow_indicator).unwrap_or(false)) => break,
                c if flow && is_flow_indicator(c) => break,
                _ => {
                    out.push(c);
                    self.bump();
                }
            }
        }
        out
    }

    /// Continue a block plain scalar over following lines that are indented more than `parent`.
    fn continue_plain_block(&mut self, parent: i32, first: String) -> YResult<String> {
        let mut out = first;
        loop {
            let m = self.mark();
            self.skip_inline_ws();
            if self.peek() != Some('\n') {
                self.reset(m);
                break;
            }
            let mut breaks = 0;
            while self.peek() == Some('\n') {
                self.bump();
                breaks += 1;
                while matches!(self.peek(), Some(' ') | Some('\t')) {
                    self.bump();
                }
            }
            if self.at_eof() || (self.col as i32) <= parent || self.peek() == Some('#') {
                self.reset(m);
                break;
            }
            // doc markers are only recognised at column 0
            if self.at_doc_marker() {
                self.reset(m);
                break;
            }
            let line = self.scan_plain_line(false);
            if line.is_empty() {
                self.reset(m);
                break;
            }
            if breaks == 1 {
                out.push(' ');
            } else {
                for _ in 1..breaks {
                    out.push('\n');
                }
            }
            out.push_str(&line);
            let save = self.mark();
            self.skip_inline_ws();
            if self.peek() == Some(':') && is_blank_or_end(self.peek_at(1)) {
                return self.err("mapping values are not allowed in this context (a multi-line plain scalar cannot be a key)");
            }
            self.reset(save);
        }
        Ok(out)
    }

    fn parse_block_scalar(&mut self, parent: i32) -> YResult<Node> {
        let start = self.mark();
        let literal = self.bump() == Some('|');
        let mut chomp = Chomp::Clip;
        let mut explicit: Option<usize> = None;
        let mut seen_chomp = false;
        for _ in 0..2 {
            match self.peek() {
                Some('+') if !seen_chomp => {
                    chomp = Chomp::Keep;
                    seen_chomp = true;
                }
                Some('-') if !seen_chomp => {
                    chomp = Chomp::Strip;
                    seen_chomp = true;
                }
                Some(d @ '1'..='9') if explicit.is_none() => explicit = Some(d as usize - '0' as usize),
                Some('0') => return self.err("block scalar indentation indicator must be between 1 and 9"),
                _ => break,
            }
            self.bump();
        }
        self.skip_inline_ws();
        if self.peek() == Some('#') && !self.prev_is_blank() {
            return self.err("a comment after a block scalar header must be separated by whitespace");
        }
        self.skip_comment();
        if !self.at_eol() {
            return self.err("unexpected characters after block scalar header");
        }
        let mut indent: Option<usize> = explicit.map(|e| (parent + e as i32).max(0) as usize);
        // (is_blank, text)
        let mut lines: Vec<(bool, String)> = Vec::new();
        while self.peek() == Some('\n') {
            let m = self.mark();
            self.bump();
            let mut n = 0usize;
            while self.peek() == Some(' ') && indent.map(|i| n < i).unwrap_or(true) {
                self.bump();
                n += 1;
            }
            // blank line? (only spaces until end of line)
            let mut j = self.pos;
            while self.s.get(j) == Some(&' ') {
                j += 1;
            }
            let blank = matches!(self.s.get(j), None | Some('\n'));
            if blank {
                if self.s.get(j).is_none() {
                    // trailing spaces before EOF are not a line
                    while self.pos < j {
                        self.bump();
                    }
                    break;
                }
                let extra = if indent.is_some() { j - self.pos } else { 0 };
                while self.pos < j {
                    self.bump();
                }
                lines.push((true, " ".repeat(extra)));
                continue;
            }
            if n == 0 && self.at_doc_marker() {
                self.reset(m);
                break;
            }
            let ci = match indent {
                Some(i) => i,
                None => {
                    // more spaces may follow (we stopped at none limit) - n is the full indent here
                    if (n as i32) <= parent {
                        self.reset(m);
                        break;
                    }
                    indent = Some(n);
                    n
                }
            };
            if n < ci {
                if self.peek() == Some('\t') && n as i32 > parent {
                    return self.err("found a tab character where indentation is expected");
                }
                self.reset(m);
                break;
            }
            let mut text = String::new();
            while !self.at_eol() {
                text.push(self.bump().unwrap());
            }
            lines.push((false, text));
        }
        let last_content = lines.iter().rposition(|(b, _)| !*b);
        let (body, trailing) = match last_content {
            Some(i) => (&lines[..=i], lines.len() - i - 1),
            None => (&lines[..0], lines.len()),
        };
        let mut out = String::new();
        if literal {
            let parts: Vec<&str> = body.iter().map(|(_, t)| t.as_str()).collect();
            out.push_str(&parts.join("\n"));
        } else {
            let mut prev_normal = false;
            let mut have_content = false;
            let mut empties = 0usize;
            for (blank, text) in body {
                if *blank {
                    empties += 1;
                    continue;
                }
                let more = text.starts_with(' ') || text.starts_with('\t');
                if have_content {
                    if prev_normal && !more && empties == 0 {
                        out.push(' ');
                    } else if prev_normal && !more {
                        out.push_str(&"\n".repeat(empties));
                    } else {
                        out.push_str(&"\n".repeat(empties + 1));
                    }
                } else {
                    out.push_str(&"\n".repeat(empties));
                }
                out.push_str(text);
                have_content = true;
                prev_normal = !more;
                empties = 0;
            }
        }
        match chomp {
            Chomp::Strip => {}
            Chomp::Clip => {
                if !body.is_empty() {
                    out.push('\n');
                }
            }
            Chomp::Keep => {
                if !body.is_empty() {
                    out.push('\n');
                }
                out.push_str(&"\n".repeat(trailing));
            }
        }
        let style = if literal { ScalarStyle::Literal } else { ScalarStyle::Folded };
        Ok(Node { kind: NodeKind::Scalar(out, style), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }

    // --- quoted scalars ---

    fn parse_quoted(&mut self) -> YResult<Node> {
        let start = self.mark();
        let q = self.bump().unwrap();
        let double = q == '"';
        let mut out = String::new();
        let mut pending_ws = String::new();
        loop {
            let c = match self.peek() {
                None => {
                    return self.err_at(start, if double { "unterminated double-quoted scalar" } else { "unterminated single-quoted scalar" })
                }
                Some(c) => c,
            };
            match c {
                '\'' if !double => {
                    if self.peek_at(1) == Some('\'') {
                        out.push_str(&pending_ws);
                        pending_ws.clear();
                        out.push('\'');
                        self.bump();
                        self.bump();
                    } else {
                        self.bump();
                        break;
                    }
                }
                '"' if double => {
                    self.bump();
                    break;
                }
                ' ' | '\t' => {
                    pending_ws.push(c);
                    self.bump();
                }
                '\n' => {
                    pending_ws.clear();
                    let mut breaks = 0;
                    while self.peek() == Some('\n') {
                        self.bump();
                        breaks += 1;
                        while matches!(self.peek(), Some(' ') | Some('\t')) {
                            self.bump();
                        }
                    }
                    if self.col == 0 && self.at_doc_marker() {
                        return self.err("document marker inside a quoted scalar");
                    }
                    if breaks == 1 {
                        out.push(' ');
                    } else {
                        out.push_str(&"\n".repeat(breaks - 1));
                    }
                }
                '\\' if double => {
                    out.push_str(&pending_ws);
                    pending_ws.clear();
                    let esc_mark = self.mark();
                    self.bump();
                    let e = match self.bump() {
                        Some(e) => e,
                        None => return self.err_at(start, "unterminated double-quoted scalar"),
                    };
                    match e {
                        '0' => out.push('\0'),
                        'a' => out.push('\u{07}'),
                        'b' => out.push('\u{08}'),
                        't' | '\t' => out.push('\t'),
                        'n' => out.push('\n'),
                        'v' => out.push('\u{0b}'),
                        'f' => out.push('\u{0c}'),
                        'r' => out.push('\r'),
                        'e' => out.push('\u{1b}'),
                        ' ' => out.push(' '),
                        '"' => out.push('"'),
                        '/' => out.push('/'),
                        '\\' => out.push('\\'),
                        'N' => out.push('\u{85}'),
                        '_' => out.push('\u{a0}'),
                        'L' => out.push('\u{2028}'),
                        'P' => out.push('\u{2029}'),
                        'x' | 'u' | 'U' => {
                            let n = match e {
                                'x' => 2,
                                'u' => 4,
                                _ => 8,
                            };
                            let mut hex = String::new();
                            for _ in 0..n {
                                match self.bump() {
                                    Some(h) if h.is_ascii_hexdigit() => hex.push(h),
                                    _ => return self.err_at(esc_mark, format!("invalid \\{} escape: expected {} hex digits", e, n)),
                                }
                            }
                            let code = u32::from_str_radix(&hex, 16).unwrap();
                            match char::from_u32(code) {
                                Some(ch) => out.push(ch),
                                None => return self.err_at(esc_mark, format!("invalid unicode code point \\{}{}", e, hex)),
                            }
                        }
                        '\n' => {
                            // escaped line break: join without a space
                            while matches!(self.peek(), Some(' ') | Some('\t')) {
                                self.bump();
                            }
                            let mut breaks = 0;
                            while self.peek() == Some('\n') {
                                self.bump();
                                breaks += 1;
                                while matches!(self.peek(), Some(' ') | Some('\t')) {
                                    self.bump();
                                }
                            }
                            out.push_str(&"\n".repeat(breaks));
                        }
                        other => return self.err_at(esc_mark, format!("invalid escape sequence '\\{}' in double-quoted scalar", other)),
                    }
                }
                _ => {
                    out.push_str(&pending_ws);
                    pending_ws.clear();
                    out.push(c);
                    self.bump();
                }
            }
        }
        out.push_str(&pending_ws);
        let style = if double { ScalarStyle::DoubleQuoted } else { ScalarStyle::SingleQuoted };
        Ok(Node { kind: NodeKind::Scalar(out, style), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }

    // --- flow collections ---

    fn skip_flow_ws(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') | Some('\n') => {
                    self.bump();
                }
                Some('#') if self.prev_is_blank() => self.skip_comment(),
                _ => break,
            }
        }
    }

    fn parse_flow_collection(&mut self) -> YResult<Node> {
        if self.peek() == Some('[') {
            self.parse_flow_seq()
        } else {
            self.parse_flow_map()
        }
    }

    fn parse_flow_node(&mut self) -> YResult<Node> {
        self.skip_flow_ws();
        let m = self.mark();
        let (anchor, tag) = self.parse_properties()?;
        if anchor.is_some() || tag.is_some() {
            self.skip_flow_ws();
        }
        let node = match self.peek() {
            None => return self.err("unexpected end of stream inside a flow collection"),
            Some('[') => self.parse_flow_seq()?,
            Some('{') => self.parse_flow_map()?,
            Some('"') | Some('\'') => self.parse_quoted()?,
            Some('*') => self.parse_alias()?,
            Some(',') | Some(']') | Some('}') => Node::empty(m.line, m.col + 1),
            Some(':') if is_blank_or_end(self.peek_at(1)) || self.peek_at(1).map(is_flow_indicator).unwrap_or(false) => {
                Node::empty(m.line, m.col + 1)
            }
            Some(c @ ('@' | '`')) => return self.err(format!("found reserved character '{}' that cannot start a plain scalar", c)),
            Some(_) => {
                let mk = self.mark();
                let text = self.scan_plain_flow();
                Node { kind: NodeKind::Scalar(text, ScalarStyle::Plain), tag: None, anchor: None, line: mk.line, column: mk.col + 1 }
            }
        };
        Ok(self.finish_node(node, anchor, tag))
    }

    fn scan_plain_flow(&mut self) -> String {
        let mut out = self.scan_plain_line(true);
        loop {
            if self.peek() != Some('\n') {
                break;
            }
            let m = self.mark();
            let mut breaks = 0;
            while self.peek() == Some('\n') {
                self.bump();
                breaks += 1;
                while matches!(self.peek(), Some(' ') | Some('\t')) {
                    self.bump();
                }
            }
            let stop = match self.peek() {
                None => true,
                Some(c) if is_flow_indicator(c) || c == '#' => true,
                Some(':') => is_blank_or_end(self.peek_at(1)) || self.peek_at(1).map(is_flow_indicator).unwrap_or(false),
                _ => false,
            };
            if stop {
                self.reset(m);
                break;
            }
            let line = self.scan_plain_line(true);
            if breaks == 1 {
                out.push(' ');
            } else {
                out.push_str(&"\n".repeat(breaks - 1));
            }
            out.push_str(&line);
        }
        out
    }

    /// After a flow entry: is there a `:` value indicator?
    fn flow_value_indicator(&mut self, key: &Node) -> bool {
        if self.peek() != Some(':') {
            return false;
        }
        let adjacent_ok = matches!(&key.kind, NodeKind::Scalar(_, ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted))
            || matches!(&key.kind, NodeKind::Sequence(_) | NodeKind::Mapping(_));
        adjacent_ok || is_blank_or_end(self.peek_at(1)) || self.peek_at(1).map(is_flow_indicator).unwrap_or(false)
    }

    fn parse_flow_seq(&mut self) -> YResult<Node> {
        let start = self.mark();
        self.bump(); // '['
        let mut items = Vec::new();
        loop {
            self.skip_flow_ws();
            match self.peek() {
                None => return self.err_at(start, "unclosed flow sequence: expected ']'"),
                Some(']') => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            let entry_mark = self.mark();
            let explicit = self.peek() == Some('?') && is_blank_or_end(self.peek_at(1));
            if explicit {
                self.bump();
            }
            let item = self.parse_flow_node()?;
            self.skip_flow_ws();
            let item = if self.flow_value_indicator(&item) {
                self.bump();
                self.skip_flow_ws();
                let value = if matches!(self.peek(), Some(',') | Some(']')) { Node::empty(self.line, self.col + 1) } else { self.parse_flow_node()? };
                Node { kind: NodeKind::Mapping(vec![(item, value)]), tag: None, anchor: None, line: entry_mark.line, column: entry_mark.col + 1 }
            } else if explicit {
                Node {
                    kind: NodeKind::Mapping(vec![(item, Node::empty(self.line, self.col + 1))]),
                    tag: None,
                    anchor: None,
                    line: entry_mark.line,
                    column: entry_mark.col + 1,
                }
            } else {
                item
            };
            items.push(item);
            self.skip_flow_ws();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {}
                None => return self.err_at(start, "unclosed flow sequence: expected ']'"),
                Some(c) => return self.err(format!("expected ',' or ']' in flow sequence but found '{}'", c)),
            }
        }
        Ok(Node { kind: NodeKind::Sequence(items), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }

    fn parse_flow_map(&mut self) -> YResult<Node> {
        let start = self.mark();
        self.bump(); // '{'
        let mut pairs = Vec::new();
        loop {
            self.skip_flow_ws();
            match self.peek() {
                None => return self.err_at(start, "unclosed flow mapping: expected '}'"),
                Some('}') => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            if self.peek() == Some('?') && is_blank_or_end(self.peek_at(1)) {
                self.bump();
            }
            let key = self.parse_flow_node()?;
            self.skip_flow_ws();
            let value = if self.flow_value_indicator(&key) {
                self.bump();
                self.skip_flow_ws();
                if matches!(self.peek(), Some(',') | Some('}')) {
                    Node::empty(self.line, self.col + 1)
                } else {
                    self.parse_flow_node()?
                }
            } else {
                Node::empty(self.line, self.col + 1)
            };
            pairs.push((key, value));
            self.skip_flow_ws();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some('}') => {}
                None => return self.err_at(start, "unclosed flow mapping: expected '}'"),
                Some(c) => return self.err(format!("expected ',' or '}}' in flow mapping but found '{}'", c)),
            }
        }
        Ok(Node { kind: NodeKind::Mapping(pairs), tag: None, anchor: None, line: start.line, column: start.col + 1 })
    }
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Emit a value as block-style YAML (2-space indentation, sequences indented under
/// their key, multi-line strings as literal block scalars). Mapping keys are emitted
/// in the (sorted) order of the underlying map.
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    match v {
        Value::Object(m) if !m.is_empty() => emit_map(m, 0, false, &mut out),
        Value::Array(a) if !a.is_empty() => emit_seq(a, 0, false, &mut out),
        Value::String(s) if s.contains('\n') => {
            out.push_str(&quote_double(s));
            out.push('\n');
        }
        other => {
            out.push_str(&format_scalar(other));
            out.push('\n');
        }
    }
    out
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn emit_value_after_indicator(v: &Value, indent: usize, out: &mut String) {
    // Called after "key:" or "-" has been written.
    match v {
        Value::String(s) if use_literal(s) => {
            out.push(' ');
            emit_literal(s, indent, out);
        }
        _ => {
            out.push(' ');
            out.push_str(&format_scalar(v));
            out.push('\n');
        }
    }
}

fn emit_map(m: &ObjectMap, indent: usize, first_inline: bool, out: &mut String) {
    for (i, (k, v)) in m.iter().enumerate() {
        if !(first_inline && i == 0) {
            pad(out, indent);
        }
        out.push_str(&format_key(k));
        out.push(':');
        match v {
            Value::Object(sub) if !sub.is_empty() => {
                out.push('\n');
                emit_map(sub, indent + 2, false, out);
            }
            Value::Array(items) if !items.is_empty() => {
                out.push('\n');
                emit_seq(items, indent + 2, false, out);
            }
            _ => emit_value_after_indicator(v, indent + 2, out),
        }
    }
}

fn emit_seq(items: &[Value], indent: usize, first_inline: bool, out: &mut String) {
    for (i, v) in items.iter().enumerate() {
        if !(first_inline && i == 0) {
            pad(out, indent);
        }
        out.push('-');
        match v {
            Value::Object(sub) if !sub.is_empty() => {
                out.push(' ');
                emit_map(sub, indent + 2, true, out);
            }
            Value::Array(sub) if !sub.is_empty() => {
                out.push(' ');
                emit_seq(sub, indent + 2, true, out);
            }
            _ => emit_value_after_indicator(v, indent + 2, out),
        }
    }
}

fn use_literal(s: &str) -> bool {
    let body = s.trim_end_matches('\n');
    s.contains('\n')
        && !body.is_empty()
        && !s.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || c == '\u{feff}')
        && !body.lines().any(|l| l.ends_with(' ') || l.ends_with('\t'))
}

fn emit_literal(s: &str, indent: usize, out: &mut String) {
    let body = s.trim_end_matches('\n');
    let trailing = s.len() - body.len();
    out.push('|');
    let first_content = body.lines().find(|l| !l.is_empty()).unwrap_or("");
    if first_content.starts_with(' ') || first_content.starts_with('\t') {
        out.push('2');
    }
    match trailing {
        0 => out.push('-'),
        1 => {}
        _ => out.push('+'),
    }
    out.push('\n');
    for line in body.split('\n') {
        if !line.is_empty() {
            pad(out, indent);
            out.push_str(line);
        }
        out.push('\n');
    }
    for _ in 1..trailing.max(1) {
        out.push('\n');
    }
}

fn format_key(k: &str) -> String {
    if needs_quotes(k) || k == "<<" {
        quote_double(k)
    } else {
        k.to_string()
    }
}

/// Format a scalar value (collections are emitted as compact flow/JSON).
pub fn format_scalar(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_nan() {
                ".nan".to_string()
            } else if f.is_infinite() {
                if *f > 0.0 { ".inf".to_string() } else { "-.inf".to_string() }
            } else {
                let s = format!("{}", f);
                if s.contains(['.', 'e', 'E']) {
                    s
                } else {
                    format!("{}.0", s)
                }
            }
        }
        Value::String(s) => {
            if needs_quotes(s) {
                quote_double(s)
            } else {
                s.clone()
            }
        }
        Value::Bytes(b) => format!("!!binary {}", base64_encode(b)),
        Value::Array(a) if a.is_empty() => "[]".to_string(),
        Value::Object(o) if o.is_empty() => "{}".to_string(),
        other => other.to_json(),
    }
}

fn needs_quotes(s: &str) -> bool {
    if s.is_empty() || !matches!(resolve_plain(s), Value::String(_)) {
        return true;
    }
    let first = s.chars().next().unwrap();
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) || first.is_whitespace() {
        return true;
    }
    if s.ends_with(' ') || s.ends_with('\t') || s.ends_with(':') {
        return true;
    }
    if s.contains(": ") || s.contains(" #") || s.contains(":\t") || s.contains("\t#") {
        return true;
    }
    if s.starts_with("...") {
        return true;
    }
    s.chars().any(|c| c.is_control() || c == '\u{85}' || c == '\u{2028}' || c == '\u{2029}' || c == '\u{feff}')
}

fn quote_double(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\u{1b}' => out.push_str("\\e"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\x{:02X}", c as u32)),
            '\u{feff}' => out.push_str("\\uFEFF"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Value {
        match parse(s) {
            Ok(v) => v,
            Err(e) => panic!("parse failed: {}\n---\n{}", e, s),
        }
    }

    fn obj(items: Vec<(&str, Value)>) -> Value {
        Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    fn s(x: &str) -> Value {
        Value::String(x.to_string())
    }

    fn err(src: &str) -> YamlError {
        match parse(src) {
            Ok(v) => panic!("expected error for {:?}, got {:?}", src, v),
            Err(e) => e,
        }
    }

    #[test]
    fn empty_documents() {
        assert_eq!(p(""), Value::Null);
        assert_eq!(p("# only a comment\n\n"), Value::Null);
        assert_eq!(parse_all("").unwrap().len(), 0);
        assert_eq!(parse_all("---\n").unwrap(), vec![Value::Null]);
    }

    #[test]
    fn simple_mapping() {
        assert_eq!(p("a: 1\nb: two\n"), obj(vec![("a", Value::Int(1)), ("b", s("two"))]));
    }

    #[test]
    fn nested_mappings() {
        let v = p("a:\n  b:\n    c: 1\n  d: 2\ne: 3");
        assert_eq!(v, obj(vec![("a", obj(vec![("b", obj(vec![("c", Value::Int(1))])), ("d", Value::Int(2))])), ("e", Value::Int(3))]));
    }

    #[test]
    fn simple_sequence() {
        assert_eq!(p("- a\n- b\n- 3\n"), arr(vec![s("a"), s("b"), Value::Int(3)]));
    }

    #[test]
    fn sequence_of_compact_mappings() {
        let v = p("- name: a\n  value: 1\n- name: b\n  value: 2\n");
        assert_eq!(
            v,
            arr(vec![obj(vec![("name", s("a")), ("value", Value::Int(1))]), obj(vec![("name", s("b")), ("value", Value::Int(2))])])
        );
    }

    #[test]
    fn sequence_of_sequences() {
        assert_eq!(p("- - a\n  - b\n- - c\n"), arr(vec![arr(vec![s("a"), s("b")]), arr(vec![s("c")])]));
        assert_eq!(p("- - - deep\n"), arr(vec![arr(vec![arr(vec![s("deep")])])]));
    }

    #[test]
    fn sequence_as_map_value_same_indent() {
        let v = p("key:\n- a\n- b\nother: 1\n");
        assert_eq!(v, obj(vec![("key", arr(vec![s("a"), s("b")])), ("other", Value::Int(1))]));
    }

    #[test]
    fn sequence_as_map_value_indented() {
        let v = p("key:\n    - a\n    - b\nother: 1\n");
        assert_eq!(v, obj(vec![("key", arr(vec![s("a"), s("b")])), ("other", Value::Int(1))]));
    }

    #[test]
    fn camel_route_shape() {
        let src = r#"
- route:
    id: r1
    from:
      uri: direct:start
      steps:
        - setBody:
            simple: "${body}"
        - to: mock:result
"#;
        let v = p(src);
        let steps = v.get("0").unwrap().get("route").unwrap().get("from").unwrap().get("steps").unwrap();
        assert_eq!(steps.as_array().unwrap().len(), 2);
        assert_eq!(steps.get("1").unwrap().get("to").unwrap(), &s("mock:result"));
        assert_eq!(steps.get("0").unwrap().get("setBody").unwrap().get("simple").unwrap(), &s("${body}"));
    }

    #[test]
    fn compact_mapping_with_nested_seq() {
        let v = p("- from:\n    uri: a\n    steps:\n    - to: b\n    - to: c\n  extra: 1\n");
        let first = v.get("0").unwrap();
        assert_eq!(first.get("extra").unwrap(), &Value::Int(1));
        assert_eq!(first.get("from").unwrap().get("steps").unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn empty_values() {
        let v = p("a:\nb: ~\nc: null\nd:\n");
        assert_eq!(v, obj(vec![("a", Value::Null), ("b", Value::Null), ("c", Value::Null), ("d", Value::Null)]));
        assert_eq!(p("- \n-\n- x"), arr(vec![Value::Null, Value::Null, s("x")]));
    }

    #[test]
    fn core_schema_resolution() {
        let v = p("- true\n- False\n- TRUE\n- 42\n- -17\n- +5\n- 0x1F\n- 0o17\n- 1.5\n- -2.5e3\n- .5\n- .inf\n- -.Inf\n- 1e3\n- yes\n- 0b101\n- 1_000\n- 12abc\n- \"1\"\n");
        let a = v.as_array().unwrap();
        assert_eq!(a[0], Value::Bool(true));
        assert_eq!(a[1], Value::Bool(false));
        assert_eq!(a[2], Value::Bool(true));
        assert_eq!(a[3], Value::Int(42));
        assert_eq!(a[4], Value::Int(-17));
        assert_eq!(a[5], Value::Int(5));
        assert_eq!(a[6], Value::Int(31));
        assert_eq!(a[7], Value::Int(15));
        assert_eq!(a[8], Value::Float(1.5));
        assert_eq!(a[9], Value::Float(-2500.0));
        assert_eq!(a[10], Value::Float(0.5));
        assert_eq!(a[11], Value::Float(f64::INFINITY));
        assert_eq!(a[12], Value::Float(f64::NEG_INFINITY));
        assert_eq!(a[13], Value::Float(1000.0));
        assert_eq!(a[14], s("yes"));
        assert_eq!(a[15], s("0b101"));
        assert_eq!(a[16], s("1_000"));
        assert_eq!(a[17], s("12abc"));
        assert_eq!(a[18], s("1"));
        assert!(matches!(a[3], Value::Int(_)));
        assert!(matches!(a[13], Value::Float(_)));
    }

    #[test]
    fn nan_resolution() {
        match p(".nan") {
            Value::Float(f) => assert!(f.is_nan()),
            other => panic!("{:?}", other),
        }
        assert_eq!(p("-.nan"), s("-.nan"));
        assert_eq!(p("inf"), s("inf"));
        assert_eq!(p("nan"), s("nan"));
    }

    #[test]
    fn big_integers() {
        assert_eq!(p("9223372036854775807"), Value::Int(i64::MAX));
        assert!(matches!(p("92233720368547758070"), Value::Float(_)));
    }

    #[test]
    fn plain_scalars_with_colons_and_hashes() {
        let v = p("uri: timer:tick?period=1000\nurl: http://example.com/a#frag\nmsg: a#b\nk: v # comment\n");
        assert_eq!(v.get("uri").unwrap(), &s("timer:tick?period=1000"));
        assert_eq!(v.get("url").unwrap(), &s("http://example.com/a#frag"));
        assert_eq!(v.get("msg").unwrap(), &s("a#b"));
        assert_eq!(v.get("k").unwrap(), &s("v"));
    }

    #[test]
    fn plain_with_special_chars() {
        let v = p("a: ${body} and {{prop}}\nb: -x\nc: ?y\nd: x[0]\n");
        assert_eq!(v.get("a").unwrap(), &s("${body} and {{prop}}"));
        assert_eq!(v.get("b").unwrap(), &s("-x"));
        assert_eq!(v.get("c").unwrap(), &s("?y"));
        assert_eq!(v.get("d").unwrap(), &s("x[0]"));
    }

    #[test]
    fn multi_line_plain_scalar() {
        let v = p("key: this is\n  a long\n\n  value\nnext: 1\n");
        assert_eq!(v.get("key").unwrap(), &s("this is a long\nvalue"));
        assert_eq!(v.get("next").unwrap(), &Value::Int(1));
    }

    #[test]
    fn multi_line_plain_in_sequence() {
        assert_eq!(p("- one\n  two\n- three\n"), arr(vec![s("one two"), s("three")]));
    }

    #[test]
    fn plain_scalar_stops_at_comment_line() {
        let v = p("a: x\n  # comment\nb: y\n");
        assert_eq!(v, obj(vec![("a", s("x")), ("b", s("y"))]));
    }

    #[test]
    fn single_quoted() {
        let v = p("a: 'it''s'\nb: 'x: y # z'\nc: ''\nd: '  padded  '\n");
        assert_eq!(v.get("a").unwrap(), &s("it's"));
        assert_eq!(v.get("b").unwrap(), &s("x: y # z"));
        assert_eq!(v.get("c").unwrap(), &s(""));
        assert_eq!(v.get("d").unwrap(), &s("  padded  "));
    }

    #[test]
    fn single_quoted_multiline_folding() {
        assert_eq!(p("'first\n  second\n\n  third'"), s("first second\nthird"));
    }

    #[test]
    fn double_quoted_escapes() {
        let v = p(r#"a: "tab\tnl\nquote\"bs\\ slash\/ \x41\u00e9\U0001F600 \0end""#);
        assert_eq!(v.get("a").unwrap(), &s("tab\tnl\nquote\"bs\\ slash/ Aé😀 \0end"));
    }

    #[test]
    fn double_quoted_multiline() {
        assert_eq!(p("\"a\n  b\n\n  c\""), s("a b\nc"));
        assert_eq!(p("\"abc\\\n   def\""), s("abcdef"));
        assert_eq!(p("\"trailing   \n  next\""), s("trailing next"));
        assert_eq!(p("\"esc\\ \n  next\""), s("esc  next"));
    }

    #[test]
    fn double_quoted_invalid_escape() {
        let e = err("a: \"bad \\q\"");
        assert!(e.message.contains("invalid escape"), "{}", e);
        assert_eq!(e.line, 1);
        assert_eq!(e.column, 9);
    }

    #[test]
    fn quoted_values_are_strings() {
        let v = p("a: \"true\"\nb: '123'\nc: \"null\"\nd: \"\"\n");
        assert_eq!(v, obj(vec![("a", s("true")), ("b", s("123")), ("c", s("null")), ("d", s(""))]));
    }

    #[test]
    fn quoted_keys() {
        let v = p("\"a key\": 1\n'b: c': 2\n\"\": 3\n");
        assert_eq!(v, obj(vec![("a key", Value::Int(1)), ("b: c", Value::Int(2)), ("", Value::Int(3))]));
    }

    #[test]
    fn numeric_and_bool_keys_are_strings() {
        let v = p("1: one\ntrue: yes\n");
        assert_eq!(v, obj(vec![("1", s("one")), ("true", s("yes"))]));
    }

    #[test]
    fn key_with_space_before_colon() {
        assert_eq!(p("a : 1\n"), obj(vec![("a", Value::Int(1))]));
    }

    #[test]
    fn literal_block_scalar() {
        let v = p("a: |\n  line1\n  line2\n\n  line4\nb: x\n");
        assert_eq!(v.get("a").unwrap(), &s("line1\nline2\n\nline4\n"));
        assert_eq!(v.get("b").unwrap(), &s("x"));
    }

    #[test]
    fn literal_preserves_relative_indentation() {
        let v = p("code: |\n  if x:\n      y()\n  z\n");
        assert_eq!(v.get("code").unwrap(), &s("if x:\n    y()\nz\n"));
    }

    #[test]
    fn literal_chomping() {
        let src = "strip: |-\n  text\n\n\nclip: |\n  text\n\n\nkeep: |+\n  text\n\n\nend: 1\n";
        let v = p(src);
        assert_eq!(v.get("strip").unwrap(), &s("text"));
        assert_eq!(v.get("clip").unwrap(), &s("text\n"));
        assert_eq!(v.get("keep").unwrap(), &s("text\n\n\n"));
    }

    #[test]
    fn keep_chomping_at_end_of_stream() {
        assert_eq!(p("a: |+\n  x\n\n"), obj(vec![("a", s("x\n\n"))]));
        assert_eq!(p("a: |\n  x"), obj(vec![("a", s("x\n"))]));
        assert_eq!(p("a: |-\n  x\n"), obj(vec![("a", s("x"))]));
    }

    #[test]
    fn folded_block_scalar() {
        let v = p("a: >\n  folded\n  text\n\n  next para\n    more indented\n  back\n");
        assert_eq!(v.get("a").unwrap(), &s("folded text\nnext para\n  more indented\nback\n"));
    }

    #[test]
    fn folded_strip() {
        assert_eq!(p("a: >-\n  one\n  two\n"), obj(vec![("a", s("one two"))]));
    }

    #[test]
    fn block_scalar_explicit_indent() {
        let v = p("a: |2\n    indented\n  base\n");
        assert_eq!(v.get("a").unwrap(), &s("  indented\nbase\n"));
        let v = p("- |1\n  explicit\n");
        assert_eq!(v, arr(vec![s(" explicit\n")]));
        let v = p("a: |-2\n   x\n");
        assert_eq!(v.get("a").unwrap(), &s(" x"));
        let v = p("a: |2-\n   x\n");
        assert_eq!(v.get("a").unwrap(), &s(" x"));
    }

    #[test]
    fn block_scalar_with_comment_header_and_leading_blank_lines() {
        let v = p("a: | # comment\n\n  text\nb: 1\n");
        assert_eq!(v.get("a").unwrap(), &s("\ntext\n"));
    }

    #[test]
    fn block_scalar_in_sequence_and_empty() {
        let v = p("- |\n  a\n  b\n- >\n  c\n  d\n- |\n- x\n");
        assert_eq!(v, arr(vec![s("a\nb\n"), s("c d\n"), s(""), s("x")]));
    }

    #[test]
    fn block_scalar_keeps_hash() {
        let v = p("a: |\n  # not a comment\n  x\n");
        assert_eq!(v.get("a").unwrap(), &s("# not a comment\nx\n"));
    }

    #[test]
    fn flow_sequence() {
        assert_eq!(p("[a, b, 1, true, null, \"q\"]"), arr(vec![s("a"), s("b"), Value::Int(1), Value::Bool(true), Value::Null, s("q")]));
        assert_eq!(p("[]"), arr(vec![]));
        assert_eq!(p("[a, b, ]"), arr(vec![s("a"), s("b")]));
    }

    #[test]
    fn flow_mapping() {
        let v = p("{a: 1, b: [x, y], c: {d: e}, f}");
        assert_eq!(
            v,
            obj(vec![("a", Value::Int(1)), ("b", arr(vec![s("x"), s("y")])), ("c", obj(vec![("d", s("e"))])), ("f", Value::Null)])
        );
        assert_eq!(p("{}"), obj(vec![]));
    }

    #[test]
    fn flow_json_style() {
        let v = p(r#"{"a":1,"b":[true,false],"c":{"d":"e"}}"#);
        assert_eq!(
            v,
            obj(vec![("a", Value::Int(1)), ("b", arr(vec![Value::Bool(true), Value::Bool(false)])), ("c", obj(vec![("d", s("e"))]))])
        );
    }

    #[test]
    fn flow_multiline_and_comments() {
        let v = p("a: {\n  x: 1, # comment\n  y: [\n    p,\n    q\n  ]\n}\nb: 2\n");
        assert_eq!(v, obj(vec![("a", obj(vec![("x", Value::Int(1)), ("y", arr(vec![s("p"), s("q")]))])), ("b", Value::Int(2))]));
    }

    #[test]
    fn flow_plain_with_colons() {
        let v = p("[http://a.b/c, direct:start, a:b]");
        assert_eq!(v, arr(vec![s("http://a.b/c"), s("direct:start"), s("a:b")]));
    }

    #[test]
    fn flow_seq_single_pair() {
        assert_eq!(p("[a: 1, b]"), arr(vec![obj(vec![("a", Value::Int(1))]), s("b")]));
    }

    #[test]
    fn flow_errors() {
        let e = err("a: [1, 2");
        assert!(e.message.contains("unclosed flow sequence"), "{}", e);
        assert_eq!((e.line, e.column), (1, 4));
        let e = err("a: {x: 1]}");
        assert!(e.message.contains("expected ',' or '}'"), "{}", e);
        assert_eq!((e.line, e.column), (1, 9));
        let e = err("a: {x: 1\n");
        assert!(e.message.contains("unclosed flow mapping"), "{}", e);
    }

    #[test]
    fn comments_everywhere() {
        let v = p("# head\na: 1 # trailing\n# between\nb:   # after key\n  - x # item\n  # inside\n  - y\n");
        assert_eq!(v, obj(vec![("a", Value::Int(1)), ("b", arr(vec![s("x"), s("y")]))]));
    }

    #[test]
    fn multi_documents() {
        let docs = parse_all("a: 1\n---\nb: 2\n...\n---\n- x\n").unwrap();
        assert_eq!(docs, vec![obj(vec![("a", Value::Int(1))]), obj(vec![("b", Value::Int(2))]), arr(vec![s("x")])]);
    }

    #[test]
    fn document_with_directive_and_inline_content() {
        let docs = parse_all("%YAML 1.2\n--- text\n--- |\n  block\n---\n").unwrap();
        assert_eq!(docs, vec![s("text"), s("block\n"), Value::Null]);
    }

    #[test]
    fn parse_rejects_multiple_documents() {
        let e = err("a: 1\n---\nb: 2\n");
        assert!(e.message.contains("single document"));
        assert_eq!(e.line, 3);
    }

    #[test]
    fn anchors_and_aliases() {
        let v = p("base: &b\n  x: 1\ncopy: *b\nlist:\n  - &s hello\n  - *s\n");
        assert_eq!(v.get("copy").unwrap(), &obj(vec![("x", Value::Int(1))]));
        assert_eq!(v.get("list").unwrap(), &arr(vec![s("hello"), s("hello")]));
    }

    #[test]
    fn merge_keys() {
        let src = "defaults: &d\n  a: 1\n  b: 2\nother: &o\n  c: 3\n  a: 9\nm:\n  <<: *d\n  b: 20\nmulti:\n  <<: [*d, *o]\n";
        let v = p(src);
        assert_eq!(v.get("m").unwrap(), &obj(vec![("a", Value::Int(1)), ("b", Value::Int(20))]));
        assert_eq!(v.get("multi").unwrap(), &obj(vec![("a", Value::Int(1)), ("b", Value::Int(2)), ("c", Value::Int(3))]));
        // SnakeYAML order: merged keys spliced in at the `<<`, explicit keys override in place
        let v = p("d: &d\n  y: 1\n  x: 2\nm:\n  z: 0\n  <<: *d\n  x: 20\n  w: 3\n");
        assert_eq!(v.get("m").unwrap().to_json(), r#"{"z":0,"y":1,"x":20,"w":3}"#);
    }

    #[test]
    fn mappings_keep_source_order() {
        let src = "zebra: 1\napple:\n  second: b\n  first: a\nmango: [3, 2]\n";
        let v = p(src);
        assert_eq!(v.to_json(), r#"{"zebra":1,"apple":{"second":"b","first":"a"},"mango":[3,2]}"#);
        assert_eq!(to_yaml(&v), "zebra: 1\napple:\n  second: b\n  first: a\nmango:\n  - 3\n  - 2\n");
        assert_eq!(p(&to_yaml(&v)).to_json(), v.to_json());
    }

    #[test]
    fn merge_key_invalid() {
        let e = err("m:\n  <<: 5\n");
        assert!(e.message.contains("merge key"), "{}", e);
    }

    #[test]
    fn unknown_alias_error() {
        let e = err("a: *nope\n");
        assert!(e.message.contains("unknown alias '*nope'"));
        assert_eq!((e.line, e.column), (1, 4));
    }

    #[test]
    fn anchor_in_flow() {
        assert_eq!(p("[&a x, *a]"), arr(vec![s("x"), s("x")]));
    }

    #[test]
    fn anchor_on_seq_entry_mapping() {
        let v = p("- &item\n  name: a\n- *item\n");
        assert_eq!(v, arr(vec![obj(vec![("name", s("a"))]), obj(vec![("name", s("a"))])]));
    }

    #[test]
    fn billion_laughs_is_limited() {
        let mut src = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        let mut prev = 'a';
        for c in "bcdefghij".chars() {
            src.push_str(&format!("{c}: &{c} [*{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}]\n", c = c, p = prev));
            prev = c;
        }
        let e = err(&src);
        assert!(e.message.contains("alias"), "{}", e);
    }

    #[test]
    fn tags() {
        let v = p("a: !!str 123\nb: !!int \"42\"\nc: !!float 1\nd: !!bool \"true\"\ne: !!null ''\nf: !custom 7\ng: !!binary aGVsbG8=\nh: ! 12\n");
        assert_eq!(v.get("a").unwrap(), &s("123"));
        assert_eq!(v.get("b").unwrap(), &Value::Int(42));
        assert_eq!(v.get("c").unwrap(), &Value::Float(1.0));
        assert_eq!(v.get("d").unwrap(), &Value::Bool(true));
        assert_eq!(v.get("e").unwrap(), &Value::Null);
        assert_eq!(v.get("f").unwrap(), &Value::Int(7));
        assert_eq!(v.get("g").unwrap(), &Value::Bytes(b"hello".to_vec()));
        assert_eq!(v.get("h").unwrap(), &s("12"));
    }

    #[test]
    fn tag_on_collection_and_verbatim() {
        let v = p("a: !!map\n  x: 1\nb: !<tag:yaml.org,2002:str> 5\nc: !!seq\n- 1\n");
        assert_eq!(v.get("a").unwrap(), &obj(vec![("x", Value::Int(1))]));
        assert_eq!(v.get("b").unwrap(), &s("5"));
        assert_eq!(v.get("c").unwrap(), &arr(vec![Value::Int(1)]));
    }

    #[test]
    fn invalid_tagged_int() {
        let e = err("a: !!int abc\n");
        assert!(e.message.contains("invalid !!int"), "{}", e);
        assert_eq!(e.line, 1);
    }

    #[test]
    fn explicit_keys() {
        let v = p("? complex key\n: value\n? another\n");
        assert_eq!(v, obj(vec![("complex key", s("value")), ("another", Value::Null)]));
    }

    #[test]
    fn flow_key_as_complex_key() {
        let v = p("[a, b]: pair\n");
        assert_eq!(v, obj(vec![("[\"a\",\"b\"]", s("pair"))]));
    }

    #[test]
    fn duplicate_key_error() {
        let e = err("a: 1\nb: 2\na: 3\n");
        assert!(e.message.contains("duplicate mapping key 'a'"), "{}", e);
        assert_eq!((e.line, e.column), (3, 1));
    }

    #[test]
    fn bad_indentation_errors() {
        let e = err("a:\n  b: 1\n   c: 2\n");
        assert!(e.message.contains("mapping values are not allowed"), "{}", e);
        assert_eq!(e.line, 3);
        let e = err("a:\n  b:\n    c: 1\n   d: 2\n");
        assert!(e.message.contains("bad indentation of a mapping entry"), "{}", e);
        assert_eq!((e.line, e.column), (4, 4));
        let e = err("- a:\n    - x\n  - y\n");
        assert!(e.message.contains("sequence entry"), "{}", e);
        assert_eq!(e.line, 3);
        let e = err("- a\nb: 1\n");
        assert!(e.message.contains("unexpected content"), "{}", e);
    }

    #[test]
    fn dash_continuation_is_plain_text() {
        // a more indented "- b" continues the plain scalar (as in other YAML parsers)
        assert_eq!(p("- a\n  - b\n"), arr(vec![s("a - b")]));
    }

    #[test]
    fn mapping_value_not_allowed_inline() {
        let e = err("a: b: c\n");
        assert!(e.message.contains("mapping values are not allowed"), "{}", e);
        assert_eq!((e.line, e.column), (1, 5));
    }

    #[test]
    fn missing_colon_error() {
        let e = err("a: 1\nb\nc: 2\n");
        assert!(e.message.contains("':'"), "{}", e);
        assert_eq!(e.line, 2);
    }

    #[test]
    fn tab_indentation_error() {
        let e = err("a:\n\tb: 1\n");
        assert!(e.message.contains("tab"), "{}", e);
        assert_eq!(e.line, 2);
    }

    #[test]
    fn tabs_as_separators_are_fine() {
        assert_eq!(p("a:\t1\nb: x\t# c\n"), obj(vec![("a", Value::Int(1)), ("b", s("x"))]));
    }

    #[test]
    fn unterminated_quote_error() {
        let e = err("a: 'abc\nb: 1\n");
        assert!(e.message.contains("unterminated single-quoted"), "{}", e);
        assert_eq!((e.line, e.column), (1, 4));
        let e = err("a: \"abc");
        assert!(e.message.contains("unterminated double-quoted"), "{}", e);
    }

    #[test]
    fn sequence_inline_after_key_error() {
        let e = err("a: - b\n");
        assert!(e.message.contains("sequence"), "{}", e);
    }

    #[test]
    fn content_after_document_error() {
        let e = err("a: 1\n  b: 2\n");
        assert!(e.message.contains("indentation") || e.message.contains("mapping"), "{}", e);
        assert_eq!(e.line, 2);
    }

    #[test]
    fn reserved_character_error() {
        let e = err("a: @x\n");
        assert!(e.message.contains("reserved"), "{}", e);
    }

    #[test]
    fn crlf_and_bom() {
        assert_eq!(p("\u{feff}a: 1\r\nb: 2\r\n"), obj(vec![("a", Value::Int(1)), ("b", Value::Int(2))]));
    }

    #[test]
    fn deep_indentation_mix() {
        let src = "
root:
    level1:
          - name: x
            items:
               - 1
               - 2
          - name: y
            items: []
    sibling: z
";
        let v = p(src);
        let l1 = v.get("root").unwrap().get("level1").unwrap();
        assert_eq!(l1.get("0").unwrap().get("items").unwrap(), &arr(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(l1.get("1").unwrap().get("items").unwrap(), &arr(vec![]));
        assert_eq!(v.get("root").unwrap().get("sibling").unwrap(), &s("z"));
    }

    #[test]
    fn indented_top_level() {
        assert_eq!(p("  a: 1\n  b: 2\n"), obj(vec![("a", Value::Int(1)), ("b", Value::Int(2))]));
    }

    #[test]
    fn value_on_next_line_scalar() {
        let v = p("a:\n  hello\n  world\nb: 1");
        assert_eq!(v, obj(vec![("a", s("hello world")), ("b", Value::Int(1))]));
    }

    #[test]
    fn seq_entry_with_properties_and_value() {
        let v = p("- !!str\n  42\n- &x 5\n- *x\n");
        assert_eq!(v, arr(vec![s("42"), Value::Int(5), Value::Int(5)]));
    }

    #[test]
    fn nested_seq_in_compact_map_in_seq() {
        let v = p("- a:\n  - 1\n  - 2\n  b: 3\n");
        assert_eq!(v, arr(vec![obj(vec![("a", arr(vec![Value::Int(1), Value::Int(2)])), ("b", Value::Int(3))])]));
    }

    #[test]
    fn node_positions() {
        let docs = parse_nodes("a:\n  b:\n    - x\n    - y: 1\n").unwrap();
        let n = docs[0].lookup(&[PathSegment::Key("a".into()), PathSegment::Key("b".into()), PathSegment::Index(1)]);
        assert_eq!((n.line, n.column), (4, 7));
        let y = n.get("y").unwrap();
        assert_eq!((y.line, y.column), (4, 10));
    }

    #[test]
    fn unicode_content() {
        let v = p("greeting: héllo wörld ✓\n\"ключ\": значение\n");
        assert_eq!(v.get("greeting").unwrap(), &s("héllo wörld ✓"));
        assert_eq!(v.get("ключ").unwrap(), &s("значение"));
    }

    #[test]
    fn document_end_marker_terminates_block() {
        let docs = parse_all("a: |\n  text\n...\n").unwrap();
        assert_eq!(docs, vec![obj(vec![("a", s("text\n"))])]);
    }

    #[test]
    fn compact_map_with_nested_map_first() {
        let v = p("- a:\n    b: 1\n  c: 2\n- d\n");
        assert_eq!(v, arr(vec![obj(vec![("a", obj(vec![("b", Value::Int(1))])), ("c", Value::Int(2))]), s("d")]));
    }

    #[test]
    fn folded_in_compact_map() {
        let v = p("- msg: >\n    a\n    b\n  next: 1\n");
        assert_eq!(v, arr(vec![obj(vec![("msg", s("a b\n")), ("next", Value::Int(1))])]));
    }

    #[test]
    fn literal_with_overindented_blank_line() {
        let v = p("a: |\n  x\n    \n  y\n");
        assert_eq!(v.get("a").unwrap(), &s("x\n  \ny\n"));
    }

    #[test]
    fn doc_start_with_comment_and_flow_items() {
        let v = p("--- # comment\n- {a: 1}\n- [b, c]\n- \"k\": {x: y}\n");
        assert_eq!(
            v,
            arr(vec![obj(vec![("a", Value::Int(1))]), arr(vec![s("b"), s("c")]), obj(vec![("k", obj(vec![("x", s("y"))]))])])
        );
    }

    #[test]
    fn multi_line_plain_in_flow() {
        assert_eq!(p("[a\n b, c]"), arr(vec![s("a b"), s("c")]));
    }

    #[test]
    fn top_level_multi_line_plain() {
        assert_eq!(p("just some\ntext here\n"), s("just some text here"));
    }

    #[test]
    fn junk_after_flow_collection() {
        let e = err("key: [a, b] extra\n");
        assert!(e.message.contains("unexpected character 'e'"), "{}", e);
        assert_eq!((e.line, e.column), (1, 13));
    }

    #[test]
    fn tags_on_empty_values() {
        let v = p("a: !!str\nb: !!null\nc: !!map\nd: !!seq\n");
        assert_eq!(v, obj(vec![("a", s("")), ("b", Value::Null), ("c", obj(vec![])), ("d", arr(vec![]))]));
    }

    #[test]
    fn empty_value_followed_by_dedent_and_seq() {
        let v = p("a:\n  b:\n  c:\n    - 1\nd:\n");
        assert_eq!(v, obj(vec![("a", obj(vec![("b", Value::Null), ("c", arr(vec![Value::Int(1)]))])), ("d", Value::Null)]));
    }

    #[test]
    fn windows_paths_and_quotes_in_plain() {
        let v = p("path: C:\\temp\\x\nq: it's \"fine\"\n");
        assert_eq!(v.get("path").unwrap(), &s("C:\\temp\\x"));
        assert_eq!(v.get("q").unwrap(), &s("it's \"fine\""));
    }

    #[test]
    fn camel_rest_dsl_shape() {
        let src = "- rest:\n    path: /api\n    get:\n      - path: /hello\n        to: direct:hello\n      - path: /bye\n        to: direct:bye\n";
        let v = p(src);
        let gets = v.get("0").unwrap().get("rest").unwrap().get("get").unwrap();
        assert_eq!(gets.as_array().unwrap().len(), 2);
        assert_eq!(gets.get("1").unwrap().get("to").unwrap(), &s("direct:bye"));
    }

    // --- emitter ---

    fn roundtrip(v: &Value) {
        let y = to_yaml(v);
        let back = parse(&y).unwrap_or_else(|e| panic!("re-parse failed: {}\n{}", e, y));
        assert_eq!(&back, v, "yaml was:\n{}", y);
    }

    #[test]
    fn emit_simple() {
        let v = obj(vec![("a", Value::Int(1)), ("b", s("x")), ("c", arr(vec![s("p"), Value::Bool(true)]))]);
        assert_eq!(to_yaml(&v), "a: 1\nb: x\nc:\n  - p\n  - true\n");
        roundtrip(&v);
    }

    #[test]
    fn emit_seq_of_maps() {
        let v = arr(vec![obj(vec![("route", obj(vec![("id", s("r1")), ("from", obj(vec![("uri", s("direct:a"))]))]))])]);
        assert_eq!(to_yaml(&v), "- route:\n    id: r1\n    from:\n      uri: direct:a\n");
        roundtrip(&v);
    }

    #[test]
    fn emit_quotes_ambiguous_strings() {
        let v = arr(vec![
            s("true"),
            s("123"),
            s(""),
            s("null"),
            s("a: b"),
            s("- x"),
            s(" lead"),
            s("trail "),
            s("#c"),
            s("x #y"),
            s("{{p}}"),
            s("${body}"),
            s("tab\there"),
            s("1.5"),
            s("..."),
            s("ctl\u{1}"),
        ]);
        roundtrip(&v);
        let y = to_yaml(&v);
        assert!(y.contains("- \"true\"\n"));
        assert!(y.contains("- ${body}\n"));
    }

    #[test]
    fn emit_multiline_strings() {
        let v = obj(vec![
            ("clip", s("a\nb\n")),
            ("strip", s("a\nb")),
            ("keep", s("a\n\n")),
            ("indented", s("  x\ny\n")),
            ("blank_inside", s("a\n\nb\n")),
            ("only_nl", s("\n")),
        ]);
        roundtrip(&v);
        let y = to_yaml(&v);
        assert!(y.contains("clip: |\n  a\n  b\n"), "{}", y);
        assert!(y.contains("strip: |-\n"), "{}", y);
        assert!(y.contains("keep: |+\n"), "{}", y);
    }

    #[test]
    fn emit_multiline_in_sequence() {
        roundtrip(&arr(vec![s("line1\nline2\n"), obj(vec![("k", s("x\ny"))])]));
    }

    #[test]
    fn emit_numbers_and_special() {
        let v = arr(vec![
            Value::Float(1.0),
            Value::Float(-0.25),
            Value::Float(1e20),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Int(-5),
            Value::Null,
            Value::Bytes(b"hi there".to_vec()),
        ]);
        roundtrip(&v);
        assert!(to_yaml(&v).contains("- 1.0\n"));
    }

    #[test]
    fn emit_nested_empty_and_seq_of_seq() {
        let v = obj(vec![("e", arr(vec![])), ("m", obj(vec![])), ("ss", arr(vec![arr(vec![Value::Int(1), Value::Int(2)]), arr(vec![])]))]);
        roundtrip(&v);
    }

    #[test]
    fn emit_special_keys() {
        let v = obj(vec![("<<", Value::Int(1)), ("a key: x", Value::Int(2)), ("123", Value::Int(3)), ("", Value::Int(4))]);
        roundtrip(&v);
    }

    #[test]
    fn emit_scalars_top_level() {
        assert_eq!(to_yaml(&s("x")), "x\n");
        assert_eq!(to_yaml(&Value::Null), "null\n");
        roundtrip(&s("multi\nline"));
        assert_eq!(to_yaml(&arr(vec![])), "[]\n");
    }

    #[test]
    fn base64_roundtrip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"\x00\xff\x10"] {
            assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
        }
    }
}
