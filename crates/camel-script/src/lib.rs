//! Scripting and expression languages for rust-camel, implemented as tree-walking interpreters
//! over a shared AST and a dynamic object model on top of `camel_core::Value`:
//!
//! | language names | language |
//! |---|---|
//! | `js`, `javascript`, `javaScript` | JavaScript (ES5.1 + common ES2015) |
//! | `groovy` | Groovy subset |
//! | `python` | Python subset (indentation based) |
//! | `ognl`, `mvel`, `spel` | Java expression languages (with Camel's root object) |
//! | `datasonnet` | DataSonnet (Jsonnet subset) with `payload` and `cml` |
//! | `csimple` | alias of the Simple language |
//! | `joor`, `java` | not supported (explains why) |
//!
//! Scripts see the exchange through bindings: `exchange` (with Java-like accessors such as
//! `exchange.getIn().setHeader(..)`), `request`/`message`, `body`, `headers`, `properties` /
//! `exchangeProperties`, `variables`, `camelContext` and `log`. When a script runs as a
//! processor ([`script`], the YAML `script` step) its changes to the message, headers,
//! properties and variables are written back to the exchange; as an expression it works on a
//! copy and only its result counts.
//!
//! Call [`register`] to add all languages to a context.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use camel_core::builder::ValueBuilder;
use camel_core::context::CamelContext;
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::{Exchange, Headers};
use camel_core::spi::{Expression, Language, Predicate, Processor};
use camel_core::value::Value;

pub mod ast;
mod builtins;
pub mod error;
mod format;
mod host;
mod interp;
mod json;
mod jsonnet;
mod lexer;
mod parser;
mod python;
mod value;

pub use ast::Dialect;

/// Registry name of the factory used by the YAML `script` step to run scripts as processors
/// (so that their exchange changes are kept).
pub const SCRIPT_PROCESSOR_FACTORY: &str = "camelScriptProcessorFactory";

/// The factory type bound under [`SCRIPT_PROCESSOR_FACTORY`]: `(language, text, options)` ->
/// a processor, or `None` when the language is not a script language of this crate.
pub type ScriptProcessorFactory = Box<dyn Fn(&str, &str, &BTreeMap<String, String>) -> Option<Result<Arc<dyn Processor>>> + Send + Sync>;

/// The dialect for a language name (`groovy`, `js`, ...).
pub fn dialect_for(name: &str) -> Option<Dialect> {
    Some(match name {
        "js" | "javascript" | "javaScript" | "JavaScript" => Dialect::Js,
        "groovy" => Dialect::Groovy,
        "python" => Dialect::Python,
        "ognl" => Dialect::Ognl,
        "mvel" => Dialect::Mvel,
        "spel" => Dialect::Spel,
        "datasonnet" => Dialect::Jsonnet,
        _ => return None,
    })
}

/// A compiled script / expression.
pub struct Script {
    dialect: Dialect,
    src: Arc<str>,
    prog: ast::Program,
    /// Run on a dedicated thread with a large stack (user-defined functions may recurse).
    deep: bool,
    result_type: Option<String>,
    output_media_type: String,
    body_media_type: String,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Script[{}: {}]", self.dialect.display_name(), self.src)
    }
}

fn has_functions(d: Dialect, src: &str) -> bool {
    match d {
        Dialect::Js => src.contains("function") || src.contains("=>") || src.contains("class"),
        Dialect::Groovy | Dialect::Mvel => src.contains("->") || src.contains("def ") || src.contains(") {") || src.contains("){"),
        Dialect::Python => src.contains("def ") || src.contains("lambda"),
        Dialect::Jsonnet => src.contains("function") || src.contains("local") && src.contains('('),
        Dialect::Ognl | Dialect::Spel => false,
    }
}

fn parse_spel(text: &str) -> std::result::Result<ast::Program, error::SyntaxError> {
    if !text.contains("#{") {
        return parser::parse_program(text, Dialect::Spel);
    }
    let mut parts = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match text[i..].find("#{") {
            Some(off) => {
                let start = i + off;
                if start > i {
                    parts.push(ast::TplPart::Lit(text[i..start].to_string()));
                }
                let end = lexer::matching_brace(text, start + 2).ok_or(error::SyntaxError { pos: start, msg: "unterminated #{ in template".into() })?;
                let e = parser::parse_embedded(&text[start + 2..end], start + 2, Dialect::Spel)?;
                parts.push(ast::TplPart::Expr(e, None, None));
                i = end + 1;
            }
            None => {
                parts.push(ast::TplPart::Lit(text[i..].to_string()));
                break;
            }
        }
    }
    let e = if parts.len() == 1 && matches!(parts[0], ast::TplPart::Expr(..)) {
        match parts.pop() {
            Some(ast::TplPart::Expr(e, _, _)) => e,
            _ => unreachable!(),
        }
    } else {
        ast::Expr::Template(parts)
    };
    Ok(ast::Program { dialect: Dialect::Spel, body: vec![ast::Stmt::Expr(e)] })
}

