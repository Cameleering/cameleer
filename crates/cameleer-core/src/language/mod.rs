//! Expression languages.
//!
//! Built-in languages registered on every context: `simple`, `constant`, `header`,
//! `exchangeProperty`, `variable`, `jsonpath`, `xpath`, `tokenize`, `jq`, `ref`, `method`
//! and `bean`. `groovy`, `js` etc. are not supported (resolving them gives
//! `NoSuchLanguageException`).
//!
//! Builder functions for the Rust DSL: [`jsonpath`], [`jsonpath_as`], [`tokenize`],
//! [`tokenize_regex`], [`tokenize_pair`], [`tokenize_xml`], [`xpath`], [`xpath_as`], [`jq`],
//! [`method`], [`bean_method`], [`ref_expression`]. Options can be added with
//! [`LanguageOptions::with_option`].

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::builder::{ExpressionDefinition, ValueBuilder};
use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Expression, Language, Predicate};
use crate::value::Value;

pub mod jq;
pub mod jsonpath;
pub mod simple;
pub mod support;
pub mod tokenize;
pub mod xml;
pub mod xpath;

pub use support::{bind_bean_method, BeanMethod};

/// `constant` language: the text itself.
pub struct ConstantLanguage;
impl Language for ConstantLanguage {
    fn name(&self) -> &str {
        "constant"
    }
    fn create_expression(&self, text: &str, o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let mut v = Value::String(text.to_string());
        if let Some(t) = o.get("resultType").filter(|t| !t.is_empty()) {
            v = support::convert_to_type(&v, t)?;
        }
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

/// `variable` language: value of the named variable.
pub struct VariableLanguage;
impl Language for VariableLanguage {
    fn name(&self) -> &str {
        "variable"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let n = format!("CamelVariable.{}", text.trim());
        Ok(Arc::new(move |ex: &Exchange| Ok(ex.properties.get(&n).cloned().unwrap_or(Value::Null))))
    }
}

/// `ref` language: looks up an expression (or predicate / bean method / value) in the
/// registry by name at evaluation time.
pub struct RefLanguage;
impl Language for RefLanguage {
    fn name(&self) -> &str {
        "ref"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let name = text.trim().to_string();
        Ok(Arc::new(move |ex: &Exchange| {
            let ctx = ex.context()?;
            let reg = ctx.registry();
            if let Some(e) = reg.lookup::<Arc<dyn Expression>>(&name) {
                return e.evaluate(ex);
            }
            if let Some(p) = reg.lookup::<Arc<dyn Predicate>>(&name) {
                return Ok(Value::Bool(p.matches(ex)?));
            }
            if !reg.contains(&name) {
                return Err(CamelError::no_such_bean(&name));
            }
            support::lookup_ref_value(ctx, &name, ex)
        }))
    }
    fn create_predicate(&self, text: &str, o: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        let name = text.trim().to_string();
        let expr = self.create_expression(text, o)?;
        Ok(Arc::new(crate::spi::FnPredicate(move |ex: &Exchange| {
            if let Some(p) = ex.context()?.registry().lookup::<Arc<dyn Predicate>>(&name) {
                return p.matches(ex);
            }
            Ok(support::value_predicate(&expr.evaluate(ex)?))
        })))
    }
}

/// `method` / `bean` language: invokes a [`BeanMethod`] (or processor) from the registry.
/// Text: `beanName`, `beanName.method`, `beanName::method` or `beanName?method=m`;
/// option `method` names the method.
pub struct MethodLanguage(pub &'static str);
impl Language for MethodLanguage {
    fn name(&self) -> &str {
        self.0
    }
    fn create_expression(&self, text: &str, o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let t = text.trim();
        let bean = o.get("ref").cloned().filter(|r| !r.is_empty() && t.is_empty());
        let (name, mut method) = if let Some(b) = bean {
            (b, None)
        } else if let Some((n, m)) = t.split_once("?method=") {
            (n.to_string(), Some(m.to_string()))
        } else if let Some((n, m)) = t.split_once("::") {
            (n.to_string(), Some(m.to_string()))
        } else if let Some((n, m)) = t.split_once('.') {
            (n.to_string(), Some(m.to_string()))
        } else {
            (t.to_string(), None)
        };
        if let Some(m) = o.get("method").filter(|m| !m.is_empty()) {
            method = Some(m.clone());
        }
        if name.is_empty() {
            return Err(CamelError::illegal_argument("method language requires a bean name"));
        }
        Ok(Arc::new(move |ex: &Exchange| support::invoke_bean(ex.context()?, &name, method.as_deref(), ex)))
    }
}

/// Register all built-in languages on a context.
pub fn register_builtin_languages(ctx: &CamelContext) {
    ctx.add_language(ConstantLanguage);
    ctx.add_language(HeaderLanguage);
    ctx.add_language(ExchangePropertyLanguage);
    ctx.add_language(VariableLanguage);
    ctx.add_language(simple::SimpleLanguage);
    ctx.add_language(jsonpath::JsonPathLanguage);
    ctx.add_language(xpath::XPathLanguage);
    ctx.add_language(tokenize::TokenizeLanguage);
    ctx.add_language(jq::JqLanguage);
    ctx.add_language(RefLanguage);
    ctx.add_language(MethodLanguage("method"));
    ctx.add_language(MethodLanguage("bean"));
}

// ---------------------------------------------------------------------------
// DSL builder functions
// ---------------------------------------------------------------------------

/// Adds language options to a [`ValueBuilder`] made from a language expression.
pub trait LanguageOptions {
    /// Add a language option (e.g. `resultType`, `suppressExceptions`, `group`).
    fn with_option(self, key: &str, value: &str) -> ValueBuilder;
}

impl LanguageOptions for ValueBuilder {
    fn with_option(self, key: &str, value: &str) -> ValueBuilder {
        ValueBuilder::new(self.def.option(key, value))
    }
}

fn lang(name: &str, text: &str) -> ExpressionDefinition {
    ExpressionDefinition::language(name, text)
}

/// JSONPath expression, e.g. `jsonpath("$.order.id")`.
pub fn jsonpath(path: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("jsonpath", &path.into()))
}

/// JSONPath expression converted to a result type (`String`, `Integer`, `Map`, ...).
pub fn jsonpath_as(path: impl Into<String>, result_type: &str) -> ValueBuilder {
    ValueBuilder::new(lang("jsonpath", &path.into()).option("resultType", result_type))
}

/// Tokenize the body on a token, e.g. `tokenize("\n")`.
pub fn tokenize(token: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("tokenize", "").option("token", token.into()))
}

/// Tokenize the body on a regular expression.
pub fn tokenize_regex(regex: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("tokenize", "").option("token", regex.into()).option("regex", "true"))
}

