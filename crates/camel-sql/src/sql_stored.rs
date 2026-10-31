//! The `sql-stored` component (Camel `camel-sql` stored procedures): call a stored
//! procedure or function described by a template.
//!
//! `sql-stored:SUBNUMBERS(INTEGER ${headers.num1},INTEGER ${headers.num2},OUT INTEGER resultofsub)?dataSource=#ds`
//!
//! Template syntax (as in Camel): `NAME(param, param, ...)` where
//!
//! * IN parameters are `[IN] ['name'] TYPE[(scale)] ['typeName'] valueSource`,
//! * OUT parameters are `OUT TYPE[(scale)] name ['typeName']`,
//! * INOUT parameters are `INOUT TYPE[(scale)] valueSource name ['typeName']`.
//!
//! `TYPE` is a `java.sql.Types` name (`INTEGER`, `VARCHAR`, `DECIMAL(10)`,
//! `org.example.Types.INTEGER` — the last segment is used) or its integer code; IN values
//! are converted to it. A value source is a Simple expression (`${header.num}`,
//! `${body.qty}`, `${exchangeProperty.x}`), `:#name` (body map entry, then header,
//! exchange property, variable), `:#${simple}`, or a literal (`5`, `'text'`).
//!
//! The procedure is called as `CALL name(?, ?, NULL)` (OUT positions passed as NULL, as
//! PostgreSQL 14+ expects); with `function=true` as `SELECT * FROM name(?, ?)` (IN and INOUT
//! arguments only) and the first OUT template parameter names the return value.
//!
//! Result: the OUT / INOUT values as a map (`{resultofsub=2}`) in the body, or in the header
//! named by `outputHeader`; result sets (functions returning `SETOF` / `TABLE`, procedures
//! whose body runs a plain SELECT on the embedded engine) follow `outputType`
//! (`SelectList` — default — list of row maps, `SelectOne` — one row map or single value).
//! When a template declares OUT parameters and the call also returns a result set, the
//! rows are added to the map under `#result-set-1` (unless `outputType` is set, which
//! makes the rows the result). Header `CamelSqlStoredUpdateCount`: rows affected.
//!
//! Options: `dataSource`, `function` (default false), `outputType`, `outputHeader`,
//! `noop` (keep the body), `batch` (the body is a list; each element — a map or a value —
//! is the parameter source of one call, all in one transaction; the body is kept and the
//! header holds the total update count), `useMessageBodyForTemplate` (the template is the
//! message body), `templateOptions.*` / `outputClass` / `lazyStartProducer` (accepted).
//! The template may be loaded with `file:` / `classpath:`; header `CamelSqlStoredTemplate`
//! overrides it per exchange.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{ObjectMap, Value};

use crate::datasource::{DataSource, ExecuteOptions, QueryResult};
use crate::params::lookup_named;
use crate::sql::{resolve_endpoint_datasource, resolve_query_text, select_body, OutputType};
use crate::transaction::with_connection;

/// Header with the update count of the call.
pub const SQL_STORED_UPDATE_COUNT: &str = "CamelSqlStoredUpdateCount";
/// Header with a template overriding the endpoint's.
pub const SQL_STORED_TEMPLATE: &str = "CamelSqlStoredTemplate";

/// Parameter direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    InOut,
}

/// Where an IN value comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueSource {
    /// A Simple expression (`${header.x}`).
    Simple(String),
    /// `:#name`.
    Named(String),
    /// A literal value.
    Literal(Value),
}

/// A template parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateParam {
    pub direction: Direction,
    /// The `java.sql.Types` name (upper case, without scale).
    pub sql_type: String,
    pub scale: Option<i64>,
    /// Name (OUT/INOUT result name, optional quoted IN name).
    pub name: Option<String>,
    pub type_name: Option<String>,
    pub value: Option<ValueSource>,
}

/// A parsed `NAME(params...)` template.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub procedure: String,
    pub params: Vec<TemplateParam>,
}

fn jdbc_type_name(code: i64) -> Option<&'static str> {
    Some(match code {
        4 => "INTEGER",
        -5 => "BIGINT",
        5 => "SMALLINT",
        -6 => "TINYINT",
        12 => "VARCHAR",
        1 => "CHAR",
        -1 => "LONGVARCHAR",
        -9 => "NVARCHAR",
        8 => "DOUBLE",
        6 => "FLOAT",
        7 => "REAL",
        3 => "DECIMAL",
        2 => "NUMERIC",
        16 => "BOOLEAN",
        -7 => "BIT",
        91 => "DATE",
        92 => "TIME",
        93 => "TIMESTAMP",
        2005 => "CLOB",
        2004 => "BLOB",
        -2 => "BINARY",
        -3 => "VARBINARY",
        1111 => "OTHER",
        _ => return None,
    })
}

