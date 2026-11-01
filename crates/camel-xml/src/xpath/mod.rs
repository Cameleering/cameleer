//! A complete, namespace-aware XPath 1.0 implementation.
//!
//! * All 13 axes, all node tests, predicates (positional with reverse-axis proximity),
//!   unions, filter expressions, variables, the full operator set with the XPath 1.0
//!   comparison / conversion rules, and the 27 core functions. `ends-with`, `upper-case`
//!   and `lower-case` are accepted as extensions (compatibility with the core subset).
//! * Namespace prefixes in name tests, variables and function names are resolved at
//!   compile time ([`XPath::compile_with`]); unprefixed names match nodes without namespace,
//!   as the specification requires.
//! * Extension functions and variables come from an [`Env`] (used by XSLT and by the
//!   Camel `xpath` language for `in:body()`, `$header` etc.).
//! * [`XPath::compile_v2`] adds the commonly used XPath 2.0 syntax (`if then else`,
//!   `for ... return`, `some` / `every`, sequences `(a, b)`, `()`, `to`, value comparisons,
//!   `intersect` / `except`, `idiv`, `||`) and the XPath 2.0 function library of [`f2`]
//!   (regular expressions, `string-join`, `distinct-values`, dates and `format-date`,
//!   `xs:*` constructors...). Sequences of atomic values are represented as node-sets of
//!   text nodes (one per item, in order) in a scratch document, so XPath 1.0 consumers keep
//!   working; the functions are also available to 1.0 expressions.
//!
//! ```
//! use camel_xml::xpath::XPath;
//! let doc = camel_xml::parser::parse("<a><b>1</b><b>2</b></a>").unwrap();
//! assert_eq!(XPath::compile("sum(//b)").unwrap().eval_string(&doc).unwrap(), "3");
//! ```

pub mod ast;
mod eval;
pub mod f2;
mod parse;

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::dom::{Document, NodeId, NodeType};

pub use eval::{format_number, string_to_number};
pub use parse::PrefixResolver;

/// An XPath error: syntax (`XPathExpressionException` at compile time) or evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct XPathError {
    pub message: String,
    pub syntax: bool,
}

impl XPathError {
    pub(crate) fn syntax(expr: &str, m: String) -> XPathError {
        XPathError { message: format!("Invalid xpath: {}. Reason: {}", expr, m), syntax: true }
    }

    /// An evaluation error.
    pub fn eval(m: impl Into<String>) -> XPathError {
        XPathError { message: m.into(), syntax: false }
    }
}

impl fmt::Display for XPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for XPathError {}

impl From<XPathError> for camel_core::error::CamelError {
    fn from(e: XPathError) -> Self {
        use camel_core::error::{kinds, CamelError};
        if e.syntax {
            CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, e.message)
        } else {
            crate::register_kinds();
            CamelError::new(crate::XPATH_EXPRESSION_EXCEPTION, e.message)
        }
    }
}

/// A node reference: document index in a [`Docs`] set, node id, and namespace-node index
/// (`ns == 0` for a real node, `k` for the k-th in-scope namespace of element `id`).
/// The derived ordering is document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct XNode {
    pub doc: u32,
    pub id: u32,
    pub ns: u32,
}

impl XNode {
    pub fn new(doc: u32, id: NodeId) -> XNode {
        XNode { doc, id: id as u32, ns: 0 }
    }

    pub fn node_id(&self) -> NodeId {
        self.id as NodeId
    }

    pub fn is_namespace(&self) -> bool {
        self.ns != 0
    }
}

/// An XPath value.
#[derive(Clone, Debug, PartialEq)]
pub enum XValue {
    /// A node-set in document order without duplicates.
    Nodes(Vec<XNode>),
    Str(String),
    Num(f64),
    Bool(bool),
}

impl XValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            XValue::Nodes(_) => "node-set",
            XValue::Str(_) => "string",
            XValue::Num(_) => "number",
            XValue::Bool(_) => "boolean",
        }
    }

    /// XPath `boolean()`.
    pub fn to_bool(&self) -> bool {
        match self {
            XValue::Nodes(n) => !n.is_empty(),
            XValue::Str(s) => !s.is_empty(),
            XValue::Num(n) => *n != 0.0 && !n.is_nan(),
            XValue::Bool(b) => *b,
        }
    }

    /// XPath `string()`.
    pub fn to_string_value(&self, docs: &Docs) -> String {
        match self {
            XValue::Nodes(n) => n.first().map(|n| docs.string_value(*n)).unwrap_or_default(),
            XValue::Str(s) => s.clone(),
            XValue::Num(n) => format_number(*n),
            XValue::Bool(b) => b.to_string(),
        }
    }

    /// XPath `number()`.
    pub fn to_number(&self, docs: &Docs) -> f64 {
        match self {
            XValue::Num(n) => *n,
            XValue::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            XValue::Str(s) => string_to_number(s),
            XValue::Nodes(_) => string_to_number(&self.to_string_value(docs)),
        }
    }
}

