//! Data formats (marshal / unmarshal).
//!
//! A [`DataFormat`] transforms the message body: `marshal` turns a structured value into a
//! wire format, `unmarshal` does the reverse. Built-in formats (resolved by name):
//!
//! | name | aliases | |
//! |---|---|---|
//! | `json` | `jackson`, `gson`, `jsonb`, `fastjson` | value <-> JSON text |
//! | `csv` | | RFC-4180 CSV |
//! | `base64` | | base64 text |
//! | `string` | | string <-> bytes (charset) |
//! | `hex` | | bytes <-> hex text |
//! | `gzipDeflater` | `gzip` | gzip compression |
//! | `zipDeflater` | `zip`, `deflate`, `zlib` | zlib compression |
//! | `zipFile` | `zipfile` | ZIP archive with one entry |
//! | `jacksonXml` | `xml`, `jaxb` | value <-> XML |
//! | `urlencoded` | | object <-> form encoding |
//!
//! Custom formats are bound in the registry with [`bind_data_format`] and referenced by
//! name (`DataFormatDefinition::named("myDf")`). Use [`DataFormatDefinition`] in the DSL:
//! `.marshal(DataFormatDefinition::json().pretty_print(true))`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::{CamelContext, Registry};
use crate::error::{CamelError, Result};
use crate::message::Exchange;

pub mod base64;
pub mod compress;
pub mod csv;
pub mod formats;

/// A data format: transforms the body to (marshal) and from (unmarshal) a wire format.
pub trait DataFormat: Send + Sync {
    /// Name of the data format (e.g. `json`).
    fn name(&self) -> &str;
    /// Marshal the exchange body (object -> wire format).
    fn marshal(&self, exchange: &mut Exchange) -> Result<()>;
    /// Unmarshal the exchange body (wire format -> object).
    fn unmarshal(&self, exchange: &mut Exchange) -> Result<()>;
}

/// Bind a data format instance in the registry under `name`.
pub fn bind_data_format(registry: &Registry, name: impl Into<String>, df: impl DataFormat + 'static) {
    let df: Arc<dyn DataFormat> = Arc::new(df);
    registry.bind(name, df);
}

/// Look up a data format bound in the registry.
pub fn lookup_data_format(registry: &Registry, name: &str) -> Option<Arc<dyn DataFormat>> {
    registry.lookup::<Arc<dyn DataFormat>>(name).map(|a| (*a).clone())
}

