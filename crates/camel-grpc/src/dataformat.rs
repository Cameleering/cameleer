//! The `protobuf` data format (Camel `camel-protobuf`).
//!
//! The message type is named by `instanceClass` (a fully-qualified protobuf message
//! name such as `tutorial.Person`; a Java-style `org.example.Outer$Person` is reduced to
//! its last segment) and loaded from the `.proto` file given by `protoFile`.
//!
//! * `contentTypeFormat=native` (default): `marshal` encodes the body (an `Object`, a
//!   JSON string, or already-encoded bytes) to protobuf bytes; `unmarshal` decodes
//!   protobuf bytes to an `Object`.
//! * `contentTypeFormat=json`: `marshal` renders the body as proto3 JSON text
//!   (lowerCamelCase names); `unmarshal` parses proto3 JSON into an `Object`.
//! * `contentTypeHeader` (default true) sets `Content-Type`
//!   (`application/octet-stream` or `application/json`).
//!
//! Use [`protobuf`] / [`ProtobufDataFormat`] in the Rust DSL. The registry bean bound by
//! [`crate::register`] under the name `protobuf` reads its configuration from the headers
//! `CamelProtobufInstanceClass`, `CamelProtobufProtoFile`, `CamelProtobufContentTypeFormat`
//! or else the properties `camel.dataformat.protobuf.instanceClass`, `...protoFile`,
//! `...contentTypeFormat` (name-based references cannot carry options in rust-camel core).

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::dataformat::{DataFormat, DataFormatDefinition};
use camel_core::error::{CamelError, Result};
use camel_core::message::{props, Exchange};
use camel_core::value::Value;

use crate::proto::{load_cached, DescriptorPool};

/// Header overriding the message type of the default `protobuf` registry bean.
pub const PROTOBUF_INSTANCE_CLASS: &str = "CamelProtobufInstanceClass";
/// Header overriding the `.proto` file of the default `protobuf` registry bean.
pub const PROTOBUF_PROTO_FILE: &str = "CamelProtobufProtoFile";
/// Header overriding the content type format of the default `protobuf` registry bean.
pub const PROTOBUF_CONTENT_TYPE_FORMAT: &str = "CamelProtobufContentTypeFormat";

/// `native` protobuf bytes or proto3 `json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentTypeFormat {
    Native,
    Json,
}

impl ContentTypeFormat {
    pub fn parse(s: &str) -> Result<ContentTypeFormat> {
        match s.to_ascii_lowercase().as_str() {
            "native" => Ok(ContentTypeFormat::Native),
            "json" => Ok(ContentTypeFormat::Json),
            other => Err(CamelError::illegal_argument(format!("Invalid contentTypeFormat: {} (native or json)", other))),
        }
    }
}

/// Normalizes an `instanceClass` value to a protobuf message name.
pub fn message_name(instance_class: &str) -> String {
    match instance_class.rsplit_once('$') {
        Some((_, inner)) => inner.to_string(),
        None => instance_class.to_string(),
    }
}

fn df_err(msg: impl Into<String>) -> CamelError {
    CamelError::new("DataFormatException", msg.into())
}

/// Converts a body into the native value for `type_name`: objects pass through, JSON text
/// (string) is parsed with the proto3 JSON mapping.
pub fn body_to_native(pool: &DescriptorPool, type_name: &str, body: &Value) -> Result<Value> {
    match body {
        Value::String(s) if s.trim_start().starts_with('{') => {
            let json = camel_core::json::parse(s)?;
            pool.from_json(type_name, &json)
        }
        other => Ok(other.clone()),
    }
}

/// The protobuf data format.
#[derive(Clone)]
pub struct ProtobufDataFormat {
    config: Option<(Arc<DescriptorPool>, String)>,
    pub content_type_format: ContentTypeFormat,
    pub content_type_header: bool,
}

impl ProtobufDataFormat {
    /// A data format for message `type_name` from a loaded descriptor pool.
    pub fn new(pool: Arc<DescriptorPool>, type_name: &str) -> Result<ProtobufDataFormat> {
        let name = message_name(type_name);
        let md = pool.message(&name).ok_or_else(|| CamelError::illegal_argument(format!("Unknown protobuf message type {} (known: {})", name, pool.message_names().join(", "))))?;
        Ok(ProtobufDataFormat { config: Some((pool, md.full_name.clone())), content_type_format: ContentTypeFormat::Native, content_type_header: true })
    }

    /// Loads `proto_file` and selects `instance_class`.
    pub fn from_file(proto_file: &str, instance_class: &str) -> Result<ProtobufDataFormat> {
        Self::new(load_cached(proto_file)?, instance_class)
    }

    /// Builds from Camel options: `instanceClass`, `protoFile`, `contentTypeFormat`, `contentTypeHeader`.
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<ProtobufDataFormat> {
        let mut df = match (o.get("instanceClass"), o.get("protoFile")) {
            (Some(ic), Some(pf)) => Self::from_file(pf, ic)?,
            (None, None) => Self::dynamic(),
            _ => return Err(CamelError::illegal_argument("protobuf data format requires both instanceClass and protoFile")),
        };
        if let Some(f) = o.get("contentTypeFormat") {
            df.content_type_format = ContentTypeFormat::parse(f)?;
        }
        if let Some(h) = o.get("contentTypeHeader") {
            df.content_type_header = h != "false";
        }
        Ok(df)
    }