struct Parts {
    exchange_id: String,
    pattern: String,
    message_id: String,
    context: Option<CamelContext>,
    headers: Headers,
    body: Value,
    properties: BTreeMap<String, Value>,
    exception: Option<(String, String)>,
    route_id: Option<String>,
}

type Outcome = Result<(Value, Headers, Value, BTreeMap<String, Value>)>;

impl Script {
    /// Compile a script in the given dialect. Options: `resultType` (convert the result),
    /// DataSonnet `bodyMediaType` / `outputMediaType`.
    pub fn compile(dialect: Dialect, text: &str, options: &BTreeMap<String, String>) -> Result<Script> {
        let parsed = match dialect {
            Dialect::Python => python::parse_program(text),
            Dialect::Jsonnet => jsonnet::parse_program(text),
            Dialect::Spel => parse_spel(text),
            d => parser::parse_program(text, d),
        };
        let prog = parsed.map_err(|e| e.into_camel(dialect, text))?;
        let opt = |k: &str| options.get(k).filter(|v| !v.is_empty()).cloned();
        Ok(Script {
            dialect,
            src: Arc::from(text),
            prog,
            deep: has_functions(dialect, text),
            result_type: opt("resultType"),
            output_media_type: opt("outputMediaType").unwrap_or_else(|| "application/json".into()),
            body_media_type: opt("bodyMediaType").unwrap_or_else(|| "application/json".into()),
        })
    }

    /// Compile by language name.
    pub fn compile_named(language: &str, text: &str) -> Result<Script> {
        let d = dialect_for(language).ok_or_else(|| CamelError::no_such_language(language))?;
        Script::compile(d, text, &BTreeMap::new())
    }

    /// The script dialect.
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    fn parts_of(ex: &Exchange) -> Parts {
        Parts {
            exchange_id: ex.exchange_id.clone(),
            pattern: format!("{:?}", ex.pattern),
            message_id: ex.message.message_id.clone(),
            context: ex.context.clone(),
            headers: Headers::new(),
            body: Value::Null,
            properties: BTreeMap::new(),
            exception: ex.exception.as_ref().map(|e| (e.kind.clone(), e.message.clone())),
            route_id: ex.from_route_id.clone(),
        }
    }

    /// Evaluate against a copy of the exchange (changes are discarded).
    pub fn evaluate_copy(&self, ex: &Exchange) -> Result<Value> {
        let mut parts = Self::parts_of(ex);
        parts.headers = ex.message.headers.clone();
        parts.body = ex.message.body.clone();
        parts.properties = ex.properties.clone();
        self.run(parts).map(|(v, ..)| v)
    }

    /// Evaluate and write the changes of message body, headers, properties and variables back.
    pub fn evaluate_mut(&self, ex: &mut Exchange) -> Result<Value> {
        let mut parts = Self::parts_of(ex);
        parts.headers = std::mem::take(&mut ex.message.headers);
        parts.body = std::mem::take(&mut ex.message.body);
        parts.properties = std::mem::take(&mut ex.properties);
        // keep copies to restore on failure
        let (h, b, p) = (parts.headers.clone(), parts.body.clone(), parts.properties.clone());
        match self.run(parts) {
            Ok((v, headers, body, props)) => {
                ex.message.headers = headers;
                ex.message.body = body;
                ex.properties = props;
                Ok(v)
            }
            Err(e) => {
                ex.message.headers = h;
                ex.message.body = b;
                ex.properties = p;
                Err(e)
            }
        }
    }

    fn run(&self, parts: Parts) -> Outcome {
        if !self.deep {
            return self.run_inner(parts, 40);
        }
        std::thread::scope(|s| {
            let h = std::thread::Builder::new()
                .name("camel-script".into())
                .stack_size(256 << 20)
                .spawn_scoped(s, move || self.run_inner(parts, interp::MAX_DEPTH))
                .map_err(|e| CamelError::new(kinds::EXPRESSION_EVALUATION, format!("cannot start script thread: {}", e)))?;
            h.join().unwrap_or_else(|_| Err(CamelError::new(kinds::EXPRESSION_EVALUATION, format!("{} script panicked", self.dialect.display_name()))))
        })
    }

