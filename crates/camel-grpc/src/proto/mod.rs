//! Protocol Buffers support: a `.proto` parser ([`parser`]), runtime descriptors
//! ([`DescriptorPool`]), a dynamic wire-format codec between protobuf bytes and
//! [`camel_core::value::Value`] ([`codec`]) and the proto3 JSON mapping ([`json`]).
//!
//! ```
//! use camel_grpc::proto::DescriptorPool;
//! use camel_core::value::Value;
//! let pool = DescriptorPool::from_source("t.proto", r#"
//!     syntax = "proto3";
//!     package demo;
//!     message Test1 { int32 a = 1; }
//! "#).unwrap();
//! let bytes = pool.encode("demo.Test1", &Value::object([("a", 150)])).unwrap();
//! assert_eq!(bytes, [0x08, 0x96, 0x01]);
//! assert_eq!(pool.decode("demo.Test1", &bytes).unwrap(), Value::object([("a", 150)]));
//! ```

pub mod codec;
pub mod json;
pub mod parser;
mod wkt;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::value::Value;

/// `syntax` of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    Proto2,
    Proto3,
}

/// Scalar and composite field types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Double,
    Float,
    Int64,
    Uint64,
    Int32,
    Fixed64,
    Fixed32,
    Bool,
    String,
    Bytes,
    Uint32,
    Sfixed32,
    Sfixed64,
    Sint32,
    Sint64,
    Enum,
    Message,
}

impl FieldType {
    /// Parses a scalar type keyword.
    pub fn scalar(name: &str) -> Option<FieldType> {
        Some(match name {
            "double" => FieldType::Double,
            "float" => FieldType::Float,
            "int64" => FieldType::Int64,
            "uint64" => FieldType::Uint64,
            "int32" => FieldType::Int32,
            "fixed64" => FieldType::Fixed64,
            "fixed32" => FieldType::Fixed32,
            "bool" => FieldType::Bool,
            "string" => FieldType::String,
            "bytes" => FieldType::Bytes,
            "uint32" => FieldType::Uint32,
            "sfixed32" => FieldType::Sfixed32,
            "sfixed64" => FieldType::Sfixed64,
            "sint32" => FieldType::Sint32,
            "sint64" => FieldType::Sint64,
            _ => return None,
        })
    }

    /// Whether repeated fields of this type may be packed.
    pub fn is_packable(&self) -> bool {
        !matches!(self, FieldType::String | FieldType::Bytes | FieldType::Message)
    }

    /// The wire type used for a single value.
    pub fn wire_type(&self) -> u8 {
        match self {
            FieldType::Double | FieldType::Fixed64 | FieldType::Sfixed64 => 1,
            FieldType::Float | FieldType::Fixed32 | FieldType::Sfixed32 => 5,
            FieldType::String | FieldType::Bytes | FieldType::Message => 2,
            _ => 0,
        }
    }
}

/// Field cardinality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// proto3 singular field without `optional` (implicit presence).
    Implicit,
    /// `optional` (explicit presence; proto2 or proto3 `optional`).
    Optional,
    /// proto2 `required`.
    Required,
    Repeated,
}

/// A message field.
#[derive(Clone, Debug)]
pub struct FieldDescriptor {
    pub name: String,
    pub json_name: String,
    pub number: u32,
    pub label: Label,
    pub field_type: FieldType,
    /// Fully-qualified message/enum type name (without leading dot) for Message/Enum fields.
    pub type_name: String,
    /// Index into [`MessageDescriptor::oneofs`].
    pub oneof_index: Option<usize>,
    /// `[packed = ...]` when given.
    pub packed: Option<bool>,
    /// proto2 `[default = ...]` (raw text).
    pub default_value: Option<String>,
    /// True for `map<K, V>` fields (the type is a synthesized `...Entry` message).
    pub is_map: bool,
}

impl FieldDescriptor {
    pub fn is_repeated(&self) -> bool {
        self.label == Label::Repeated
    }
    /// Whether presence is tracked (absent vs default value).
    pub fn has_presence(&self) -> bool {
        match self.label {
            Label::Repeated => false,
            Label::Implicit => self.field_type == FieldType::Message || self.oneof_index.is_some(),
            _ => true,
        }
    }
}

