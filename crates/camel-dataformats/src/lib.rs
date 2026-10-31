//! rust-camel data formats: the formats from Apache Camel that are not part of `camel-core`.
//!
//! | name | aliases | module |
//! |---|---|---|
//! | `avro` | `avroJackson`, `avro-jackson` (JSON <-> Avro) | [`avro`] |
//! | `bindyCsv` | `bindy-csv`, `bindy` | [`bindy`] |
//! | `bindyFixed` | `bindy-fixed` | [`bindy`] |
//! | `bindyKvp` | `bindy-kvp` | [`bindy`] |
//! | `yaml` | `snakeYaml`, `snakeyaml` | [`yaml_df`] |
//! | `tarFile` | `tarfile`, `tar` | [`tar`] |
//! | `zipFile` | `zipfile` (multi-entry, replaces the core one) | [`zip`] |
//! | `crypto` | | [`crypto`] |
//! | `pgp` | | [`pgp`] |
//! | `cbor` | | [`cbor`] |
//! | `thrift` | | [`thrift`] |
//! | `asn1` | | [`asn1`] |
//! | `iCal` | `ical` | [`ical`] |
//! | `mimeMultipart` | `mime-multipart` | [`mime`] |
//! | `univocityCsv`, `univocityTsv`, `univocityFixed` | `univocity-csv`, ... | [`univocity`] |
//!
//! Call [`register`] on a context: every format is registered as a data format *factory*
//! ([`camel_core::dataformat::register_data_format_factory`]) so that name references with
//! options work in the Rust DSL (`DataFormatDefinition::new("avro").option("schema", ..)`),
//! the YAML DSL (`marshal: {avro: {schema: ...}}`) and the `dataformat:` component.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::dataformat::{register_data_format_factory, DataFormat};
use camel_core::error::{CamelError, Result};
use camel_core::{CamelContext, Value};

pub mod asn1;
pub mod avro;
pub mod bindy;
pub mod cbor;
pub mod crypto;
pub mod ical;
pub mod mime;
pub mod pgp;
pub mod tar;
pub mod thrift;
pub mod univocity;
pub mod yaml_df;
pub mod zip;

/// Error kind used by the data formats in this crate.
pub const DATA_FORMAT_EXCEPTION: &str = "DataFormatException";

pub(crate) fn df_err(msg: impl Into<String>) -> CamelError {
    CamelError::new(DATA_FORMAT_EXCEPTION, msg)
}

/// Options of a data format (string key/values as passed by the DSLs), with typed getters.
#[derive(Clone, Debug, Default)]
pub struct Opts(pub BTreeMap<String, String>);

impl Opts {
    pub fn new(o: &BTreeMap<String, String>) -> Opts {
        Opts(o.clone())
    }
    /// First present option among `keys` (exact match, then case-insensitive).
    pub fn get(&self, keys: &[&str]) -> Option<&str> {
        for k in keys {
            if let Some(v) = self.0.get(*k) {
                return Some(v.as_str());
            }
        }
        for k in keys {
            if let Some((_, v)) = self.0.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)) {
                return Some(v.as_str());
            }
        }
        None
    }
    pub fn string(&self, keys: &[&str], default: &str) -> String {
        self.get(keys).unwrap_or(default).to_string()
    }
    pub fn bool(&self, keys: &[&str], default: bool) -> Result<bool> {
        match self.get(keys) {
            None => Ok(default),
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => Ok(true),
                "false" | "no" | "off" => Ok(false),
                _ => Err(CamelError::illegal_argument(format!("Option {} must be true or false but was: {}", keys[0], v))),
            },
        }
    }
    pub fn num(&self, keys: &[&str], default: i64) -> Result<i64> {
        match self.get(keys) {
            None => Ok(default),
            Some(v) => v.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Option {} must be a number but was: {}", keys[0], v))),
        }
    }
}

