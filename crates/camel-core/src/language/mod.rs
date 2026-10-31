//! Expression languages.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::Result;
use crate::message::Exchange;
use crate::spi::{Expression, Language};
use crate::value::Value;

pub mod simple;

/// `constant` language: the text itself.
pub struct ConstantLanguage;
impl Language for ConstantLanguage {
    fn name(&self) -> &str {
        "constant"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let v = Value::String(text.to_string());
        Ok(Arc::new(move |_: &Exchange| Ok(v.clone())))
    }
}

/// `header` language: value of the named header.
pub struct HeaderLanguage;
impl Language for HeaderLanguage {
    fn name(&self) -> &str {
        "header"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let n = text.trim().to_string();
        Ok(Arc::new(move |ex: &Exchange| Ok(ex.message.headers.get(&n).cloned().unwrap_or(Value::Null))))
    }
}

/// `exchangeProperty` language: value of the named exchange property.
pub struct ExchangePropertyLanguage;
impl Language for ExchangePropertyLanguage {
    fn name(&self) -> &str {
        "exchangeProperty"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let n = text.trim().to_string();
        Ok(Arc::new(move |ex: &Exchange| Ok(ex.properties.get(&n).cloned().unwrap_or(Value::Null))))
    }
}

pub fn register_builtin_languages(ctx: &CamelContext) {
    ctx.add_language(ConstantLanguage);
    ctx.add_language(HeaderLanguage);
    ctx.add_language(ExchangePropertyLanguage);
    ctx.add_language(simple::SimpleLanguage::default());
}