/// The set of documents an evaluation works on (the source document, result tree
/// fragments, documents loaded by `document()`).
#[derive(Default)]
pub struct Docs {
    list: RefCell<Vec<Rc<Document>>>,
}

impl Docs {
    /// A set containing one document (index 0).
    pub fn new(doc: Document) -> Docs {
        Docs { list: RefCell::new(vec![Rc::new(doc)]) }
    }

    /// Add a document; returns its index.
    pub fn add(&self, doc: Document) -> u32 {
        self.add_rc(Rc::new(doc))
    }

    /// A sequence of atomic values: one text node per item, in order, in a new document.
    pub fn atomic_sequence(&self, items: &[String]) -> XValue {
        let mut d = Document::new();
        let ids: Vec<NodeId> = items.iter().map(|s| d.append_text_node(0, s)).collect();
        let di = self.add(d);
        XValue::Nodes(ids.into_iter().map(|i| XNode::new(di, i)).collect())
    }

    /// Replace document `d` (XQuery updates); node ids of the old document stay valid when
    /// the new one only appends nodes.
    pub fn replace(&self, d: u32, doc: Document) {
        self.list.borrow_mut()[d as usize] = Rc::new(doc);
    }

    pub fn add_rc(&self, doc: Rc<Document>) -> u32 {
        let mut l = self.list.borrow_mut();
        l.push(doc);
        (l.len() - 1) as u32
    }

    /// Document by index.
    pub fn get(&self, d: u32) -> Rc<Document> {
        self.list.borrow()[d as usize].clone()
    }

    pub fn len(&self) -> usize {
        self.list.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.borrow().is_empty()
    }

    /// The root (document node) of document `d`'s first tree.
    pub fn root(&self, d: u32) -> XNode {
        XNode::new(d, 0)
    }

    /// String value of a node.
    pub fn string_value(&self, n: XNode) -> String {
        let doc = self.get(n.doc);
        if n.ns != 0 {
            return doc.in_scope_namespaces(n.node_id()).get(n.ns as usize - 1).map(|(_, u)| u.clone()).unwrap_or_default();
        }
        doc.string_value(n.node_id())
    }

    /// `(prefix, local, uri)` of a node (namespace node: prefix as local name).
    pub fn node_name(&self, n: XNode) -> (String, String, String) {
        let doc = self.get(n.doc);
        if n.ns != 0 {
            let p = doc.in_scope_namespaces(n.node_id()).get(n.ns as usize - 1).map(|(p, _)| p.clone()).unwrap_or_default();
            return (String::new(), p, String::new());
        }
        let node = doc.node(n.node_id());
        match node.ty {
            NodeType::Element | NodeType::Attribute => (node.name.prefix.clone(), node.name.local.clone(), node.name.uri.clone()),
            NodeType::ProcessingInstruction => (String::new(), node.name.local.clone(), String::new()),
            _ => (String::new(), String::new(), String::new()),
        }
    }

    /// Node type (`None` for namespace nodes).
    pub fn node_type(&self, n: XNode) -> Option<NodeType> {
        if n.ns != 0 {
            None
        } else {
            Some(self.get(n.doc).node_type(n.node_id()))
        }
    }

    /// Serialize a node: elements and documents as XML, other nodes as their string value.
    pub fn node_to_string(&self, n: XNode) -> String {
        match self.node_type(n) {
            Some(NodeType::Element) => self.get(n.doc).to_xml(n.node_id()),
            Some(NodeType::Document) => {
                let d = self.get(n.doc);
                d.children(n.node_id()).iter().map(|&c| d.to_xml(c)).collect()
            }
            _ => self.string_value(n),
        }
    }
}

/// The dynamic context of an evaluation.
pub struct Ctx<'a> {
    pub docs: &'a Docs,
    pub node: XNode,
    pub pos: usize,
    pub size: usize,
    pub env: &'a dyn Env,
}