/// Split at top-level `sep` (outside `()`, `{}` and quotes).
fn split_top(s: &str, sep: impl Fn(char) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            c if depth == 0 && sep(c) => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn bad(msg: impl Into<String>) -> CamelError {
    CamelError::illegal_argument(msg)
}

fn parse_type(tok: &str) -> Result<(String, Option<i64>)> {
    let (base, scale) = match tok.find('(') {
        Some(i) => {
            let inner = tok[i + 1..].trim_end_matches(')');
            let scale = inner.split(',').next().and_then(|s| s.trim().parse::<i64>().ok());
            (&tok[..i], scale)
        }
        None => (tok, None),
    };
    let name = if let Ok(code) = base.parse::<i64>() {
        jdbc_type_name(code).ok_or_else(|| bad(format!("Unknown java.sql.Types code {}", code)))?.to_string()
    } else {
        base.rsplit('.').next().unwrap_or(base).to_ascii_uppercase()
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad(format!("Invalid SQL type '{}' in stored procedure template", tok)));
    }
    Ok((name, scale))
}

fn unquote(s: &str) -> Option<String> {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        Some(t[1..t.len() - 1].to_string())
    } else {
        None
    }
}

fn parse_value_source(tok: &str) -> ValueSource {
    let t = tok.trim();
    if let Some(name) = t.strip_prefix(":#") {
        if name.starts_with("${") {
            return ValueSource::Simple(name.to_string());
        }
        return ValueSource::Named(name.to_string());
    }
    if t.contains("${") {
        return ValueSource::Simple(t.to_string());
    }
    if let Some(s) = unquote(t) {
        return ValueSource::Literal(Value::String(s));
    }
    if let Ok(i) = t.parse::<i64>() {
        return ValueSource::Literal(Value::Int(i));
    }
    if let Ok(f) = t.parse::<f64>() {
        return ValueSource::Literal(Value::Float(f));
    }
    match t.to_ascii_lowercase().as_str() {
        "true" => ValueSource::Literal(Value::Bool(true)),
        "false" => ValueSource::Literal(Value::Bool(false)),
        "null" => ValueSource::Literal(Value::Null),
        _ => ValueSource::Literal(Value::String(t.to_string())),
    }
}

/// Parse a stored procedure template.
pub fn parse_template(text: &str) -> Result<Template> {
    let t = text.trim().trim_end_matches(';').trim();
    let open = t.find('(').ok_or_else(|| bad(format!("Invalid stored procedure template (expected NAME(...)): {}", t)))?;
    if !t.ends_with(')') {
        return Err(bad(format!("Invalid stored procedure template (missing closing parenthesis): {}", t)));
    }
    let procedure = t[..open].trim().to_string();
    if procedure.is_empty() || !procedure.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"') {
        return Err(bad(format!("Invalid stored procedure name '{}'", procedure)));
    }
    let inner = &t[open + 1..t.len() - 1];
    let mut params = Vec::new();
    for p in split_top(inner, |c| c == ',') {
        let mut toks = split_top(&p, char::is_whitespace);
        let direction = match toks.first().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("OUT") => Direction::Out,
            Some("INOUT") => Direction::InOut,
            Some("IN") => Direction::In,
            _ => {
                toks.insert(0, "IN".into());
                Direction::In
            }
        };
        toks.remove(0);
        let mut it = toks.into_iter().peekable();
        let param = match direction {
            Direction::In => {
                let name = match it.peek().and_then(|s| unquote(s)) {
                    Some(n) => {
                        it.next();
                        Some(n)
                    }
                    None => None,
                };
                let ty = it.next().ok_or_else(|| bad(format!("Missing SQL type in template parameter '{}'", p)))?;
                let (sql_type, scale) = parse_type(&ty)?;
                let rest: Vec<String> = it.collect();
                let (type_name, value) = match rest.as_slice() {
                    [v] => (None, v.clone()),
                    [tn, v] if unquote(tn).is_some() => (unquote(tn), v.clone()),
                    [] => return Err(bad(format!("Missing value source in template IN parameter '{}'", p))),
                    _ => (None, rest.join(" ")),
                };
                TemplateParam { direction, sql_type, scale, name, type_name, value: Some(parse_value_source(&value)) }
            }
            Direction::Out => {
                let ty = it.next().ok_or_else(|| bad(format!("Missing SQL type in template parameter '{}'", p)))?;
                let (sql_type, scale) = parse_type(&ty)?;
                let name = it.next().ok_or_else(|| bad(format!("Missing name of template OUT parameter '{}'", p)))?;
                let type_name = it.next().and_then(|s| unquote(&s));
                TemplateParam { direction, sql_type, scale, name: Some(name), type_name, value: None }
            }
            Direction::InOut => {
                let ty = it.next().ok_or_else(|| bad(format!("Missing SQL type in template parameter '{}'", p)))?;
                let (sql_type, scale) = parse_type(&ty)?;
                let value = it.next().ok_or_else(|| bad(format!("Missing value source of template INOUT parameter '{}'", p)))?;
                let name = it.next().ok_or_else(|| bad(format!("Missing name of template INOUT parameter '{}'", p)))?;
                let type_name = it.next().and_then(|s| unquote(&s));
                TemplateParam { direction, sql_type, scale, name: Some(name), type_name, value: Some(parse_value_source(&value)) }
            }
        };
        params.push(param);
    }
    Ok(Template { procedure, params })
}