    fn run_inner(&self, parts: Parts, max_depth: usize) -> Outcome {
        let state = host::HostState::new(parts.exchange_id, parts.pattern, parts.message_id, parts.context, parts.headers, parts.body, parts.properties, parts.exception, parts.route_id);
        let host = Rc::new(state);
        let mut it = interp::Interp::new(self.dialect, self.src.clone(), host.clone());
        it.max_depth = max_depth;
        if self.dialect == Dialect::Jsonnet {
            let payload = self.payload(&host);
            it.globals.define("payload", payload);
        }
        let r = it.run_program(&self.prog);
        let result = match r {
            Ok(v) | Err(interp::Ctrl::Return(v)) => self.finish(&mut it, v),
            Err(interp::Ctrl::Throw(e)) => Err(self.to_error(&it, &e)),
            Err(_) => Ok(Value::Null),
        };
        it.teardown();
        drop(it);
        let result = result?;
        let state = Rc::try_unwrap(host).map_err(|_| CamelError::illegal_state("script state still referenced"))?;
        let (headers, body, props) = state.into_parts();
        Ok((result, headers, body, props))
    }

    fn payload(&self, host: &host::HostState) -> value::V {
        let body = host.body_value();
        let json = self.body_media_type.contains("json");
        match &body {
            Value::String(_) | Value::Bytes(_) if json => {
                let s = body.as_str().unwrap_or("").to_string();
                if s.trim().is_empty() {
                    return value::V::Null;
                }
                json::parse(&s).unwrap_or_else(|_| value::V::string(s))
            }
            other => value::from_value(other),
        }
    }

    fn finish(&self, it: &mut interp::Interp, v: value::V) -> Result<Value> {
        use value::{Host, V};
        let v = match &v {
            V::Host(h) => match &**h {
                Host::Headers | Host::Properties | Host::Variables => host::snapshot(it, h).unwrap_or(V::Null),
                Host::Message | Host::Exchange | Host::Root => it.host.body(),
                _ => V::Null,
            },
            _ => v,
        };
        let mut out = value::to_value(&v);
        if self.dialect == Dialect::Jsonnet && self.result_type.is_none() {
            let m = self.output_media_type.to_ascii_lowercase();
            if m.contains("json") {
                out = Value::String(json::stringify(&v, &json::JsonOpts { indent: None, item_sep: "", key_sep: "", sort_keys: true, js: false, newline: "\n" }).unwrap_or_else(|| "null".into()));
            } else if m.starts_with("text/") {
                out = Value::String(it.display(&v));
            }
        }
        if let Some(t) = &self.result_type {
            out = camel_core::language::support::convert_to_type(&out, t)?;
        }
        Ok(out)
    }

    fn to_error(&self, it: &interp::Interp, e: &value::V) -> CamelError {
        use value::{ObjKind, V};
        let lang = self.dialect.display_name();
        if let V::Obj(o) = e {
            let o = o.borrow();
            if o.kind == ObjKind::Error {
                let name = o.props.get("name").map(|n| it.display(n)).unwrap_or_else(|| "Error".into());
                let msg = o.props.get("message").map(|n| it.display(n)).unwrap_or_default();
                let at = match (o.props.get("lineNumber"), o.props.get("columnNumber")) {
                    (Some(l), Some(c)) => format!(" (line {}, column {})", it.display(l), it.display(c)),
                    _ => String::new(),
                };
                // Java exception names thrown by scripts keep their kind (onException / doCatch)
                let kind = if name.ends_with("Exception") && !name.contains(' ') { name.clone() } else { kinds::EXPRESSION_EVALUATION.to_string() };
                return CamelError::new(kind, format!("{} error: {}: {}{}", lang, name, msg, at));
            }
        }
        CamelError::new(kinds::EXPRESSION_EVALUATION, format!("{} error: uncaught exception: {}", lang, it.display(e)))
    }
}

impl Expression for Script {
    fn evaluate(&self, exchange: &Exchange) -> Result<Value> {
        self.evaluate_copy(exchange)
    }
}

/// A script running as a processor (exchange changes are kept; the result is ignored).
pub struct ScriptProcessor(pub Arc<Script>);

