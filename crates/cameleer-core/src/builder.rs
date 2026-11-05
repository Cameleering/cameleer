//! Expression and predicate builders for the Rust DSL:
//! `simple("${body}")`, `constant(5)`, `header("foo").is_equal_to("bar")`, `body()`, ...

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Expression, ExpressionPredicate, FnPredicate, Predicate, PredicateExpression};
use crate::value::{compare_values, values_equal, Value};

/// A (possibly not yet resolved) expression. Language expressions are resolved
/// against the context when the route is created.
#[derive(Clone)]
pub enum ExpressionDefinition {
    /// An expression in a named language, e.g. `simple` / `${body}`.
    Language { language: String, expression: String, options: BTreeMap<String, String> },
    /// A ready-made expression.
    Expression(Arc<dyn Expression>),
    /// A ready-made predicate.
    Predicate(Arc<dyn Predicate>),
}

impl std::fmt::Debug for ExpressionDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpressionDefinition::Language { language, expression, options } => {
                write!(f, "{}{{{}}}", language, expression)?;
                if !options.is_empty() {
                    write!(f, "{:?}", options)?;
                }
                Ok(())
            }
            ExpressionDefinition::Expression(_) => f.write_str("<expression>"),
            ExpressionDefinition::Predicate(_) => f.write_str("<predicate>"),
        }
    }
}

impl ExpressionDefinition {
    pub fn language(language: impl Into<String>, expression: impl Into<String>) -> Self {
        ExpressionDefinition::Language { language: language.into(), expression: expression.into(), options: BTreeMap::new() }
    }

    /// Add a language option (e.g. `token`, `resultType`).
    pub fn option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if let ExpressionDefinition::Language { options, .. } = &mut self {
            options.insert(key.into(), value.into());
        }
        self
    }

    /// Resolve to an executable expression.
    pub fn resolve_expression(&self, ctx: &CamelContext) -> Result<Arc<dyn Expression>> {
        match self {
            ExpressionDefinition::Language { language, expression, options } => {
                let lang = ctx.resolve_language(language)?;
                let text = ctx.resolve_placeholders(expression)?;
                lang.create_expression(&text, options)
            }
            ExpressionDefinition::Expression(e) => Ok(e.clone()),
            ExpressionDefinition::Predicate(p) => Ok(Arc::new(PredicateExpression(p.clone()))),
        }
    }

    /// Resolve to an executable predicate.
    pub fn resolve_predicate(&self, ctx: &CamelContext) -> Result<Arc<dyn Predicate>> {
        match self {
            ExpressionDefinition::Language { language, expression, options } => {
                let lang = ctx.resolve_language(language)?;
                let text = ctx.resolve_placeholders(expression)?;
                lang.create_predicate(&text, options)
            }
            ExpressionDefinition::Expression(e) => Ok(Arc::new(ExpressionPredicate(e.clone()))),
            ExpressionDefinition::Predicate(p) => Ok(p.clone()),
        }
    }

    /// Evaluate directly (resolving against the exchange's context). Convenience for tests.
    pub fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        match self {
            ExpressionDefinition::Expression(e) => e.evaluate(ex),
            ExpressionDefinition::Predicate(p) => Ok(Value::Bool(p.matches(ex)?)),
            _ => self.resolve_expression(ex.context()?)?.evaluate(ex),
        }
    }
}

impl From<Arc<dyn Expression>> for ExpressionDefinition {
    fn from(e: Arc<dyn Expression>) -> Self {
        ExpressionDefinition::Expression(e)
    }
}
impl From<Arc<dyn Predicate>> for ExpressionDefinition {
    fn from(p: Arc<dyn Predicate>) -> Self {
        ExpressionDefinition::Predicate(p)
    }
}
impl From<ValueBuilder> for ExpressionDefinition {
    fn from(v: ValueBuilder) -> Self {
        v.def
    }
}

/// Anything that can be used where the DSL expects an expression or predicate.
pub trait IntoExpression {
    fn into_expression(self) -> ExpressionDefinition;
}
impl<T: Into<ExpressionDefinition>> IntoExpression for T {
    fn into_expression(self) -> ExpressionDefinition {
        self.into()
    }
}

// ---------------------------------------------------------------------------
// Builder functions
// ---------------------------------------------------------------------------

/// Simple language expression, e.g. `simple("Hello ${body}")`.
pub fn simple(text: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(ExpressionDefinition::language("simple", text))
}

