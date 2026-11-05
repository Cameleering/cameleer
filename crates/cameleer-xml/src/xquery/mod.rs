//! An XQuery 1.0 processor (with common XQuery 3.x conveniences) built on the cameleer-xml
//! DOM and the XPath 1.0 axis machinery.
//!
//! Supported:
//!
//! * Prolog: `xquery version`, `declare namespace`, `declare default element|function
//!   namespace`, `declare variable $v [as T] := expr` / `external [:= default]`,
//!   `declare function local:f($a as T, ...) as T { ... }` (recursive), `declare
//!   boundary-space`, `declare default order empty`, `declare option` (ignored), other
//!   setters accepted and ignored. No schema import or schema types.
//! * FLWOR: `for $x [as T] [at $i] in`, `let $x [as T] :=`, `where`, `[stable] order by ...
//!   [ascending|descending] [empty greatest|least]`, `return`; XQuery 3.0 `count $n` and
//!   `group by $k [:= expr]`.
//! * `some`/`every ... satisfies`, `if/then/else`, `typeswitch`, XQuery 3.0
//!   `try { } catch * | err:CODE { }` (with `$err:code`, `$err:description`).
//! * The XPath 2.0 expression language: sequences, `to` ranges, general (`=`, `<`...) and
//!   value (`eq`, `lt`...) comparisons, node comparisons (`is`, `<<`, `>>`), arithmetic with
//!   integer/decimal/double promotion (`div`, `idiv`, `mod`), `union`/`|`, `intersect`,
//!   `except`, `instance of`, `treat as`, `castable as`, `cast as`, path expressions over all
//!   13 axes with name tests, wildcards (`*`, `p:*`, `*:l`) and kind tests (`element(n)`,
//!   `attribute()`, `document-node()`, `text()`...), predicates, filter expressions; XQuery 3
//!   `||` and simple map `!`.
//! * Constructors: direct elements with attributes (`{expr}` value templates), namespace
//!   declarations, nested content, enclosed expressions, CDATA, entity/character
//!   references, direct comments and PIs; computed `element`, `attribute`, `text`,
//!   `comment`, `processing-instruction`, `document` constructors.
//! * Atomic types `xs:string`, `untypedAtomic`, `anyURI`, `integer` (+ `int`, `long`,
//!   `short`, `byte`, `nonNegativeInteger`...), `decimal`, `double`, `float`, `boolean`,
//!   `dateTime`, `date`, `time`, `duration` (lexical only); constructor functions `xs:T(..)`.
//! * Functions: see [`funcs`] (`string`, `data`, `concat`, `string-join`, `substring*`,
//!   `contains`, `starts-with`, `ends-with`, `upper-case`, `lower-case`,
//!   `normalize-space`, `translate`, `tokenize`, `replace`, `matches` (via
//!   [`cameleer_core::regex`]), `count`, `sum`, `avg`, `min`, `max`, `distinct-values`,
//!   `exists`, `empty`, `position`, `last`, `reverse`, `subsequence`, `insert-before`,
//!   `remove`, `index-of`, `head`, `tail`, `deep-equal`, `doc` (local files),
//!   `current-dateTime`, `number`, `round`, `floor`, `ceiling`, `abs`, `boolean`, `not`,
//!   `name`, `local-name`, `root`, date component accessors, `error`, `trace`, `serialize`,
//!   `parse-xml`, `math:*`...).
//! * XQuery 3.1: maps (`map { k : v }`, `map:*`), arrays (`[a, b]`, `array { }`,
//!   `array:*`), lookups (`$m?key`, `?*`, unary `?k`), inline functions, named function
//!   references (`local:f#1`), dynamic calls, partial application (`f(?, 1)`), the arrow
//!   operator `=>`, and `for-each`, `filter`, `fold-left`, `fold-right`, `for-each-pair`,
//!   `sort`, `apply`, `function-arity`, `function-name`, `function-lookup`.
//! * Library modules (`module namespace p = "uri";`) imported with `import module namespace
//!   p = "uri" at "file.xqm";` (relative to the query file) or registered with
//!   [`register_module`].
//! * Arbitrary-precision `xs:decimal` arithmetic ([`decimal`]), date / time / duration
//!   arithmetic and accessors ([`datetime`]), timezone adjustment.
//! * XQuery Update Facility basics ([`update`]): `insert node(s) ... into | as first into |
//!   as last into | before | after`, `delete node(s)`, `replace node ... with`, `replace
//!   value of node`, `rename node ... as`, and `copy $c := ... modify ... return`. An
//!   updating query applies its pending updates to the context document and returns it.
//!
//! ```
//! use cameleer_xml::xquery::XQuery;
//! let q = XQuery::compile("for $b in /books/book order by $b/@year return string($b/title)", &[]).unwrap();
//! let xml = cameleer_core::value::Value::from("<books><book year='2001'><title>B</title></book><book year='1999'><title>A</title></book></books>");
//! let out = q.evaluate(Some(&xml), &|_, _| None, |docs, items| cameleer_xml::xquery::items_to_list(docs, items)).unwrap();
//! assert_eq!(out, cameleer_core::value::Value::from(vec!["A", "B"]));
//! ```