/// A message type.
#[derive(Clone, Debug)]
pub struct MessageDescriptor {
    pub name: String,
    pub full_name: String,
    pub syntax: Syntax,
    pub fields: Vec<FieldDescriptor>,
    pub oneofs: Vec<String>,
    /// Synthesized map entry type.
    pub is_map_entry: bool,
}

impl MessageDescriptor {
    pub fn field(&self, name: &str) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|f| f.name == name).or_else(|| self.fields.iter().find(|f| f.json_name == name))
    }
    pub fn field_by_number(&self, n: u32) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|f| f.number == n)
    }
}

/// An enum type.
#[derive(Clone, Debug)]
pub struct EnumDescriptor {
    pub name: String,
    pub full_name: String,
    pub values: Vec<(String, i32)>,
}

impl EnumDescriptor {
    pub fn name_of(&self, n: i32) -> Option<&str> {
        self.values.iter().find(|(_, v)| *v == n).map(|(k, _)| k.as_str())
    }
    pub fn number_of(&self, name: &str) -> Option<i32> {
        self.values.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
    }
    pub fn default_value(&self) -> (String, i32) {
        self.values.first().cloned().unwrap_or_else(|| ("0".into(), 0))
    }
}

/// An rpc method.
#[derive(Clone, Debug)]
pub struct MethodDescriptor {
    pub name: String,
    pub input_type: String,
    pub output_type: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

impl MethodDescriptor {
    /// `unary`, `server_streaming`, `client_streaming` or `bidi_streaming`.
    pub fn kind(&self) -> &'static str {
        match (self.client_streaming, self.server_streaming) {
            (false, false) => "unary",
            (false, true) => "server_streaming",
            (true, false) => "client_streaming",
            (true, true) => "bidi_streaming",
        }
    }
}

/// A service.
#[derive(Clone, Debug)]
pub struct ServiceDescriptor {
    pub name: String,
    pub full_name: String,
    pub methods: Vec<MethodDescriptor>,
}

impl ServiceDescriptor {
    /// Finds a method by its proto name (`SayHello`) or lowerCamel stub name (`sayHello`).
    pub fn method(&self, name: &str) -> Option<&MethodDescriptor> {
        self.methods.iter().find(|m| m.name == name).or_else(|| self.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
    }
}

/// Converts `snake_case` to the protobuf JSON name (`lowerCamelCase`).
pub fn to_json_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Converts an rpc name to the lowerCamel method name used by generated stubs (`SayHello` -> `sayHello`).
pub fn to_lower_camel(name: &str) -> String {
    let mut c = name.chars();
    match c.next() {
        Some(f) => f.to_ascii_lowercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

/// Error raised by the parser or resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtoError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}: {}", self.file, self.line, self.message)
        } else {
            write!(f, "{}: {}", self.file, self.message)
        }
    }
}

impl From<ProtoError> for CamelError {
    fn from(e: ProtoError) -> Self {
        CamelError::new("ProtoParseException", e.to_string())
    }
}

/// A set of loaded `.proto` files with all their types resolved.
#[derive(Clone, Debug, Default)]
pub struct DescriptorPool {
    pub messages: HashMap<String, Arc<MessageDescriptor>>,
    pub enums: HashMap<String, Arc<EnumDescriptor>>,
    pub services: HashMap<String, Arc<ServiceDescriptor>>,
    /// Loaded file names.
    pub files: Vec<String>,
}

impl DescriptorPool {
    /// An empty pool containing only the built-in well-known types.
    pub fn new() -> Self {
        let mut pool = DescriptorPool::default();
        for (name, src) in wkt::SOURCES {
            let f = parser::parse(name, src).expect("built-in proto parses");
            pool.add_parsed(vec![f]).expect("built-in proto resolves");
        }
        pool.files.clear();
        pool
    }

    /// Parses a single file from source text (imports other than the built-in
    /// `google/protobuf/*.proto` are not available).
    pub fn from_source(name: &str, src: &str) -> std::result::Result<DescriptorPool, ProtoError> {
        let mut pool = DescriptorPool::new();
        let f = parser::parse(name, src)?;
        pool.add_parsed(vec![f])?;
        pool.files.push(name.to_string());
        Ok(pool)
    }