/// Expression in any registered language.
pub fn language(name: impl Into<String>, text: impl Into<String>) -> ValueBuilder {
    ValueBuilder::new(ExpressionDefinition::language(name, text))
}

/// A constant value.
pub fn constant(v: impl Into<Value>) -> ValueBuilder {
    let v = v.into();
    expr(move |_| Ok(v.clone()))
}

/// The message body.
pub fn body() -> ValueBuilder {
    expr(|ex| Ok(ex.message.body.clone()))
}

/// A message header (null if missing).
pub fn header(name: impl Into<String>) -> ValueBuilder {
    let name = name.into();
    expr(move |ex| Ok(ex.message.headers.get(&name).cloned().unwrap_or(Value::Null)))
}

/// An exchange property (null if missing).
pub fn exchange_property(name: impl Into<String>) -> ValueBuilder {
    let name = name.into();
    expr(move |ex| Ok(ex.properties.get(&name).cloned().unwrap_or(Value::Null)))
}

/// All headers as an object.
pub fn headers() -> ValueBuilder {
    expr(|ex| Ok(ex.message.headers.to_value()))
}

/// The exchange id.
pub fn exchange_id() -> ValueBuilder {
    expr(|ex| Ok(Value::String(ex.exchange_id.to_string())))
}

/// The caught exception message (null if none).
pub fn exception_message() -> ValueBuilder {
    expr(|ex| {
        let e = ex.exception.as_ref().or(None);
        if let Some(e) = e {
            return Ok(Value::String(e.message.clone()));
        }
        Ok(ex
            .properties
            .get(crate::message::props::EXCEPTION_CAUGHT)
            .and_then(|v| v.get("message").cloned())
            .unwrap_or(Value::Null))
    })
}

/// Expression from a closure.
pub fn expr<F>(f: F) -> ValueBuilder
where
    F: Fn(&Exchange) -> Result<Value> + Send + Sync + 'static,
{
    ValueBuilder::new(ExpressionDefinition::Expression(Arc::new(f)))
}

/// Predicate from a closure.
pub fn pred<F>(f: F) -> ValueBuilder
where
    F: Fn(&Exchange) -> Result<bool> + Send + Sync + 'static,
{
    ValueBuilder::new(ExpressionDefinition::Predicate(Arc::new(FnPredicate(f))))
}

/// Predicate that always returns the given value.
pub fn constant_predicate(b: bool) -> ValueBuilder {
    pred(move |_| Ok(b))
}

/// Logical AND of predicates.
pub fn and(preds: Vec<ExpressionDefinition>) -> ValueBuilder {
    combine(preds, true)
}

/// Logical OR of predicates.
pub fn or(preds: Vec<ExpressionDefinition>) -> ValueBuilder {
    combine(preds, false)
}

/// Logical NOT.
pub fn not(p: impl IntoExpression) -> ValueBuilder {
    let p = p.into_expression();
    lazy_pred(vec![p], |ps, ex| Ok(!ps[0].matches(ex)?))
}

fn combine(preds: Vec<ExpressionDefinition>, all: bool) -> ValueBuilder {
    lazy_pred(preds, move |ps, ex| {
        for p in ps {
            let m = p.matches(ex)?;
            if all && !m {
                return Ok(false);
            }
            if !all && m {
                return Ok(true);
            }
        }
        Ok(all)
    })
}

/// A predicate built over sub-expressions that may need context resolution.
/// Resolution happens lazily on first evaluation using the exchange's context.
struct LazyPredicate<F> {
    defs: Vec<ExpressionDefinition>,
    resolved: std::sync::OnceLock<Result<Vec<Arc<dyn Predicate>>>>,
    f: F,
}

impl<F> Predicate for LazyPredicate<F>
where
    F: Fn(&[Arc<dyn Predicate>], &Exchange) -> Result<bool> + Send + Sync,
{
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        let r = self.resolved.get_or_init(|| {
            self.defs
                .iter()
                .map(|d| match d {
                    ExpressionDefinition::Predicate(p) => Ok(p.clone()),
                    ExpressionDefinition::Expression(e) => Ok(Arc::new(ExpressionPredicate(e.clone())) as Arc<dyn Predicate>),
                    _ => d.resolve_predicate(ex.context()?),
                })
                .collect()
        });
        match r {
            Ok(ps) => (self.f)(ps, ex),
            Err(e) => Err(e.clone()),
        }
    }
}