    /// A format configured per exchange (headers) or from context properties.
    pub fn dynamic() -> ProtobufDataFormat {
        ProtobufDataFormat { config: None, content_type_format: ContentTypeFormat::Native, content_type_header: true }
    }

    pub fn content_type_format(mut self, f: ContentTypeFormat) -> Self {
        self.content_type_format = f;
        self
    }

    pub fn content_type_header(mut self, b: bool) -> Self {
        self.content_type_header = b;
        self
    }

    /// The selected message type (when statically configured).
    pub fn message_type(&self) -> Option<&str> {
        self.config.as_ref().map(|(_, n)| n.as_str())
    }

    fn resolve(&self, ex: &Exchange) -> Result<(Arc<DescriptorPool>, String, ContentTypeFormat)> {
        let prop = |k: &str| ex.context.as_ref().and_then(|c| c.properties().get(&format!("camel.dataformat.protobuf.{}", k)));
        let hdr = |k: &str| ex.header(k).map(|v| v.to_string());
        let format = match hdr(PROTOBUF_CONTENT_TYPE_FORMAT).or_else(|| if self.config.is_none() { prop("contentTypeFormat") } else { None }) {
            Some(f) => ContentTypeFormat::parse(&f)?,
            None => self.content_type_format,
        };
        if let Some((pool, name)) = &self.config {
            return Ok((pool.clone(), name.clone(), format));
        }
        let ic = hdr(PROTOBUF_INSTANCE_CLASS).or_else(|| prop("instanceClass")).ok_or_else(|| df_err("protobuf data format: no instanceClass configured"))?;
        let pf = hdr(PROTOBUF_PROTO_FILE).or_else(|| prop("protoFile")).ok_or_else(|| df_err("protobuf data format: no protoFile configured"))?;
        let df = Self::from_file(&pf, &ic)?;
        let (pool, name) = df.config.expect("configured");
        Ok((pool, name, format))
    }
}

impl DataFormat for ProtobufDataFormat {
    fn name(&self) -> &str {
        "protobuf"
    }

    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let (pool, name, format) = self.resolve(ex)?;
        let body = ex.message.body.clone();
        match format {
            ContentTypeFormat::Native => {
                let bytes = match &body {
                    Value::Bytes(b) => b.clone(),
                    other => pool.encode(&name, &body_to_native(&pool, &name, other)?)?,
                };
                ex.message.body = Value::Bytes(bytes);
                if self.content_type_header {
                    ex.message.headers.set(props::CONTENT_TYPE, "application/octet-stream");
                }
            }
            ContentTypeFormat::Json => {
                let native = match &body {
                    Value::Bytes(b) => pool.decode(&name, b)?,
                    other => body_to_native(&pool, &name, other)?,
                };
                // validate against the schema by encoding once
                pool.encode(&name, &native)?;
                ex.message.body = Value::String(pool.to_json(&name, &native)?.to_json());
                if self.content_type_header {
                    ex.message.headers.set(props::CONTENT_TYPE, "application/json");
                }
            }
        }
        Ok(())
    }

    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let (pool, name, format) = self.resolve(ex)?;
        let v = match format {
            ContentTypeFormat::Native => {
                let bytes = match &ex.message.body {
                    Value::Bytes(b) => b.clone(),
                    Value::String(s) => s.as_bytes().to_vec(),
                    Value::Null => Vec::new(),
                    other => return Err(df_err(format!("Cannot unmarshal protobuf from {}", other.type_name()))),
                };
                pool.decode(&name, &bytes)?
            }
            ContentTypeFormat::Json => {
                let text = match &ex.message.body {
                    Value::Bytes(b) => String::from_utf8(b.clone()).map_err(|_| df_err("JSON body is not valid UTF-8"))?,
                    Value::String(s) => s.clone(),
                    other => return Err(df_err(format!("Cannot unmarshal protobuf JSON from {}", other.type_name()))),
                };
                let native = pool.from_json(&name, &camel_core::json::parse(&text)?)?;
                // round-trip through the codec to validate and fill defaults
                pool.decode(&name, &pool.encode(&name, &native)?)?
            }
        };
        ex.message.body = v;
        Ok(())
    }
}

/// `DataFormatDefinition` for the protobuf data format (Rust DSL):
/// `.marshal(camel_grpc::protobuf("proto/addressbook.proto", "tutorial.Person")?)`.
pub fn protobuf(proto_file: &str, instance_class: &str) -> Result<DataFormatDefinition> {
    let df = ProtobufDataFormat::from_file(proto_file, instance_class)?;
    Ok(DataFormatDefinition::custom(df))
}

/// Like [`protobuf`] with `contentTypeFormat=json`.
pub fn protobuf_json(proto_file: &str, instance_class: &str) -> Result<DataFormatDefinition> {
    let df = ProtobufDataFormat::from_file(proto_file, instance_class)?.content_type_format(ContentTypeFormat::Json);
    Ok(DataFormatDefinition::custom(df))
}