/// Tokenize the text between a start and end token.
pub fn tokenize_pair(start: impl Into<String>, end: impl Into<String>, include_tokens: bool) -> ValueBuilder {
    ValueBuilder::new(
        lang("tokenize", "").option("token", start.into()).option("endToken", end.into()).option("includeTokens", include_tokens.to_string()),
    )
}

/// Tokenize XML elements with the given tag name (e.g. `tokenize_xml("order")`).
pub fn tokenize_xml(tag: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("tokenize", "").option("token", tag.into()).option("xml", "true"))
}

/// XPath expression, e.g. `xpath("/order/@id")`.
pub fn xpath(expr: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("xpath", &expr.into()))
}

/// XPath expression with a result type (`String`, `Boolean`, `Number`, `Integer`, `NodeList`).
pub fn xpath_as(expr: impl Into<String>, result_type: &str) -> ValueBuilder {
    ValueBuilder::new(lang("xpath", &expr.into()).option("resultType", result_type))
}

/// jq expression, e.g. `jq(".items | length")`.
pub fn jq(expr: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("jq", &expr.into()))
}

/// Invoke a registry bean (`"bean"` or `"bean.method"`), see [`bind_bean_method`].
pub fn method(bean: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("method", &bean.into()))
}

/// Invoke a registry bean method.
pub fn bean_method(bean: impl Into<String>, method_name: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("method", &bean.into()).option("method", method_name.into()))
}

/// Expression looked up in the registry by name (`ref` language).
pub fn ref_expression(name: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(lang("ref", &name.into()))
}