fn lazy_pred<F>(defs: Vec<ExpressionDefinition>, f: F) -> ValueBuilder
where
    F: Fn(&[Arc<dyn Predicate>], &Exchange) -> Result<bool> + Send + Sync + 'static,
{
    ValueBuilder::new(ExpressionDefinition::Predicate(Arc::new(LazyPredicate { defs, resolved: std::sync::OnceLock::new(), f })))
}

/// A lazily resolved expression (for language expressions used inside builders).
struct LazyExpression {
    def: ExpressionDefinition,
    resolved: std::sync::OnceLock<Result<Arc<dyn Expression>>>,
}

impl Expression for LazyExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        if let ExpressionDefinition::Expression(e) = &self.def {
            return e.evaluate(ex);
        }
        let r = self.resolved.get_or_init(|| self.def.resolve_expression(ex.context()?));
        match r {
            Ok(e) => e.evaluate(ex),
            Err(e) => Err(e.clone()),
        }
    }
}

fn lazy_expr(def: ExpressionDefinition) -> Arc<dyn Expression> {
    match def {
        ExpressionDefinition::Expression(e) => e,
        other => Arc::new(LazyExpression { def: other, resolved: std::sync::OnceLock::new() }),
    }
}

/// Fluent builder over an expression, offering predicate and transformation methods.
#[derive(Clone, Debug)]
pub struct ValueBuilder {
    pub def: ExpressionDefinition,
}

impl ValueBuilder {
    pub fn new(def: ExpressionDefinition) -> Self {
        ValueBuilder { def }
    }

    fn left(&self) -> Arc<dyn Expression> {
        lazy_expr(self.def.clone())
    }

    fn compare<F>(&self, right: impl Into<Operand>, f: F) -> ValueBuilder
    where
        F: Fn(&Value, &Value) -> bool + Send + Sync + 'static,
    {
        let l = self.left();
        let r = right.into().into_expr();
        pred(move |ex| {
            let a = l.evaluate(ex)?;
            let b = r.evaluate(ex)?;
            Ok(f(&a, &b))
        })
    }

