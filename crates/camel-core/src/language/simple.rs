//! The Simple language.
//!
//! PLACEHOLDER: supports only `${body}`, `${header.X}`/`${headers.X}`,
//! `${exchangeProperty.X}` and literal text until the full implementation lands.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::Result;
use crate::message::Exchange;
use crate::spi::{Expression, Language};
use crate::value::Value;

#[derive(Default)]
pub struct SimpleLanguage;

impl Language for SimpleLanguage {
    fn name(&self) -> &str {
        "simple"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let text = text.to_string();
        Ok(Arc::new(move |ex: &Exchange| {
            let mut out = String::new();
            let mut rest = text.as_str();
            let mut only_one = None;
            while let Some(s) = rest.find("${") {
                out.push_str(&rest[..s]);
                let e = rest[s..].find('}').map(|e| s + e).unwrap_or(rest.len() - 1);
                let f = &rest[s + 2..e];
                let v = if f == "body" || f == "in.body" {
                    ex.message.body.clone()
                } else if let Some(h) = f.strip_prefix("header.").or_else(|| f.strip_prefix("headers.")) {
                    ex.message.headers.get(h).cloned().unwrap_or_default()
                } else if let Some(p) = f.strip_prefix("exchangeProperty.") {
                    ex.properties.get(p).cloned().unwrap_or_default()
                } else {
                    Value::Null
                };
                if s == 0 && e == rest.len() - 1 && out.is_empty() {
                    only_one = Some(v.clone());
                }
                out.push_str(&v.to_string());
                rest = &rest[e + 1..];
            }
            out.push_str(rest);
            Ok(only_one.unwrap_or(Value::String(out)))
        }))
    }
}