/// A factory creating a data format from its options (used for formats contributed by other
/// crates, so that name references with options - `marshal: {avro: {schema: ...}}`,
/// `DataFormatDefinition::new("avro").option(..)`, `dataformat:avro:marshal?...` - work).
pub type DataFormatFactory = dyn Fn(&BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> + Send + Sync;

struct FactoryHolder(Arc<DataFormatFactory>);

const FACTORY_PREFIX: &str = "camel.dataformat.factory.";

/// Register a data format factory under `name`. Factories take precedence over built-in
/// formats of the same name (but not over data format instances bound with [`bind_data_format`]).
pub fn register_data_format_factory(
    registry: &Registry,
    name: &str,
    factory: impl Fn(&BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> + Send + Sync + 'static,
) {
    registry.bind(format!("{}{}", FACTORY_PREFIX, name), FactoryHolder(Arc::new(factory)));
}

/// Look up a data format factory registered with [`register_data_format_factory`].
pub fn lookup_data_format_factory(registry: &Registry, name: &str) -> Option<Arc<DataFormatFactory>> {
    registry.lookup::<FactoryHolder>(&format!("{}{}", FACTORY_PREFIX, name)).map(|h| h.0.clone())
}

/// Create a data format by name: a registered factory, else a built-in format.
pub fn create_data_format_with(registry: &Registry, name: &str, o: &BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> {
    match lookup_data_format_factory(registry, name) {
        Some(f) => f(o),
        None => create_data_format(name, o),
    }
}

fn opt<'a>(o: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| o.get(*k).map(|s| s.as_str()))
}

fn opt_bool(o: &BTreeMap<String, String>, keys: &[&str], default: bool) -> Result<bool> {
    match opt(o, keys) {
        None => Ok(default),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(CamelError::illegal_argument(format!("Option {} must be true or false but was: {}", keys[0], v))),
        },
    }
}

fn opt_num(o: &BTreeMap<String, String>, keys: &[&str], default: i64) -> Result<i64> {
    match opt(o, keys) {
        None => Ok(default),
        Some(v) => v.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Option {} must be a number but was: {}", keys[0], v))),
    }
}

fn level(o: &BTreeMap<String, String>) -> Result<u32> {
    let l = opt_num(o, &["compressionLevel", "level"], 6)?;
    Ok(if l < 0 { 6 } else { l.min(9) as u32 })
}

/// Create a built-in data format by name with options.
pub fn create_data_format(name: &str, o: &BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> {
    Ok(match name {
        "json" | "jackson" | "gson" | "jsonb" | "fastjson" | "johnzon" => Arc::new(formats::JsonDataFormat {
            pretty_print: opt_bool(o, &["prettyPrint", "pretty_print"], false)?,
            content_type_header: opt_bool(o, &["contentTypeHeader"], true)?,
        }),
        "csv" => {
            let mut c = csv::CsvDataFormat::default();
            if let Some(d) = opt(o, &["delimiter"]) {
                c.delimiter = csv::parse_char_option(d).unwrap_or(',');
            }
            if let Some(q) = opt(o, &["quote"]) {
                c.quote = csv::parse_char_option(q);
            }
            if opt_bool(o, &["quoteDisabled"], false)? {
                c.quote = None;
            }
            if let Some(e) = opt(o, &["escape"]) {
                c.escape = csv::parse_char_option(e);
            }
            if let Some(r) = opt(o, &["recordSeparator"]) {
                c.record_separator = r.replace("\\r", "\r").replace("\\n", "\n");
            }
            if let Some(h) = opt(o, &["header"]) {
                c.header = h.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            c.skip_header_record = opt_bool(o, &["skipHeaderRecord"], false)?;
            c.use_maps = opt_bool(o, &["useMaps"], false)? || opt_bool(o, &["useOrderedMaps"], false)?;
            c.ignore_empty_lines = opt_bool(o, &["ignoreEmptyLines"], true)?;
            c.trim = opt_bool(o, &["ignoreSurroundingSpaces", "trim"], false)?;
            if let Some(q) = opt(o, &["quoteMode"]) {
                c.quote_mode = q.to_string();
            }
            c.null_string = opt(o, &["nullString"]).map(|s| s.to_string());
            c.comment_marker = opt(o, &["commentMarker"]).and_then(csv::parse_char_option);
            Arc::new(c)
        }
        "base64" => Arc::new(base64::Base64DataFormat {
            line_length: opt_num(o, &["lineLength"], 76)?.max(0) as usize,
            line_separator: opt(o, &["lineSeparator"]).map(|s| s.replace("\\r", "\r").replace("\\n", "\n")).unwrap_or_else(|| "\r\n".into()),
            url_safe: opt_bool(o, &["urlSafe"], false)?,
        }),
        "string" => Arc::new(formats::StringDataFormat { charset: opt(o, &["charset"]).unwrap_or("UTF-8").to_string() }),
        "hex" => Arc::new(formats::HexDataFormat { upper_case: opt_bool(o, &["upperCase"], false)? }),
        "gzipDeflater" | "gzipdeflater" | "gzip" => Arc::new(compress::GzipDataFormat { level: level(o)? }),
        "zipDeflater" | "zipdeflater" | "zip" | "deflate" | "zlib" => Arc::new(compress::ZlibDataFormat { level: level(o)? }),
        "zipFile" | "zipfile" => Arc::new(compress::ZipFileDataFormat { level: level(o)?, using_iterator: opt_bool(o, &["usingIterator"], false)? }),
        "jacksonXml" | "jacksonxml" | "xml" | "jaxb" => Arc::new(formats::XmlDataFormat {
            root_name: opt(o, &["rootName"]).unwrap_or("root").to_string(),
            pretty_print: opt_bool(o, &["prettyPrint"], false)?,
            content_type_header: opt_bool(o, &["contentTypeHeader"], true)?,
        }),
        "urlencoded" => Arc::new(formats::UrlEncodedDataFormat),
        other => return Err(CamelError::illegal_argument(format!("Cannot find data format with name: {}", other))),
    })
}

/// Model of a data format reference used by the `marshal`/`unmarshal` EIPs: a built-in
/// name with options, a registry reference, or a ready instance.
#[derive(Clone)]
pub struct DataFormatDefinition {
    /// Data format name (built-in name or registry reference).
    pub name: String,
    /// Options passed to the data format.
    pub options: BTreeMap<String, String>,
    /// A ready-made instance (takes precedence).
    pub instance: Option<Arc<dyn DataFormat>>,
}

impl std::fmt::Debug for DataFormatDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.options.is_empty() {
            write!(f, "{:?}", self.options)?;
        }
        Ok(())
    }
}

impl DataFormatDefinition {
    /// A data format by name (built-in or registry reference).
    pub fn new(name: impl Into<String>) -> Self {
        DataFormatDefinition { name: name.into(), options: BTreeMap::new(), instance: None }
    }
    /// A data format bound in the registry (or a built-in name).
    pub fn named(name: impl Into<String>) -> Self {
        Self::new(name)
    }
    /// A ready-made data format instance.
    pub fn custom(df: impl DataFormat + 'static) -> Self {
        let name = df.name().to_string();
        DataFormatDefinition { name, options: BTreeMap::new(), instance: Some(Arc::new(df)) }
    }
    pub fn json() -> Self {
        Self::new("json")
    }
    pub fn csv() -> Self {
        Self::new("csv")
    }
    pub fn base64() -> Self {
        Self::new("base64")
    }
    pub fn string() -> Self {
        Self::new("string")
    }
    pub fn hex() -> Self {
        Self::new("hex")
    }
    pub fn gzip() -> Self {
        Self::new("gzipDeflater")
    }
    pub fn zip_deflater() -> Self {
        Self::new("zipDeflater")
    }
    pub fn zip_file() -> Self {
        Self::new("zipFile")
    }
    pub fn xml() -> Self {
        Self::new("jacksonXml")
    }
    pub fn urlencoded() -> Self {
        Self::new("urlencoded")
    }

    /// Set an option.
    pub fn option(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.options.insert(key.into(), value.to_string());
        self
    }
    pub fn pretty_print(self, b: bool) -> Self {
        self.option("prettyPrint", b)
    }
    pub fn delimiter(self, c: char) -> Self {
        self.option("delimiter", c)
    }
    pub fn quote(self, c: char) -> Self {
        self.option("quote", c)
    }
    pub fn use_maps(self, b: bool) -> Self {
        self.option("useMaps", b)
    }
    pub fn use_ordered_maps(self, b: bool) -> Self {
        self.option("useOrderedMaps", b)
    }
    pub fn skip_header_record(self, b: bool) -> Self {
        self.option("skipHeaderRecord", b)
    }
    pub fn header(self, names: &[&str]) -> Self {
        self.option("header", names.join(","))
    }
    pub fn line_length(self, n: usize) -> Self {
        self.option("lineLength", n)
    }
    pub fn url_safe(self, b: bool) -> Self {
        self.option("urlSafe", b)
    }
    pub fn charset(self, cs: &str) -> Self {
        self.option("charset", cs)
    }
    pub fn root_name(self, n: &str) -> Self {
        self.option("rootName", n)
    }
    pub fn compression_level(self, l: u32) -> Self {
        self.option("compressionLevel", l)
    }

    /// Resolve to a data format instance: the instance, a registry bean named `name`, or a
    /// built-in format.
    pub fn resolve(&self, ctx: &CamelContext) -> Result<Arc<dyn DataFormat>> {
        if let Some(i) = &self.instance {
            return Ok(i.clone());
        }
        if let Some(df) = lookup_data_format(ctx.registry(), &self.name) {
            return Ok(df);
        }
        let mut opts = BTreeMap::new();
        for (k, v) in &self.options {
            opts.insert(k.clone(), ctx.resolve_placeholders(v)?);
        }
        create_data_format_with(ctx.registry(), &self.name, &opts)
    }
}

impl From<&str> for DataFormatDefinition {
    fn from(s: &str) -> Self {
        DataFormatDefinition::new(s)
    }
}

impl From<String> for DataFormatDefinition {
    fn from(s: String) -> Self {
        DataFormatDefinition::new(s)
    }
}