impl Processor for ScriptProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.0.evaluate_mut(exchange).map(|_| ())
    }
}

/// A processor running a script in the named language, e.g.
/// `.process(script("groovy", "request.headers.foo = 'bar'")?)`.
pub fn script(language: &str, text: &str) -> Result<ScriptProcessor> {
    Ok(ScriptProcessor(Arc::new(Script::compile_named(language, text)?)))
}

/// A script language registered on the context.
pub struct ScriptLanguage {
    name: &'static str,
    dialect: Dialect,
}

impl ScriptLanguage {
    pub fn new(name: &'static str, dialect: Dialect) -> ScriptLanguage {
        ScriptLanguage { name, dialect }
    }
}

impl Language for ScriptLanguage {
    fn name(&self) -> &str {
        self.name
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(Script::compile(self.dialect, text, options)?))
    }
}

/// `csimple` (compiled Simple): an alias of the Simple language.
pub struct CSimpleLanguage;

impl Language for CSimpleLanguage {
    fn name(&self) -> &str {
        "csimple"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        camel_core::language::simple::SimpleLanguage.create_expression(text, options)
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        camel_core::language::simple::SimpleLanguage.create_predicate(text, options)
    }
}

/// `joor` / `java`: not supported.
pub struct JoorLanguage(pub &'static str);

impl Language for JoorLanguage {
    fn name(&self) -> &str {
        self.0
    }
    fn create_expression(&self, text: &str, _options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Err(CamelError::new(
            kinds::UNSUPPORTED_OPERATION,
            format!(
                "The {} language is not supported by rust-camel: it compiles Java source code at runtime with the JDK compiler and runs it on the JVM, \
                 neither of which exists in a native Rust runtime. Use groovy, js, python or simple instead (expression: {})",
                self.0,
                text.trim()
            ),
        ))
    }
}

/// Register all script languages (and the YAML `script` processor factory) on a context.
pub fn register(ctx: &CamelContext) {
    for (name, d) in [
        ("js", Dialect::Js),
        ("javascript", Dialect::Js),
        ("javaScript", Dialect::Js),
        ("groovy", Dialect::Groovy),
        ("python", Dialect::Python),
        ("ognl", Dialect::Ognl),
        ("mvel", Dialect::Mvel),
        ("spel", Dialect::Spel),
        ("datasonnet", Dialect::Jsonnet),
    ] {
        ctx.add_language(ScriptLanguage::new(name, d));
    }
    ctx.add_language(CSimpleLanguage);
    ctx.add_language(JoorLanguage("joor"));
    ctx.add_language(JoorLanguage("java"));
    let factory: ScriptProcessorFactory = Box::new(|language: &str, text: &str, options: &BTreeMap<String, String>| {
        let d = dialect_for(language)?;
        Some(Script::compile(d, text, options).map(|s| Arc::new(ScriptProcessor(Arc::new(s))) as Arc<dyn Processor>))
    });
    ctx.registry().bind(SCRIPT_PROCESSOR_FACTORY, factory);
}

fn lang(name: &str, text: impl Into<String>) -> ValueBuilder {
    camel_core::builder::language(name, text)
}

/// JavaScript expression for the Rust DSL.
pub fn js(text: impl Into<String>) -> ValueBuilder {
    lang("js", text)
}
/// Groovy expression for the Rust DSL.
pub fn groovy(text: impl Into<String>) -> ValueBuilder {
    lang("groovy", text)
}
/// Python expression for the Rust DSL.
pub fn python(text: impl Into<String>) -> ValueBuilder {
    lang("python", text)
}
/// OGNL expression for the Rust DSL.
pub fn ognl(text: impl Into<String>) -> ValueBuilder {
    lang("ognl", text)
}
/// MVEL expression for the Rust DSL.
pub fn mvel(text: impl Into<String>) -> ValueBuilder {
    lang("mvel", text)
}
/// SpEL expression for the Rust DSL.
pub fn spel(text: impl Into<String>) -> ValueBuilder {
    lang("spel", text)
}
/// DataSonnet expression for the Rust DSL.
pub fn datasonnet(text: impl Into<String>) -> ValueBuilder {
    lang("datasonnet", text)
}

/// Evaluate a script against an exchange (convenience for tests and tools).
pub fn eval(language: &str, text: &str, ex: &Exchange) -> Result<Value> {
    Script::compile_named(language, text)?.evaluate_copy(ex)
}