/// Provides variables and extension functions.
pub trait Env {
    /// Value of variable `{uri}local`.
    fn variable(&self, uri: &str, local: &str) -> Result<XValue, XPathError> {
        let _ = uri;
        Err(XPathError::eval(format!("Variable ${} is not defined", local)))
    }

    /// Call an extension (or overriding) function. Return `None` when the function is
    /// unknown to the environment (core functions are then tried).
    fn function(&self, uri: &str, local: &str, args: &[XValue], ctx: &Ctx) -> Option<Result<XValue, XPathError>> {
        let _ = (uri, local, args, ctx);
        None
    }
}

/// An environment without variables or extension functions.
pub struct NoEnv;
impl Env for NoEnv {}

/// A compiled XPath expression.
#[derive(Clone, Debug)]
pub struct XPath {
    text: String,
    expr: ast::Expr,
}

impl XPath {
    /// Compile an expression without namespace prefixes (other than `xml`).
    pub fn compile(text: &str) -> Result<XPath, XPathError> {
        XPath::compile_with(text, &|p: &str| if p == "xml" { Some(crate::dom::XML_NS.to_string()) } else { None })
    }

    /// Compile with a prefix resolver.
    pub fn compile_with(text: &str, ns: PrefixResolver<'_>) -> Result<XPath, XPathError> {
        let toks = parse::tokenize(text)?;
        let mut p = parse::Parser { toks, i: 0, text, ns, v2: false };
        let expr = p.parse_all()?;
        Ok(XPath { text: text.to_string(), expr })
    }

    /// Compile an XPath 2.0 expression (see the module documentation for the subset).
    pub fn compile_v2(text: &str, ns: PrefixResolver<'_>) -> Result<XPath, XPathError> {
        let toks = parse::tokenize_v(text, true)?;
        let mut p = parse::Parser { toks, i: 0, text, ns, v2: true };
        let expr = p.parse_all()?;
        Ok(XPath { text: text.to_string(), expr })
    }

    /// Compile with a list of `(prefix, uri)` bindings.
    pub fn compile_ns(text: &str, bindings: &[(String, String)]) -> Result<XPath, XPathError> {
        XPath::compile_with(text, &|p: &str| {
            bindings.iter().find(|(bp, _)| bp == p).map(|(_, u)| u.clone()).or_else(|| if p == "xml" { Some(crate::dom::XML_NS.to_string()) } else { None })
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn expr(&self) -> &ast::Expr {
        &self.expr
    }

    /// Evaluate with an explicit context.
    pub fn eval_ctx(&self, ctx: &Ctx) -> Result<XValue, XPathError> {
        eval::eval(&self.expr, ctx)
    }

    /// Evaluate against the root of a document set's first document.
    pub fn eval_docs(&self, docs: &Docs, env: &dyn Env) -> Result<XValue, XPathError> {
        let ctx = Ctx { docs, node: docs.root(0), pos: 1, size: 1, env };
        self.eval_ctx(&ctx)
    }

    /// Evaluate against a document (context node = root) and convert to a string.
    pub fn eval_string(&self, doc: &Document) -> Result<String, XPathError> {
        let docs = Docs::new(doc.clone());
        let v = self.eval_docs(&docs, &NoEnv)?;
        Ok(v.to_string_value(&docs))
    }
}

/// Evaluate an expression AST (used by XSLT).
pub fn eval_expr(e: &ast::Expr, ctx: &Ctx) -> Result<XValue, XPathError> {
    eval::eval(e, ctx)
}

/// Combine values into one sequence value (XPath 2.0 `(a, b, ...)` semantics on the
/// 1.0 data model: node-sets stay node-sets, mixed / atomic items become an atomic sequence).
pub fn sequence(items: Vec<XValue>, docs: &Docs) -> XValue {
    eval::sequence_of(items, docs)
}

/// The items of a value: one single-node value per node, or the atomic value itself.
pub fn items(v: XValue) -> Vec<XValue> {
    eval::items_of(v)
}

/// Sort node references in document order and remove duplicates.
pub fn sort_nodes(v: &mut Vec<XNode>) {
    v.sort();
    v.dedup();
}

/// Nodes of an axis from a context node, in axis order.
pub fn axis_nodes(docs: &Docs, n: XNode, axis: ast::Axis) -> Vec<XNode> {
    eval::axis_nodes(docs, n, axis)
}

/// Whether a node matches a node test on an axis.
pub fn node_test_matches(docs: &Docs, n: XNode, test: &ast::NodeTest, axis: ast::Axis) -> bool {
    eval::test_matches(docs, n, test, axis)
}