pub mod ast;
pub mod datetime;
pub mod decimal;
mod eval;
pub mod funcs;
mod hof;
mod parse;
pub mod types;
pub mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cameleer_core::value::Value;

use crate::dom::{Document, NodeType};
use crate::serialize::escape_text;
use crate::xpath::{Docs, XNode};

pub use ast::QN;
pub use types::{Atomic, Item, XqError, XqResult};

use eval::{Focus, Interp};
use types::{ItemType, SeqType};

/// The XPath functions namespace (`fn:`).
pub const FN_NS: &str = "http://www.w3.org/2005/xpath-functions";
/// XML Schema (`xs:`).
pub const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";
/// XML Schema instance (`xsi:`).
pub const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// Local functions (`local:`).
pub const LOCAL_NS: &str = "http://www.w3.org/2005/xquery-local-functions";
/// Error codes (`err:`).
pub const ERR_NS: &str = "http://www.w3.org/2005/xqt-errors";
/// Math functions (`math:`).
pub const MATH_NS: &str = "http://www.w3.org/2005/xpath-functions/math";
/// Map functions (`map:`).
pub const MAP_NS: &str = "http://www.w3.org/2005/xpath-functions/map";
/// Array functions (`array:`).
pub const ARRAY_NS: &str = "http://www.w3.org/2005/xpath-functions/array";

static MODULES: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Register the source of a library module for `import module namespace p = "uri";`
/// without an `at` location hint (Saxon's module URI resolver).
pub fn register_module(uri: &str, source: &str) {
    let mut m = MODULES.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|(u, _)| u != uri);
    m.push((uri.to_string(), source.to_string()));
}

pub(crate) fn registered_module(uri: &str) -> Option<String> {
    MODULES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(u, _)| u == uri).map(|(_, s)| s.clone())
}

/// Stack size of the evaluation thread (deeply recursive user functions).
const EVAL_STACK: usize = 256 * 1024 * 1024;

/// A compiled XQuery main module.
#[derive(Clone, Debug)]
pub struct XQuery {
    text: String,
    module: ast::Module,
    base_dir: Option<PathBuf>,
    strip_whitespace: bool,
}

/// A declared external variable.
#[derive(Clone, Debug)]
pub struct ExternalVar {
    pub name: QN,
    /// Whether the declared type expects nodes (string values are then parsed as XML).
    pub wants_nodes: bool,
    pub has_default: bool,
}

impl XQuery {
    /// Compile a query with extra `(prefix, uri)` namespace bindings.
    pub fn compile(text: &str, namespaces: &[(String, String)]) -> XqResult<XQuery> {
        let module = parse::parse_module(text, namespaces)?;
        if module.library_ns.is_some() {
            return Err(XqError::static_err("XQST0009", "A library module cannot be evaluated (a main module is required)"));
        }
        Ok(XQuery { text: text.to_string(), module, base_dir: None, strip_whitespace: false })
    }

    /// Compile a query file (`doc()` resolves relative paths against its directory).
    pub fn compile_file(path: &Path, namespaces: &[(String, String)]) -> XqResult<XQuery> {
        let text = std::fs::read_to_string(path).map_err(|e| XqError::static_err("XQST0059", format!("Cannot read query file {}: {}", path.display(), e)))?;
        let base = path.parent().map(|p| p.to_path_buf());
        let module = parse::parse_module_at(&text, namespaces, base.as_deref(), 0)?;
        if module.library_ns.is_some() {
            return Err(XqError::static_err("XQST0009", format!("{} is a library module, not a main module", path.display())));
        }
        Ok(XQuery { text, module, base_dir: base, strip_whitespace: false })
    }