    /// Loads a `.proto` file and (recursively) its imports, resolved relative to the
    /// importing file's directory.
    pub fn from_file(path: impl AsRef<Path>) -> std::result::Result<DescriptorPool, ProtoError> {
        let mut pool = DescriptorPool::new();
        pool.load_file(path)?;
        Ok(pool)
    }

    /// Loads another file (and its imports) into this pool.
    pub fn load_file(&mut self, path: impl AsRef<Path>) -> std::result::Result<(), ProtoError> {
        let path = path.as_ref();
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut parsed = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();
        self.collect(path, &dir, &mut parsed, &mut seen)?;
        let names: Vec<String> = parsed.iter().map(|f: &parser::ParsedFile| f.name.clone()).collect();
        self.add_parsed(parsed)?;
        self.files.extend(names);
        Ok(())
    }

    fn collect(&self, path: &Path, root: &Path, out: &mut Vec<parser::ParsedFile>, seen: &mut Vec<PathBuf>) -> std::result::Result<(), ProtoError> {
        if seen.iter().any(|p| p == path) {
            return Ok(());
        }
        seen.push(path.to_path_buf());
        let name = path.display().to_string();
        let src = std::fs::read_to_string(path).map_err(|e| ProtoError { file: name.clone(), line: 0, message: format!("cannot read: {}", e) })?;
        let f = parser::parse(&name, &src)?;
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for imp in &f.imports {
            if wkt::SOURCES.iter().any(|(n, _)| n == imp) {
                continue;
            }
            // imports are resolved against the importing file's directory, then the root
            let candidates = [dir.join(imp), root.join(imp), dir.join(Path::new(imp).file_name().unwrap_or_default())];
            match candidates.iter().find(|c| c.is_file()) {
                Some(c) => self.collect(c, root, out, seen)?,
                None => return Err(ProtoError { file: name.clone(), line: 0, message: format!("import \"{}\" not found (searched {})", imp, dir.display()) }),
            }
        }
        out.push(f);
        Ok(())
    }

    fn add_parsed(&mut self, files: Vec<parser::ParsedFile>) -> std::result::Result<(), ProtoError> {
        // register all type names first, then resolve references
        let mut msgs = Vec::new();
        for f in &files {
            for e in &f.enums {
                self.enums.insert(e.full_name.clone(), Arc::new(e.clone()));
            }
            for m in &f.messages {
                msgs.push((f.name.clone(), m.clone()));
            }
        }
        let known_msgs: Vec<String> = self.messages.keys().cloned().chain(msgs.iter().map(|(_, m)| m.desc.full_name.clone())).collect();
        let is_msg = |n: &str| known_msgs.iter().any(|k| k == n);
        for (file, m) in msgs {
            let mut desc = m.desc.clone();
            for (field, (raw, line)) in desc.fields.iter_mut().zip(m.raw_types.iter()) {
                if let Some(raw) = raw {
                    let full = self.resolve_name(raw, &m.scope, &is_msg).ok_or_else(|| ProtoError {
                        file: file.clone(),
                        line: *line,
                        message: format!("unknown type \"{}\" for field {}.{}", raw, desc.full_name, field.name),
                    })?;
                    field.field_type = if is_msg(&full) { FieldType::Message } else { FieldType::Enum };
                    if field.field_type == FieldType::Enum && field.packed.is_none() && desc.syntax == Syntax::Proto3 && field.is_repeated() {
                        field.packed = Some(true);
                    }
                    field.type_name = full;
                }
            }
            self.messages.insert(desc.full_name.clone(), Arc::new(desc));
        }
        for f in files {
            for s in f.services {
                let mut svc = s.desc.clone();
                for m in svc.methods.iter_mut() {
                    for t in [&mut m.input_type, &mut m.output_type] {
                        let full = self.resolve_name(t, &s.scope, &is_msg).filter(|n| is_msg(n)).ok_or_else(|| ProtoError {
                            file: f.name.clone(),
                            line: s.line,
                            message: format!("unknown message type \"{}\" in service {}", t, s.desc.full_name),
                        })?;
                        *t = full;
                    }
                }
                self.services.insert(svc.full_name.clone(), Arc::new(svc));
            }
        }
        Ok(())
    }