    pub fn is_equal_to(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, values_equal)
    }
    pub fn is_equal_to_ignore_case(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| a.to_string().eq_ignore_ascii_case(&b.to_string()))
    }
    pub fn is_not_equal_to(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| !values_equal(a, b))
    }
    pub fn is_greater_than(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| compare_values(a, b) == Some(std::cmp::Ordering::Greater))
    }
    pub fn is_greater_than_or_equal_to(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| matches!(compare_values(a, b), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)))
    }
    pub fn is_less_than(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| compare_values(a, b) == Some(std::cmp::Ordering::Less))
    }
    pub fn is_less_than_or_equal_to(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| matches!(compare_values(a, b), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)))
    }
    pub fn contains(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| match a {
            Value::Array(items) => items.iter().any(|i| values_equal(i, b)),
            Value::Object(o) => o.contains_key(&b.to_string()),
            Value::Null => false,
            other => other.to_string().contains(&b.to_string()),
        })
    }
    pub fn contains_ignore_case(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| a.to_string().to_lowercase().contains(&b.to_string().to_lowercase()))
    }
    pub fn starts_with(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| !a.is_null() && a.to_string().starts_with(&b.to_string()))
    }
    pub fn ends_with(&self, v: impl Into<Operand>) -> ValueBuilder {
        self.compare(v, |a, b| !a.is_null() && a.to_string().ends_with(&b.to_string()))
    }
    pub fn regex(&self, pattern: &str) -> ValueBuilder {
        let l = self.left();
        let re = crate::regex::Regex::new(pattern);
        pred(move |ex| {
            let re = re.as_ref().map_err(|e| e.clone())?;
            let v = l.evaluate(ex)?;
            Ok(!v.is_null() && re.is_full_match(&v.to_string()))
        })
    }
    pub fn is_null(&self) -> ValueBuilder {
        let l = self.left();
        pred(move |ex| Ok(l.evaluate(ex)?.is_null()))
    }
    pub fn is_not_null(&self) -> ValueBuilder {
        let l = self.left();
        pred(move |ex| Ok(!l.evaluate(ex)?.is_null()))
    }
    /// Value is one of the given values.
    pub fn is_in(&self, values: Vec<Value>) -> ValueBuilder {
        let l = self.left();
        pred(move |ex| {
            let v = l.evaluate(ex)?;
            Ok(values.iter().any(|x| values_equal(&v, x)))
        })
    }
    /// Value is of the given type name (`string`, `integer`, `float`, `boolean`, `array`, `object`, `bytes`, `null`).
    pub fn is_instance_of(&self, type_name: &str) -> ValueBuilder {
        let l = self.left();
        let t = type_name.to_ascii_lowercase();
        pred(move |ex| {
            let v = l.evaluate(ex)?;
            Ok(v.type_name() == t || (t == "number" && matches!(v, Value::Int(_) | Value::Float(_))))
        })
    }

    // --- transformations ---

    /// Convert to string.
    pub fn convert_to_string(&self) -> ValueBuilder {
        let l = self.left();
        expr(move |ex| Ok(Value::String(l.evaluate(ex)?.to_string())))
    }
    /// Append a string suffix.
    pub fn append(&self, suffix: impl Into<Operand>) -> ValueBuilder {
        let l = self.left();
        let r = suffix.into().into_expr();
        expr(move |ex| Ok(Value::String(format!("{}{}", l.evaluate(ex)?, r.evaluate(ex)?))))
    }
    /// Prepend a string prefix.
    pub fn prepend(&self, prefix: impl Into<Operand>) -> ValueBuilder {
        let l = self.left();
        let r = prefix.into().into_expr();
        expr(move |ex| Ok(Value::String(format!("{}{}", r.evaluate(ex)?, l.evaluate(ex)?))))
    }
    /// Regex replace-all on the string value.
    pub fn regex_replace_all(&self, pattern: &str, replacement: &str) -> ValueBuilder {
        let l = self.left();
        let re = crate::regex::Regex::new(pattern);
        let rep = replacement.to_string();
        expr(move |ex| {
            let re = re.as_ref().map_err(|e| e.clone())?;
            Ok(Value::String(re.replace_all(&l.evaluate(ex)?.to_string(), &rep)))
        })
    }
    /// Split the string value by a separator into an array (like tokenize).
    pub fn tokenize(&self, separator: &str) -> ValueBuilder {
        let l = self.left();
        let sep = separator.to_string();
        expr(move |ex| {
            let v = l.evaluate(ex)?;
            Ok(match v {
                Value::Array(a) => Value::Array(a),
                Value::Null => Value::Array(vec![]),
                other => Value::Array(other.to_string().split(sep.as_str()).map(Value::from).collect()),
            })
        })
    }
    /// Apply a closure to the value.
    pub fn map<F>(&self, f: F) -> ValueBuilder
    where
        F: Fn(Value) -> Result<Value> + Send + Sync + 'static,
    {
        let l = self.left();
        expr(move |ex| f(l.evaluate(ex)?))
    }
}

/// Right-hand side operand of a comparison: a literal value or another expression.
pub enum Operand {
    Value(Value),
    Expr(ExpressionDefinition),
}

impl Operand {
    fn into_expr(self) -> Arc<dyn Expression> {
        match self {
            Operand::Value(v) => Arc::new(move |_: &Exchange| Ok(v.clone())),
            Operand::Expr(d) => lazy_expr(d),
        }
    }
}

impl From<ValueBuilder> for Operand {
    fn from(v: ValueBuilder) -> Self {
        Operand::Expr(v.def)
    }
}
impl From<ExpressionDefinition> for Operand {
    fn from(v: ExpressionDefinition) -> Self {
        Operand::Expr(v)
    }
}
macro_rules! operand_from {
    ($($t:ty),*) => { $( impl From<$t> for Operand { fn from(v: $t) -> Self { Operand::Value(Value::from(v)) } } )* };
}
operand_from!(&str, String, i32, i64, u32, u64, usize, f64, bool, Value);

/// Helper: evaluate an expression and fail with a nice message if no context etc.
pub fn evaluate_to_string(e: &dyn Expression, ex: &Exchange) -> Result<String> {
    Ok(e.evaluate(ex)?.to_string())
}

/// Helper for components that need an error when a required value is missing.
pub fn required(v: Value, what: &str) -> Result<Value> {
    if v.is_null() {
        Err(CamelError::illegal_argument(format!("{} must be specified", what)))
    } else {
        Ok(v)
    }
}