/// Convert a value to a `java.sql.Types` type.
pub fn convert_to_sql_type(v: Value, sql_type: &str) -> Result<Value> {
    if v.is_null() {
        return Ok(v);
    }
    let fail = |v: &Value| bad(format!("Cannot convert '{}' to SQL type {}", v, sql_type));
    Ok(match sql_type {
        "INTEGER" | "INT" | "SMALLINT" | "TINYINT" | "BIGINT" => match &v {
            Value::Int(_) => v,
            Value::Float(f) => Value::Int(*f as i64),
            Value::Bool(b) => Value::Int(*b as i64),
            other => {
                let s = other.to_string();
                let t = s.trim();
                t.parse::<i64>().map(Value::Int).or_else(|_| t.parse::<f64>().map(|f| Value::Int(f as i64))).map_err(|_| fail(other))?
            }
        },
        "DOUBLE" | "FLOAT" | "REAL" | "DECIMAL" | "NUMERIC" => match &v {
            Value::Float(_) => v,
            Value::Int(i) => Value::Float(*i as f64),
            other => other.to_string().trim().parse::<f64>().map(Value::Float).map_err(|_| fail(other))?,
        },
        "BOOLEAN" | "BIT" => match &v {
            Value::Bool(_) => v,
            Value::Int(i) => Value::Bool(*i != 0),
            other => match other.to_string().trim().to_ascii_lowercase().as_str() {
                "true" | "t" | "1" | "yes" | "y" => Value::Bool(true),
                "false" | "f" | "0" | "no" | "n" => Value::Bool(false),
                _ => return Err(fail(other)),
            },
        },
        "VARCHAR" | "CHAR" | "LONGVARCHAR" | "NVARCHAR" | "NCHAR" | "LONGNVARCHAR" | "CLOB" | "NCLOB" => match v {
            Value::String(_) => v,
            Value::Bytes(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
            other => Value::String(other.to_string()),
        },
        _ => v,
    })
}

/// The `sql-stored` component.
#[derive(Default)]
pub struct SqlStoredComponent {
    default_datasource: Option<Arc<dyn DataSource>>,
}

impl SqlStoredComponent {
    pub fn new() -> Self {
        Self::default()
    }
    /// A component whose endpoints use this data source unless `dataSource` is set.
    pub fn with_datasource(ds: Arc<dyn DataSource>) -> Self {
        SqlStoredComponent { default_datasource: Some(ds) }
    }
}

/// Endpoint configuration.
#[derive(Clone)]
pub struct SqlStoredConfig {
    pub template: Option<Template>,
    pub datasource: Arc<dyn DataSource>,
    pub function: bool,
    pub output_type: Option<OutputType>,
    pub output_header: Option<String>,
    pub noop: bool,
    pub batch: bool,
    pub use_message_body_for_template: bool,
    pub template_options: BTreeMap<String, String>,
}

impl Component for SqlStoredComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let use_body = params.take_bool("useMessageBodyForTemplate", false)?;
        let text = resolve_query_text(remaining)?;
        let template = if use_body {
            // the template comes from the message body; the uri path is ignored
            None
        } else if text.trim().is_empty() {
            return Err(bad("A stored procedure template must be specified (or use useMessageBodyForTemplate=true)"));
        } else {
            Some(parse_template(&text)?)
        };
        let ds_opt = params.take("dataSource").or_else(|| params.take("dataSourceRef"));
        let datasource = resolve_endpoint_datasource(ctx, ds_opt, self.default_datasource.as_ref())?;
        let output_type = match params.take("outputType") {
            Some(s) => Some(OutputType::parse(&s)?),
            None => None,
        };
        let mut template_options = params.take_prefixed("templateOptions.");
        if let Some(v) = params.take("templateOptions") {
            template_options.insert(String::new(), v);
        }
        for k in ["outputClass", "lazyStartProducer"] {
            params.take(k);
        }
        let cfg = SqlStoredConfig {
            template,
            datasource,
            function: params.take_bool("function", false)?,
            output_type,
            output_header: params.take("outputHeader").filter(|s| !s.is_empty()),
            noop: params.take_bool("noop", false)?,
            batch: params.take_bool("batch", false)?,
            use_message_body_for_template: use_body,
            template_options,
        };
        Ok(Arc::new(SqlStoredEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `sql-stored:` endpoint (producer only).
pub struct SqlStoredEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<SqlStoredConfig>,
}

impl SqlStoredEndpoint {
    pub fn config(&self) -> &SqlStoredConfig {
        &self.cfg
    }
}

impl Endpoint for SqlStoredEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SqlStoredProducer { cfg: self.cfg.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SqlStoredProducer {
    cfg: Arc<SqlStoredConfig>,
    ctx: WeakContext,
}

impl Service for SqlStoredProducer {}

/// The SQL statement for a template call.
pub fn call_sql(t: &Template, function: bool) -> String {
    if function {
        let args: Vec<&str> = t.params.iter().filter(|p| p.direction != Direction::Out).map(|_| "?").collect();
        format!("SELECT * FROM {}({})", t.procedure, args.join(", "))
    } else {
        let args: Vec<&str> = t.params.iter().map(|p| if p.direction == Direction::Out { "NULL" } else { "?" }).collect();
        format!("CALL {}({})", t.procedure, args.join(", "))
    }
}

impl SqlStoredProducer {
    fn evaluate(&self, ex: &Exchange, src: &ValueSource, body_map: Option<&ObjectMap>) -> Result<Value> {
        match src {
            ValueSource::Literal(v) => Ok(v.clone()),
            ValueSource::Named(n) => Ok(lookup_named(ex, n, body_map).unwrap_or(Value::Null)),
            ValueSource::Simple(e) => {
                let ctx = ex.context.clone().or_else(|| self.ctx.upgrade()).ok_or_else(|| CamelError::illegal_state("No CamelContext to evaluate a Simple expression"))?;
                ctx.resolve_language("simple")?.create_expression(e, &BTreeMap::new())?.evaluate(ex)
            }
        }
    }

    /// Values of the IN / INOUT parameters, evaluated against `ex` (whose body is the
    /// parameter source).
    fn bind(&self, t: &Template, ex: &Exchange) -> Result<Vec<Value>> {
        let body_map = ex.message.body.as_object();
        let mut out = Vec::new();
        for p in &t.params {
            if let Some(src) = &p.value {
                let v = self.evaluate(ex, src, body_map)?;
                out.push(convert_to_sql_type(v, &p.sql_type)?);
            }
        }
        Ok(out)
    }

    fn result(&self, t: &Template, r: &QueryResult) -> Result<Option<Value>> {
        let outs: Vec<&TemplateParam> = t.params.iter().filter(|p| p.direction != Direction::In).collect();
        let explicit = self.cfg.output_type;
        let single_row = r.rows.len() <= 1;
        if !outs.is_empty() && r.is_query && (single_row || explicit.is_none()) {
            let mut m = ObjectMap::new();
            let row = r.rows.first();
            for (k, p) in outs.iter().enumerate() {
                let name = p.name.clone().unwrap_or_else(|| format!("out{}", k + 1));
                let v = row
                    .and_then(|row| {
                        r.columns.iter().position(|c| c.eq_ignore_ascii_case(&name)).and_then(|i| row.get(i)).or_else(|| row.get(k))
                    })
                    .cloned()
                    .unwrap_or(Value::Null);
                m.insert(name, convert_to_sql_type(v, &p.sql_type).unwrap_or(Value::Null));
            }
            if !single_row {
                m.insert("#result-set-1".into(), Value::Array(r.rows_as_values()));
            }
            return Ok(Some(Value::Object(m)));
        }
        if r.is_query {
            return select_body(r, explicit.unwrap_or(OutputType::SelectList));
        }
        Ok(Some(Value::Object(ObjectMap::new())))
    }
}

impl Processor for SqlStoredProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let header_template = ex.message.headers.get(SQL_STORED_TEMPLATE).filter(|v| !v.is_null()).map(|v| v.to_string());
        let template = match (header_template, &cfg.template) {
            (Some(h), _) => parse_template(&resolve_query_text(&h)?)?,
            (None, _) if cfg.use_message_body_for_template => parse_template(&ex.body_as::<String>()?)?,
            (None, Some(t)) => t.clone(),
            (None, None) => return Err(bad("No stored procedure template")),
        };
        let sql = call_sql(&template, cfg.function);
        let opts = ExecuteOptions::default();
        if cfg.batch {
            let items = match &ex.message.body {
                Value::Array(a) => a.clone(),
                Value::Null => Vec::new(),
                other => vec![other.clone()],
            };
            let mut calls = Vec::with_capacity(items.len());
            for item in items {
                let mut copy = ex.clone();
                copy.message.body = item;
                calls.push(self.bind(&template, &copy)?);
            }
            let ds = cfg.datasource.clone();
            let total = crate::transaction::run_in_transaction(&ds, || {
                with_connection(&ds, |c| {
                    let mut total = 0i64;
                    for vals in &calls {
                        total += c.execute_with(&sql, vals, &opts)?.update_count.max(0);
                    }
                    Ok(total)
                })
            })?;
            ex.set_header(SQL_STORED_UPDATE_COUNT, total);
            return Ok(());
        }
        let vals = self.bind(&template, ex)?;
        camel_core::camel_debug!("sql-stored", "Calling {} with {} parameter(s)", sql, vals.len());
        let r = with_connection(&cfg.datasource, |c| c.execute_with(&sql, &vals, &opts))?;
        let count = if r.is_query && r.command == "SELECT" { r.rows.len() as i64 } else { r.update_count.max(0) };
        ex.set_header(SQL_STORED_UPDATE_COUNT, count);
        if cfg.noop {
            return Ok(());
        }
        if let Some(v) = self.result(&template, &r)? {
            match &cfg.output_header {
                Some(h) => ex.set_header(h.clone(), v),
                None => ex.message.body = v,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        let t = parse_template("SUBNUMBERS(INTEGER ${headers.num1},INTEGER ${headers.num2},OUT INTEGER resultofsub)").unwrap();
        assert_eq!(t.procedure, "SUBNUMBERS");
        assert_eq!(t.params.len(), 3);
        assert_eq!(t.params[0].value, Some(ValueSource::Simple("${headers.num1}".into())));
        assert_eq!(t.params[2].direction, Direction::Out);
        assert_eq!(t.params[2].name.as_deref(), Some("resultofsub"));
        assert_eq!(call_sql(&t, false), "CALL SUBNUMBERS(?, ?, NULL)");
        assert_eq!(call_sql(&t, true), "SELECT * FROM SUBNUMBERS(?, ?)");
        let t = parse_template("MYFUNC('param1' org.example.Types.INTEGER(10) 'com.Type' :#srcValue, INOUT DECIMAL(10) ${headers.in} outheader, IN 12 'x', OUT -5 big 'java.lang.Long')").unwrap();
        assert_eq!(t.params[0].name.as_deref(), Some("param1"));
        assert_eq!(t.params[0].sql_type, "INTEGER");
        assert_eq!(t.params[0].scale, Some(10));
        assert_eq!(t.params[0].type_name.as_deref(), Some("com.Type"));
        assert_eq!(t.params[0].value, Some(ValueSource::Named("srcValue".into())));
        assert_eq!(t.params[1].direction, Direction::InOut);
        assert_eq!(t.params[1].name.as_deref(), Some("outheader"));
        assert_eq!(t.params[2].sql_type, "VARCHAR");
        assert_eq!(t.params[2].value, Some(ValueSource::Literal(Value::from("x"))));
        assert_eq!(t.params[3].sql_type, "BIGINT");
        assert!(parse_template("NOPARENS").is_err());
        assert!(parse_template("P(OUT INTEGER)").is_err());
        assert!(parse_template("P(INTEGER)").is_err());
        assert_eq!(parse_template("NOARGS()").unwrap().params.len(), 0);
        assert_eq!(convert_to_sql_type(Value::from("42"), "INTEGER").unwrap(), Value::Int(42));
        assert_eq!(convert_to_sql_type(Value::Int(1), "VARCHAR").unwrap(), Value::from("1"));
        assert!(convert_to_sql_type(Value::from("x"), "INTEGER").is_err());
    }
}