    /// Resolves a (possibly relative) type reference from `scope` (e.g. `pkg.Outer`).
    fn resolve_name(&self, name: &str, scope: &str, is_msg: &dyn Fn(&str) -> bool) -> Option<String> {
        let exists = |n: &str| is_msg(n) || self.enums.contains_key(n);
        if let Some(abs) = name.strip_prefix('.') {
            return exists(abs).then(|| abs.to_string());
        }
        let mut scope = scope.to_string();
        loop {
            let candidate = if scope.is_empty() { name.to_string() } else { format!("{}.{}", scope, name) };
            if exists(&candidate) {
                return Some(candidate);
            }
            if scope.is_empty() {
                return None;
            }
            match scope.rfind('.') {
                Some(i) => scope.truncate(i),
                None => scope.clear(),
            }
        }
    }

    /// Finds a message by full name (a unique simple-name match is accepted too).
    pub fn message(&self, name: &str) -> Option<Arc<MessageDescriptor>> {
        let name = name.trim_start_matches('.');
        if let Some(m) = self.messages.get(name) {
            return Some(m.clone());
        }
        let suffix = format!(".{}", name);
        let mut it = self.messages.values().filter(|m| m.full_name.ends_with(&suffix) || m.full_name == name);
        match (it.next(), it.next()) {
            (Some(m), None) => Some(m.clone()),
            _ => None,
        }
    }

    pub fn enum_type(&self, name: &str) -> Option<Arc<EnumDescriptor>> {
        self.enums.get(name.trim_start_matches('.')).cloned()
    }

    /// Finds a service by full name (or unique simple name).
    pub fn service(&self, name: &str) -> Option<Arc<ServiceDescriptor>> {
        if let Some(s) = self.services.get(name) {
            return Some(s.clone());
        }
        let suffix = format!(".{}", name);
        let mut it = self.services.values().filter(|s| s.full_name.ends_with(&suffix));
        match (it.next(), it.next()) {
            (Some(s), None) => Some(s.clone()),
            _ => None,
        }
    }

    fn require_message(&self, name: &str) -> Result<Arc<MessageDescriptor>> {
        self.message(name).ok_or_else(|| CamelError::illegal_argument(format!("Unknown protobuf message type: {}", name)))
    }

    /// Encodes a value as message `type_name`.
    pub fn encode(&self, type_name: &str, value: &Value) -> Result<Vec<u8>> {
        let md = self.require_message(type_name)?;
        codec::encode_message(self, &md, value)
    }

    /// Decodes message `type_name` (proto3 defaults included).
    pub fn decode(&self, type_name: &str, bytes: &[u8]) -> Result<Value> {
        let md = self.require_message(type_name)?;
        codec::decode_message(self, &md, bytes, &codec::DecodeOptions::default())
    }

    /// Converts a native value of `type_name` to its proto3 JSON representation.
    pub fn to_json(&self, type_name: &str, value: &Value) -> Result<Value> {
        let md = self.require_message(type_name)?;
        json::to_json(self, &md, value)
    }

    /// Converts a proto3 JSON value of `type_name` to the native representation.
    pub fn from_json(&self, type_name: &str, value: &Value) -> Result<Value> {
        let md = self.require_message(type_name)?;
        json::from_json(self, &md, value)
    }

    /// Message names in the pool (excluding map entries), sorted.
    pub fn message_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.messages.values().filter(|m| !m.is_map_entry).map(|m| m.full_name.clone()).collect();
        v.sort();
        v
    }
}

/// Cache of loaded `.proto` files keyed by path (files are parsed once per process).
pub fn load_cached(path: &str) -> Result<Arc<DescriptorPool>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, (std::time::SystemTime, Arc<DescriptorPool>)>>> = OnceLock::new();
    let path = path.strip_prefix("classpath:").or_else(|| path.strip_prefix("file:")).unwrap_or(path);
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| CamelError::io(format!("Cannot read proto file {}: {}", path, e)))?;
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some((t, p)) = cache.lock().unwrap().get(path) {
        if *t == mtime {
            return Ok(p.clone());
        }
    }
    let pool = Arc::new(DescriptorPool::from_file(path)?);
    cache.lock().unwrap().insert(path.to_string(), (mtime, pool.clone()));
    Ok(pool)
}