    /// Set the directory relative `doc()` uris resolve against.
    pub fn with_base_dir(mut self, dir: Option<PathBuf>) -> XQuery {
        self.base_dir = dir;
        self
    }

    /// Remove whitespace-only text nodes from the context document before evaluating
    /// (Camel's `stripsAllWhiteSpace`).
    pub fn with_strip_whitespace(mut self, strip: bool) -> XQuery {
        self.strip_whitespace = strip;
        self
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The external variables declared in the prolog.
    pub fn external_variables(&self) -> Vec<ExternalVar> {
        self.module
            .vars
            .iter()
            .filter(|v| v.external)
            .map(|v| ExternalVar { name: v.name.clone(), wants_nodes: v.ty.as_ref().is_some_and(wants_nodes), has_default: v.value.is_some() })
            .collect()
    }

    /// Evaluate the query.
    ///
    /// * `context`: the context item source (parsed as an XML document; `Null` or a
    ///   non-XML value leaves the context item absent, which only fails when used).
    /// * `resolve(uri, local)`: values of external variables (`None` = unbound).
    /// * `convert`: turns the result sequence into the caller's representation (it runs on
    ///   the evaluation thread, where the documents live).
    pub fn evaluate<T: Send>(
        &self,
        context: Option<&Value>,
        resolve: &(dyn Fn(&str, &str) -> Option<Value> + Sync),
        convert: impl FnOnce(&Docs, &[Item]) -> T + Send,
    ) -> XqResult<T> {
        std::thread::scope(|s| {
            let h = std::thread::Builder::new()
                .name("xquery".into())
                .stack_size(EVAL_STACK)
                .spawn_scoped(s, move || self.evaluate_here(context, resolve, convert))
                .map_err(|e| XqError::new("FOER0000", format!("Cannot start the XQuery evaluation thread: {}", e)))?;
            h.join().unwrap_or_else(|_| Err(XqError::new("FOER0000", "XQuery evaluation panicked")))
        })
    }

    /// Evaluate on the calling thread (deep recursion is limited by its stack).
    pub fn evaluate_here<T>(&self, context: Option<&Value>, resolve: &dyn Fn(&str, &str) -> Option<Value>, convert: impl FnOnce(&Docs, &[Item]) -> T) -> XqResult<T> {
        let docs = Docs::default();
        let mut context_error = None;
        let focus = match context {
            None | Some(Value::Null) => Focus::none(),
            Some(v) => {
                let parsed = match v {
                    Value::Bytes(b) => Document::parse_bytes(b),
                    other => Document::parse(&other.to_string()),
                };
                match parsed {
                    Ok(mut d) => {
                        if self.strip_whitespace {
                            strip_whitespace(&mut d);
                        }
                        Focus::of(Item::Node(XNode::new(docs.add(d), 0)), 1, 1)
                    }
                    Err(e) => {
                        context_error = Some(format!("the input is not well-formed XML ({})", e));
                        Focus::none()
                    }
                }
            }
        };
        let now = cameleer_core::util_time::format_in_zone(cameleer_core::util_time::now_millis(), "yyyy-MM-dd'T'HH:mm:ss.SSSXXX", &cameleer_core::tz::TimeZone::local());
        let mut it = Interp {
            docs: &docs,
            module: &self.module,
            vars: Vec::new(),
            globals: HashMap::new(),
            depth: 0,
            doc_cache: HashMap::new(),
            base_dir: self.base_dir.clone(),
            context_error,
            now,
            pul: Vec::new(),
        };
        for v in &self.module.vars {
            let mut val = None;
            if v.external {
                if let Some(x) = resolve(&v.name.uri, &v.name.local) {
                    let mut items = value_to_items(&docs, &x, v.ty.as_ref())?;
                    // strings supplied for atomic types are cast like untyped values
                    if v.ty.as_ref().is_some_and(|t| matches!(t.item, ItemType::Atomic(_))) {
                        for i in items.iter_mut() {
                            if let Item::Atomic(Atomic::Str(s)) = i {
                                *i = Item::Atomic(Atomic::Untyped(std::mem::take(s)));
                            }
                        }
                    }
                    val = Some(items);
                }
            }
            let val = match val {
                Some(x) => x,
                None => match &v.value {
                    Some(e) => it.eval(e, &focus)?,
                    None => return Err(XqError::new("XPDY0002", format!("No value supplied for the external variable ${}", v.name.local))),
                },
            };
            let val = match &v.ty {
                Some(t) => it.convert_to(val, t, &format!("${}", v.name.local))?,
                None => val,
            };
            it.globals.insert(v.name.clone(), std::rc::Rc::new(val));
        }
        let body = self.module.body.as_ref().expect("module body");
        let mut out = it.eval(body, &focus)?;
        let pul = std::mem::take(&mut it.pul);
        let updated = !pul.is_empty();
        it.apply_updates(pul)?;
        if (updated || self.module.updating) && out.is_empty() {
            // an updating query returns the updated context document
            if let Some(Item::Node(n)) = &focus.item {
                out = vec![Item::Node(*n)];
            }
        }
        Ok(convert(&docs, &out))
    }
}

/// Drop whitespace-only text children of elements and document nodes.
fn strip_whitespace(d: &mut Document) {
    for i in 0..d.nodes.len() {
        if matches!(d.nodes[i].ty, NodeType::Element | NodeType::Document) && !d.nodes[i].children.is_empty() {
            let kids = std::mem::take(&mut d.nodes[i].children);
            let kept: Vec<usize> = kids.into_iter().filter(|&c| !(d.nodes[c].ty == NodeType::Text && d.nodes[c].value.trim().is_empty())).collect();
            d.nodes[i].children = kept;
        }
    }
}

fn wants_nodes(t: &SeqType) -> bool {
    matches!(t.item, ItemType::AnyNode | ItemType::Document(_) | ItemType::Element(_))
}

/// Convert a Camel value to items for an external variable of an optional declared type.
pub fn value_to_items(docs: &Docs, v: &Value, ty: Option<&SeqType>) -> XqResult<Vec<Item>> {
    let node_type = ty.filter(|t| wants_nodes(t));
    if let Some(t) = node_type {
        let parsed = match v {
            Value::Null => return Ok(Vec::new()),
            Value::Bytes(b) => Document::parse_bytes(b),
            Value::String(s) => Document::parse(s),
            Value::Array(items) => {
                let mut out = Vec::new();
                for x in items {
                    out.extend(value_to_items(docs, x, ty)?);
                }
                return Ok(out);
            }
            other => Document::parse(&other.to_string()),
        };
        let d = parsed.map_err(|e| XqError::type_err(format!("The value of an external variable of type {} is not well-formed XML: {}", t.describe(), e)))?;
        let root = d.document_element();
        let idx = docs.add(d);
        return Ok(vec![Item::Node(match (&t.item, root) {
            (ItemType::Element(_), Some(r)) => XNode::new(idx, r),
            _ => XNode::new(idx, 0),
        })]);
    }
    Ok(match v {
        Value::Null => Vec::new(),
        Value::Bool(b) => vec![Item::bool(*b)],
        Value::Int(i) => vec![Item::int(*i)],
        Value::Float(f) => vec![Item::dbl(*f)],
        Value::String(s) => vec![Item::str(s.clone())],
        Value::Bytes(b) => vec![Item::str(String::from_utf8_lossy(b).into_owned())],
        Value::Array(items) => {
            let mut out = Vec::new();
            for x in items {
                out.extend(value_to_items(docs, x, None)?);
            }
            out
        }
        other => vec![Item::str(other.to_string())],
    })
}

fn node_xml(docs: &Docs, n: XNode) -> String {
    match docs.node_type(n) {
        Some(NodeType::Element) => docs.get(n.doc).to_xml(n.node_id()),
        Some(NodeType::Document) => {
            let d = docs.get(n.doc);
            d.children(n.node_id()).iter().map(|&c| d.to_xml(c)).collect()
        }
        Some(NodeType::Text) => escape_text(&docs.string_value(n)),
        Some(NodeType::Comment) => format!("<!--{}-->", docs.string_value(n)),
        Some(NodeType::ProcessingInstruction) => {
            let v = docs.string_value(n);
            let t = docs.node_name(n).1;
            if v.is_empty() {
                format!("<?{}?>", t)
            } else {
                format!("<?{} {}?>", t, v)
            }
        }
        _ => escape_text(&docs.string_value(n)),
    }
}

/// Serialize a result sequence as XML (nodes as markup, adjacent atomic values separated
/// by a space, attributes as their value).
pub fn serialize_items(docs: &Docs, items: &[Item]) -> String {
    let mut out = String::new();
    let mut prev_atomic = false;
    for it in items {
        match it {
            Item::Node(n) => {
                out.push_str(&node_xml(docs, *n));
                prev_atomic = false;
            }
            Item::Atomic(a) => {
                if prev_atomic {
                    out.push(' ');
                }
                out.push_str(&escape_text(&a.to_string()));
                prev_atomic = true;
            }
            other => {
                if prev_atomic {
                    out.push(' ');
                }
                out.push_str(&escape_text(&adaptive(docs, other)));
                prev_atomic = true;
            }
        }
    }
    out
}

/// Adaptive serialization of maps, arrays and functions (`map{"a":1}`, `[1,(2,3)]`).
fn adaptive(docs: &Docs, item: &Item) -> String {
    let seq = |s: &[Item]| -> String {
        let parts: Vec<String> = s.iter().map(|i| adaptive(docs, i)).collect();
        if parts.len() == 1 {
            parts.into_iter().next().unwrap()
        } else {
            format!("({})", parts.join(","))
        }
    };
    match item {
        Item::Node(n) => node_xml(docs, *n),
        Item::Atomic(a) => match a {
            Atomic::Str(s) | Atomic::Untyped(s) | Atomic::AnyUri(s) => {
                let mut o = String::new();
                cameleer_core::value::json_escape(s, &mut o);
                o
            }
            other => other.to_string(),
        },
        Item::Map(m) => format!("map{{{}}}", m.entries.iter().map(|(k, v)| format!("{}:{}", adaptive(docs, &Item::Atomic(k.clone())), seq(v))).collect::<Vec<_>>().join(",")),
        Item::Array(a) => format!("[{}]", a.iter().map(|m| seq(m)).collect::<Vec<_>>().join(",")),
        Item::Func(f) => match f.as_ref() {
            types::FuncItem::Named { name, arity } => format!("{}#{}", name.local, arity),
            other => format!("(anonymous-function)#{}", other.arity()),
        },
    }
}

/// The string values of the items, concatenated (Camel `resultsFormat=String`).
pub fn items_to_string(docs: &Docs, items: &[Item]) -> String {
    items
        .iter()
        .map(|i| match i {
            Item::Node(n) => docs.string_value(*n),
            Item::Atomic(a) => a.to_string(),
            other => adaptive(docs, other),
        })
        .collect()
}

/// One item as a Camel value: elements/documents as XML text, other nodes as their string
/// value, atomic values as `Int`/`Float`/`Bool`/`String`.
pub fn item_to_value(docs: &Docs, item: &Item) -> Value {
    match item {
        Item::Node(n) => match docs.node_type(*n) {
            Some(NodeType::Element) | Some(NodeType::Document) | Some(NodeType::Comment) | Some(NodeType::ProcessingInstruction) => Value::String(node_xml(docs, *n)),
            _ => Value::String(docs.string_value(*n)),
        },
        Item::Atomic(a) => match a {
            Atomic::Int(i) => Value::Int(*i),
            Atomic::Dec(d) => Value::Float(d.to_f64()),
            Atomic::Dbl(f) | Atomic::Flt(f) => Value::Float(*f),
            Atomic::Bool(b) => Value::Bool(*b),
            other => Value::String(other.to_string()),
        },
        Item::Map(m) => Value::object(m.entries.iter().map(|(k, v)| (k.to_string(), seq_to_value(docs, v)))),
        Item::Array(a) => Value::Array(a.iter().map(|m| seq_to_value(docs, m)).collect()),
        Item::Func(_) => Value::String(adaptive(docs, item)),
    }
}

fn seq_to_value(docs: &Docs, s: &[Item]) -> Value {
    match s {
        [] => Value::Null,
        [one] => item_to_value(docs, one),
        many => Value::Array(many.iter().map(|i| item_to_value(docs, i)).collect()),
    }
}

/// The items as a Camel list (Camel `resultsFormat=List`).
pub fn items_to_list(docs: &Docs, items: &[Item]) -> Value {
    Value::Array(items.iter().map(|i| item_to_value(docs, i)).collect())
}

/// Effective boolean value of a result (predicates); errors count as false.
pub fn items_to_bool(items: &[Item]) -> bool {
    match items {
        [] => false,
        [Item::Node(_), ..] => true,
        [Item::Atomic(a)] => match a {
            Atomic::Bool(b) => *b,
            Atomic::Str(s) | Atomic::Untyped(s) | Atomic::AnyUri(s) => !s.is_empty(),
            x if x.is_numeric() => {
                let f = x.as_f64();
                f != 0.0 && !f.is_nan()
            }
            _ => true,
        },
        _ => true,
    }
}

#[cfg(test)]
mod tests;