/// Unescape `\t`, `\n`, `\r`, `\\` and `\uXXXX` in option values (`separator: "\\t"`).
pub(crate) fn unescape_opt(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let h: String = it.by_ref().take(4).collect();
                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&h);
                    }
                }
            }
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Body as bytes (strings as UTF-8).
pub(crate) fn body_bytes(v: &Value) -> Vec<u8> {
    v.to_bytes()
}

/// Body as text (bytes decoded as UTF-8).
pub(crate) fn body_text(v: &Value) -> Result<String> {
    match v {
        Value::Bytes(b) => String::from_utf8(b.clone()).map_err(|e| df_err(format!("Body is not valid UTF-8: {}", e))),
        other => Ok(other.to_string()),
    }
}

/// Read a file option relative to the working directory (also accepts `file:` and
/// `classpath:` prefixes, the latter resolved against the working directory and `src/main/resources`).
pub(crate) fn read_resource(path: &str) -> Result<Vec<u8>> {
    let p = path.trim();
    let p = p.strip_prefix("file:").unwrap_or(p);
    if let Some(cp) = p.strip_prefix("classpath:") {
        let cp = cp.trim_start_matches('/');
        for base in ["", "src/main/resources/", "src/test/resources/"] {
            if let Ok(b) = std::fs::read(format!("{}{}", base, cp)) {
                return Ok(b);
            }
        }
        return Err(CamelError::new("FileNotFoundException", format!("Cannot find resource: {}", path)));
    }
    std::fs::read(p).map_err(|e| CamelError::new("FileNotFoundException", format!("Cannot read {}: {}", path, e)))
}

type Factory = fn(&Opts) -> Result<Arc<dyn DataFormat>>;

/// All data format names contributed by this crate with their factories.
pub fn factories() -> Vec<(&'static str, Factory)> {
    let mut v: Vec<(&'static str, Factory)> = Vec::new();
    v.push(("avro", avro::create));
    for n in ["avroJackson", "avro-jackson", "avrojackson", "jacksonAvro"] {
        v.push((n, avro::create_jackson));
    }
    for n in ["bindyCsv", "bindy-csv", "bindycsv", "bindy"] {
        v.push((n, bindy::create_csv));
    }
    for n in ["bindyFixed", "bindy-fixed", "bindyfixed"] {
        v.push((n, bindy::create_fixed));
    }
    for n in ["bindyKvp", "bindy-kvp", "bindykvp"] {
        v.push((n, bindy::create_kvp));
    }
    for n in ["yaml", "snakeYaml", "snakeyaml", "snake-yaml"] {
        v.push((n, yaml_df::create));
    }
    for n in ["tarFile", "tarfile", "tar"] {
        v.push((n, tar::create));
    }
    for n in ["zipFile", "zipfile"] {
        v.push((n, zip::create));
    }
    v.push(("crypto", crypto::create));
    v.push(("pgp", pgp::create));
    v.push(("cbor", cbor::create));
    v.push(("thrift", thrift::create));
    v.push(("asn1", asn1::create));
    for n in ["iCal", "ical"] {
        v.push((n, ical::create));
    }
    for n in ["mimeMultipart", "mime-multipart", "mimemultipart"] {
        v.push((n, mime::create));
    }
    for n in ["univocityCsv", "univocity-csv", "univocitycsv"] {
        v.push((n, univocity::create_csv));
    }
    for n in ["univocityTsv", "univocity-tsv", "univocitytsv"] {
        v.push((n, univocity::create_tsv));
    }
    for n in ["univocityFixed", "univocity-fixed", "univocityfixed"] {
        v.push((n, univocity::create_fixed));
    }
    v
}

/// Create one of this crate's data formats by name (`None` if the name is not ours).
pub fn create(name: &str, options: &BTreeMap<String, String>) -> Option<Result<Arc<dyn DataFormat>>> {
    factories().into_iter().find(|(n, _)| *n == name).map(|(_, f)| f(&Opts::new(options)))
}

/// Register all data formats of this crate on the context.
pub fn register(ctx: &CamelContext) {
    for (name, f) in factories() {
        register_data_format_factory(ctx.registry(), name, move |o| f(&Opts::new(o)));
    }
}
