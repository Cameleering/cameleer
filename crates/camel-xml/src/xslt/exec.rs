//! XSLT execution (1.0, plus the XSLT 2.0 core).

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use camel_core::error::CamelError;
use camel_core::value::Value;

use super::number::{format_number, format_number_list_lang, DecimalFormat, NumberLang};
use super::pattern::Pattern;
use super::{resolve_href, Avt, AvtPart, Function, GroupKind, Instr, Key, NumberSpec, SortSpec, Stylesheet, VarValue, WithParam};
use crate::dom::{Document, NodeId, NodeType, QName};
use crate::serialize::{Method, RAW_TEXT_TARGET};
use crate::xpath::{axis_nodes, Ctx, Docs, Env, XNode, XPath, XPathError, XValue};

/// The result of a transformation.
#[derive(Debug, Clone)]
pub struct TransformResult {
    /// The serialized result (per `xsl:output`).
    pub output: String,
    /// The result tree.
    pub tree: Document,
    /// Messages written with `xsl:message`.
    pub messages: Vec<String>,
    /// The output method used.
    pub method: Method,
    /// `xsl:output media-type`, or the default for the method.
    pub media_type: String,
    /// Secondary results written with `xsl:result-document` (`href`, serialized content).
    pub secondary: Vec<(String, String)>,
}

type XR<T> = Result<T, String>;

const MAX_DEPTH: usize = 5000;

#[derive(Clone, Copy)]
struct Frame {
    node: XNode,
    pos: usize,
    size: usize,
}

enum GState {
    Evaluating,
    Done(XValue),
}

struct Out {
    doc: Document,
    stack: Vec<NodeId>,
    /// Sequence capture (bodies of `xsl:function` and of variables with `as`).
    items: Option<Vec<Item>>,
    /// Root children of `doc` already recorded in `items`.
    flushed: usize,
}

impl Out {
    fn new() -> Out {
        Out { doc: Document::new(), stack: vec![0], items: None, flushed: 0 }
    }

    /// Record root children constructed since the last flush as items.
    fn flush(&mut self) {
        let kids = self.doc.children(0).to_vec();
        if let Some(items) = &mut self.items {
            for &k in &kids[self.flushed..] {
                items.push(Item::Node(k));
            }
        }
        self.flushed = kids.len();
    }
}

enum Item {
    Val(XValue),
    Node(NodeId),
}

/// The current group of `xsl:for-each-group`.
struct Group {
    items: Vec<XNode>,
    key: XValue,
}

struct Exec<'s> {
    ss: &'s Stylesheet,
    docs: Docs,
    outs: RefCell<Vec<Out>>,
    globals: RefCell<HashMap<Key, GState>>,
    ext_params: HashMap<Key, XValue>,
    key_index: RefCell<HashMap<(String, u32), HashMap<String, Vec<XNode>>>>,
    loaded: RefCell<HashMap<PathBuf, u32>>,
    stylesheet_doc: Cell<Option<u32>>,
    messages: RefCell<Vec<String>>,
    depth: Cell<usize>,
    rules: RefCell<Vec<(usize, Option<Key>)>>,
    groups: RefCell<Vec<Rc<Group>>>,
    regex_groups: RefCell<Vec<Rc<Vec<String>>>>,
    tunnel: RefCell<Vec<Rc<Vec<(Key, XValue)>>>>,
    secondary: RefCell<Vec<(String, String)>>,
}

struct EvalEnv<'a, 's> {
    ex: &'a Exec<'s>,
    locals: &'a [(Key, XValue)],
    current: XNode,
}

fn xerr(e: XPathError) -> String {
    e.message
}

fn local_part(q: &str) -> &str {
    q.rsplit(':').next().unwrap_or(q)
}

impl Env for EvalEnv<'_, '_> {
    fn variable(&self, uri: &str, local: &str) -> Result<XValue, XPathError> {
        if let Some((_, v)) = self.locals.iter().rev().find(|(k, _)| k.0 == uri && k.1 == local) {
            return Ok(v.clone());
        }
        match self.ex.global(&(uri.to_string(), local.to_string())) {
            Some(r) => r.map_err(XPathError::eval),
            None => Err(XPathError::eval(format!("Variable ${} has not been declared", local))),
        }
    }

    fn function(&self, uri: &str, local: &str, args: &[XValue], ctx: &Ctx) -> Option<Result<XValue, XPathError>> {
        let docs = ctx.docs;
        let s = |i: usize| args.get(i).map(|a| a.to_string_value(docs)).unwrap_or_default();
        let r: Result<XValue, String> = match (uri, local) {
            ("", "current") => Ok(XValue::Nodes(vec![self.current])),
            ("", "current-group") if args.is_empty() => Ok(match self.ex.groups.borrow().last() {
                Some(g) => XValue::Nodes(g.items.clone()),
                None => XValue::Nodes(Vec::new()),
            }),
            ("", "current-grouping-key") if args.is_empty() => Ok(match self.ex.groups.borrow().last() {
                Some(g) => g.key.clone(),
                None => XValue::Nodes(Vec::new()),
            }),
            ("", "regex-group") if args.len() == 1 => {
                let i = args[0].to_number(docs);
                Ok(XValue::Str(match self.ex.regex_groups.borrow().last() {
                    Some(g) if i >= 0.0 => g.get(i as usize).cloned().unwrap_or_default(),
                    _ => String::new(),
                }))
            }
            ("", "unparsed-text") if !args.is_empty() => {
                let p = resolve_href(&s(0), self.ex.ss.base_dir.as_deref());
                std::fs::read_to_string(&p).map(XValue::Str).map_err(|e| format!("Cannot read {}: {}", p.display(), e))
            }
            ("", "unparsed-text-available") if !args.is_empty() => Ok(XValue::Bool(resolve_href(&s(0), self.ex.ss.base_dir.as_deref()).is_file())),
            (u, l) if !u.is_empty() && self.ex.ss.functions.contains_key(&((u.to_string(), l.to_string()), args.len())) => {
                let f = self.ex.ss.functions[&((u.to_string(), l.to_string()), args.len())].clone();
                self.ex.call_function(&f, l, args, ctx.node)
            }
            ("", "key") => self.ex.key_fn(&s(0), args.get(1).cloned().unwrap_or(XValue::Str(String::new())), ctx.node.doc),
            ("", "document") => self.ex.document_fn(args),
            ("", "generate-id") => {
                let n = match args.first() {
                    None => Some(ctx.node),
                    Some(XValue::Nodes(ns)) => ns.first().copied(),
                    Some(_) => return Some(Err(XPathError::eval("generate-id() requires a node-set"))),
                };
                Ok(XValue::Str(n.map(|n| if n.ns > 0 { format!("d{}n{}ns{}", n.doc, n.id, n.ns) } else { format!("d{}n{}", n.doc, n.id) }).unwrap_or_default()))
            }
            ("", "format-number") => {
                let x = args.first().map(|a| a.to_number(docs)).unwrap_or(f64::NAN);
                let df_name = if args.len() > 2 { local_part(&s(2)).to_string() } else { String::new() };
                let df = self.ex.ss.decimal_formats.iter().find(|(k, _)| k.1 == df_name).map(|(_, v)| v.clone()).unwrap_or_else(DecimalFormat::default);
                format_number(x, &s(1), &df).map(XValue::Str)
            }
            ("", "system-property") => Ok(match local_part(&s(0)) {
                "version" => XValue::Num(if self.ex.ss.version >= 2.0 { 2.0 } else { 1.0 }),
                "product-name" => XValue::Str("rust-camel".into()),
                "supports-backwards-compatibility" => XValue::Str("yes".into()),
                "vendor" => XValue::Str("rust-camel".into()),
                "vendor-url" => XValue::Str("https://camel.apache.org".into()),
                _ => XValue::Str(String::new()),
            }),
            ("", "element-available") => {
                let n = s(0);
                let l = local_part(&n);
                Ok(XValue::Bool(n.contains(':') && (SUPPORTED_INSTRUCTIONS.contains(&l) || (self.ex.ss.version >= 2.0 && super::XSLT2_IMPLEMENTED.contains(&l)))))
            }
            ("", "function-available") => {
                let name = s(0);
                let l = local_part(&name);
                let user = match name.split_once(':') {
                    Some((p, _)) => self.ex.ss.functions.keys().any(|((u, fl), _)| fl == l && !u.is_empty() && !p.is_empty()),
                    None => false,
                };
                Ok(XValue::Bool(user || FUNCTIONS.contains(&l) || (self.ex.ss.version >= 2.0 && crate::xpath::f2::FUNCTION_NAMES.contains(&l))))
            }
            ("", "unparsed-entity-uri") => {
                let d = docs.get(ctx.node.doc);
                Ok(XValue::Str(d.unparsed_entities.get(&s(0)).cloned().unwrap_or_default()))
            }
            ("http://exslt.org/common" | "urn:schemas-microsoft-com:xslt" | "http://xml.apache.org/xalan" | "http://xml.apache.org/xslt", "node-set" | "nodeset") => {
                match args.first() {
                    Some(XValue::Nodes(n)) => Ok(XValue::Nodes(n.clone())),
                    Some(other) => {
                        let mut d = Document::new();
                        d.append_text(0, &other.to_string_value(docs));
                        Ok(XValue::Nodes(vec![XNode::new(docs.add(d), 0)]))
                    }
                    None => Err("node-set() requires one argument".into()),
                }
            }
            ("http://exslt.org/common", "object-type") => Ok(XValue::Str(
                match args.first() {
                    Some(XValue::Nodes(_)) => "node-set",
                    Some(XValue::Str(_)) => "string",
                    Some(XValue::Num(_)) => "number",
                    Some(XValue::Bool(_)) => "boolean",
                    None => "",
                }
                .into(),
            )),
            _ => return None,
        };
        Some(r.map_err(XPathError::eval))
    }
}

const SUPPORTED_INSTRUCTIONS: &[&str] = &[
    "apply-imports",
    "apply-templates",
    "attribute",
    "call-template",
    "choose",
    "comment",
    "copy",
    "copy-of",
    "element",
    "fallback",
    "for-each",
    "if",
    "message",
    "number",
    "processing-instruction",
    "text",
    "value-of",
    "variable",
];

const FUNCTIONS: &[&str] = &[
    "last", "position", "count", "id", "local-name", "namespace-uri", "name", "string", "concat", "starts-with", "contains", "substring-before",
    "substring-after", "substring", "string-length", "normalize-space", "translate", "boolean", "not", "true", "false", "lang", "number", "sum", "floor",
    "ceiling", "round", "document", "key", "format-number", "current", "unparsed-entity-uri", "generate-id", "system-property", "element-available",
    "function-available", "node-set",
];

fn value_to_xvalue(v: &Value) -> XValue {
    match v {
        Value::Bool(b) => XValue::Bool(*b),
        Value::Int(i) => XValue::Num(*i as f64),
        Value::Float(f) => XValue::Num(*f),
        other => XValue::Str(other.to_string()),
    }
}

/// Run a transformation on a thread with a large stack (deeply recursive stylesheets).
pub(crate) fn run(ss: &Stylesheet, source: Document, params: &[(String, Value)]) -> Result<TransformResult, CamelError> {
    let r = std::thread::scope(|s| {
        let h = std::thread::Builder::new()
            .name("camel-xslt".into())
            .stack_size(128 << 20)
            .spawn_scoped(s, || run_inner(ss, source, params))
            .map_err(|e| format!("Cannot start transformer thread: {}", e))?;
        h.join().unwrap_or_else(|_| Err("Transformation panicked".to_string()))
    });
    r.map_err(|m| {
        crate::register_kinds();
        CamelError::new(crate::TRANSFORMER_EXCEPTION, m)
    })
}

fn strip_space(ss: &Stylesheet, doc: &mut Document) {
    if !ss.space.iter().any(|r| r.strip) {
        return;
    }
    let decide = |doc: &Document, el: NodeId| -> bool {
        let name = doc.name(el);
        let mut best: Option<(usize, f64, bool)> = None;
        for r in &ss.space {
            let spec = match &r.test {
                None => -0.5,
                Some((u, None)) if *u == name.uri => -0.25,
                Some((u, Some(l))) if *u == name.uri && *l == name.local => 0.0,
                _ => continue,
            };
            let cand = (r.precedence, spec, !r.strip);
            if best.map(|b| (cand.0, cand.1) >= (b.0, b.1)).unwrap_or(true) {
                best = Some(cand);
            }
        }
        matches!(best, Some((_, _, false)))
    };
    let n = doc.nodes.len();
    for el in 0..n {
        if doc.nodes[el].ty != NodeType::Element || !decide(doc, el) {
            continue;
        }
        // xml:space="preserve" in scope wins
        let mut cur = Some(el);
        let mut preserve = false;
        while let Some(c) = cur {
            if let Some(v) = doc.attribute_ns(c, crate::dom::XML_NS, "space") {
                preserve = v == "preserve";
                break;
            }
            cur = doc.parent(c);
        }
        if preserve {
            continue;
        }
        let kids: Vec<NodeId> = doc.nodes[el].children.clone();
        let keep: Vec<NodeId> = kids
            .into_iter()
            .filter(|&c| !(doc.nodes[c].ty == NodeType::Text && doc.nodes[c].value.chars().all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r'))))
            .collect();
        doc.nodes[el].children = keep;
    }
}

fn run_inner(ss: &Stylesheet, mut source: Document, params: &[(String, Value)]) -> XR<TransformResult> {
    strip_space(ss, &mut source);
    let mut ext = HashMap::new();
    for (k, v) in params {
        ext.insert((String::new(), k.clone()), value_to_xvalue(v));
    }
    let ex = Exec {
        ss,
        docs: Docs::new(source),
        outs: RefCell::new(vec![Out::new()]),
        globals: RefCell::new(HashMap::new()),
        ext_params: ext,
        key_index: RefCell::new(HashMap::new()),
        loaded: RefCell::new(HashMap::new()),
        stylesheet_doc: Cell::new(None),
        messages: RefCell::new(Vec::new()),
        depth: Cell::new(0),
        rules: RefCell::new(Vec::new()),
        groups: RefCell::new(Vec::new()),
        regex_groups: RefCell::new(Vec::new()),
        tunnel: RefCell::new(vec![Rc::new(Vec::new())]),
        secondary: RefCell::new(Vec::new()),
    };
    let root = Frame { node: XNode::new(0, 0), pos: 1, size: 1 };
    ex.process_node(root, &None, &[])?;
    let out = ex.outs.borrow_mut().pop().unwrap();
    let method = ss.output.effective_method(&out.doc, 0);
    let output = crate::serialize::serialize(&out.doc, 0, &ss.output);
    let media_type = ss.output.media_type.clone().unwrap_or_else(|| {
        match method {
            Method::Xml => "text/xml",
            Method::Html => "text/html",
            Method::Text => "text/plain",
        }
        .to_string()
    });
    Ok(TransformResult { output, tree: out.doc, messages: ex.messages.into_inner(), method, media_type, secondary: ex.secondary.into_inner() })
}

impl<'s> Exec<'s> {
    fn eval(&self, xp: &XPath, f: &Frame, locals: &[(Key, XValue)]) -> XR<XValue> {
        let env = EvalEnv { ex: self, locals, current: f.node };
        let ctx = Ctx { docs: &self.docs, node: f.node, pos: f.pos, size: f.size, env: &env };
        xp.eval_ctx(&ctx).map_err(xerr)
    }

    fn eval_string(&self, xp: &XPath, f: &Frame, locals: &[(Key, XValue)]) -> XR<String> {
        Ok(self.eval(xp, f, locals)?.to_string_value(&self.docs))
    }

    fn eval_nodes(&self, xp: &XPath, f: &Frame, locals: &[(Key, XValue)]) -> XR<Vec<XNode>> {
        match self.eval(xp, f, locals)? {
            XValue::Nodes(n) => Ok(n),
            other if self.ss.version >= 2.0 => match self.docs.atomic_sequence(&[other.to_string_value(&self.docs)]) {
                XValue::Nodes(n) => Ok(n),
                _ => Ok(Vec::new()),
            },
            other => Err(format!("Expression '{}' must evaluate to a node-set but returned a {}", xp.as_str(), other.type_name())),
        }
    }

    fn avt(&self, avt: &Avt, f: &Frame, locals: &[(Key, XValue)]) -> XR<String> {
        let mut s = String::new();
        for p in &avt.0 {
            match p {
                AvtPart::Lit(l) => s.push_str(l),
                AvtPart::Expr(x) => s.push_str(&self.eval_string(x, f, locals)?),
            }
        }
        Ok(s)
    }

    fn pattern_matches(&self, p: &Pattern, n: XNode) -> XR<bool> {
        let env = EvalEnv { ex: self, locals: &[], current: n };
        p.matches(&self.docs, n, &env).map_err(xerr)
    }

    fn global(&self, key: &Key) -> Option<XR<XValue>> {
        let g = self.ss.globals.iter().find(|g| g.name == *key)?;
        if g.is_param {
            if let Some(v) = self.ext_params.get(key) {
                return Some(Ok(v.clone()));
            }
        }
        match self.globals.borrow().get(key) {
            Some(GState::Done(v)) => return Some(Ok(v.clone())),
            Some(GState::Evaluating) => return Some(Err(format!("Circular definition of global variable ${}", key.1))),
            None => {}
        }
        self.globals.borrow_mut().insert(key.clone(), GState::Evaluating);
        let root = Frame { node: XNode::new(0, 0), pos: 1, size: 1 };
        let r = self.var_value(&g.value, &root, &[]);
        match &r {
            Ok(v) => {
                self.globals.borrow_mut().insert(key.clone(), GState::Done(v.clone()));
            }
            Err(_) => {
                self.globals.borrow_mut().remove(key);
            }
        }
        Some(r)
    }

    fn var_value(&self, v: &VarValue, f: &Frame, locals: &[(Key, XValue)]) -> XR<XValue> {
        match v {
            VarValue::Select(x) => self.eval(x, f, locals),
            VarValue::Empty => Ok(XValue::Str(String::new())),
            VarValue::Body(b) => {
                let mut l = locals.to_vec();
                let doc = self.build_tree(b, f, &mut l)?;
                Ok(XValue::Nodes(vec![XNode::new(self.docs.add(doc), 0)]))
            }
            VarValue::Seq(b) => {
                let mut l = locals.to_vec();
                self.capture(b, f, &mut l)
            }
        }
    }

    /// Evaluate a body as a sequence (`xsl:sequence` items and constructed nodes, in order).
    fn capture(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<XValue> {
        let mut o = Out::new();
        o.items = Some(Vec::new());
        self.outs.borrow_mut().push(o);
        let r = self.exec_body(body, f, locals);
        let mut out = self.outs.borrow_mut().pop().unwrap();
        r?;
        out.flush();
        let items = out.items.take().unwrap_or_default();
        let d = self.docs.add(out.doc);
        let vals: Vec<XValue> = items
            .into_iter()
            .map(|i| match i {
                Item::Val(v) => v,
                Item::Node(id) => XValue::Nodes(vec![XNode::new(d, id)]),
            })
            .collect();
        Ok(crate::xpath::sequence(vals, &self.docs))
    }

    /// Call a stylesheet function (`xsl:function`).
    fn call_function(&self, func: &Function, name: &str, args: &[XValue], node: XNode) -> XR<XValue> {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(format!("Function recursion is too deep in {}() (more than {} nested calls)", name, MAX_DEPTH));
        }
        self.depth.set(d);
        let mut locals: Vec<(Key, XValue)> = func.params.iter().cloned().zip(args.iter().cloned()).collect();
        let f = Frame { node, pos: 1, size: 1 };
        let r = self.capture(&func.body, &f, &mut locals);
        self.depth.set(d - 1);
        r
    }

    fn build_tree(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<Document> {
        self.outs.borrow_mut().push(Out::new());
        let r = self.exec_body(body, f, locals);
        let out = self.outs.borrow_mut().pop().unwrap();
        r?;
        Ok(out.doc)
    }

    fn body_string(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<String> {
        Ok(self.build_tree(body, f, locals)?.string_value(0))
    }

    // ---- keys and documents ----

    fn key_fn(&self, name: &str, value: XValue, doc: u32) -> XR<XValue> {
        let lname = local_part(name).to_string();
        if !self.ss.keys.iter().any(|k| k.name.1 == lname) {
            return Err(format!("There is no xsl:key named '{}'", name));
        }
        let idx_key = (lname.clone(), doc);
        if !self.key_index.borrow().contains_key(&idx_key) {
            let mut index: HashMap<String, Vec<XNode>> = HashMap::new();
            let d = self.docs.get(doc);
            for k in self.ss.keys.iter().filter(|k| k.name.1 == lname) {
                for id in 0..d.nodes.len() {
                    // only nodes attached to the tree of the document
                    let n = XNode::new(doc, id);
                    let ty = d.nodes[id].ty;
                    if ty == NodeType::Document && id != 0 {
                        continue;
                    }
                    if !self.pattern_matches(&k.pattern, n)? {
                        continue;
                    }
                    let f = Frame { node: n, pos: 1, size: 1 };
                    match self.eval(&k.use_expr, &f, &[])? {
                        XValue::Nodes(ns) => {
                            for m in ns {
                                index.entry(self.docs.string_value(m)).or_default().push(n);
                            }
                        }
                        other => index.entry(other.to_string_value(&self.docs)).or_default().push(n),
                    }
                }
            }
            for v in index.values_mut() {
                v.sort();
                v.dedup();
            }
            self.key_index.borrow_mut().insert(idx_key.clone(), index);
        }
        let idx = self.key_index.borrow();
        let index = &idx[&idx_key];
        let mut out = Vec::new();
        match value {
            XValue::Nodes(ns) => {
                for n in ns {
                    if let Some(v) = index.get(&self.docs.string_value(n)) {
                        out.extend(v.iter().copied());
                    }
                }
            }
            other => {
                if let Some(v) = index.get(&other.to_string_value(&self.docs)) {
                    out.extend(v.iter().copied());
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(XValue::Nodes(out))
    }

    fn load_document(&self, href: &str) -> XR<Option<u32>> {
        if href.is_empty() {
            if let Some(d) = self.stylesheet_doc.get() {
                return Ok(Some(d));
            }
            let d = self.docs.add((*self.ss.source).clone());
            self.stylesheet_doc.set(Some(d));
            return Ok(Some(d));
        }
        let path = resolve_href(href, self.ss.base_dir.as_deref());
        if let Some(&d) = self.loaded.borrow().get(&path) {
            return Ok(Some(d));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("Can not load requested doc: {} ({})", path.display(), e))?;
        let mut doc = Document::parse_bytes(&bytes).map_err(|e| format!("Can not load requested doc: {} ({})", path.display(), e))?;
        doc.base_uri = Some(path.display().to_string());
        strip_space(self.ss, &mut doc);
        let d = self.docs.add(doc);
        self.loaded.borrow_mut().insert(path, d);
        Ok(Some(d))
    }

    fn document_fn(&self, args: &[XValue]) -> XR<XValue> {
        let mut out = Vec::new();
        match args.first() {
            Some(XValue::Nodes(ns)) => {
                for n in ns {
                    if let Some(d) = self.load_document(&self.docs.string_value(*n))? {
                        out.push(XNode::new(d, 0));
                    }
                }
            }
            Some(other) => {
                if let Some(d) = self.load_document(&other.to_string_value(&self.docs))? {
                    out.push(XNode::new(d, 0));
                }
            }
            None => return Err("document() requires an argument".into()),
        }
        out.sort();
        out.dedup();
        Ok(XValue::Nodes(out))
    }

    // ---- output ----

    fn with_out<T>(&self, f: impl FnOnce(&mut Out) -> T) -> T {
        let mut outs = self.outs.borrow_mut();
        f(outs.last_mut().unwrap())
    }

    fn out_text(&self, s: &str) {
        self.with_out(|o| {
            let p = *o.stack.last().unwrap();
            o.doc.append_text(p, s);
        });
    }

    fn out_raw(&self, s: &str) {
        self.with_out(|o| {
            let p = *o.stack.last().unwrap();
            o.doc.append_pi(p, RAW_TEXT_TARGET, s);
        });
    }

    fn out_attribute(&self, name: QName, value: String) {
        self.with_out(|o| {
            let p = *o.stack.last().unwrap();
            if o.doc.node_type(p) == NodeType::Element && o.doc.children(p).is_empty() {
                o.doc.set_attribute(p, name, value);
            }
        });
    }

    fn out_namespace(&self, prefix: &str, uri: &str) {
        self.with_out(|o| {
            let p = *o.stack.last().unwrap();
            if o.doc.node_type(p) == NodeType::Element && o.doc.children(p).is_empty() {
                o.doc.declare_namespace(p, prefix, uri);
            }
        });
    }

    fn start_element(&self, name: QName) -> NodeId {
        self.with_out(|o| {
            let p = *o.stack.last().unwrap();
            let el = o.doc.append_element(p, name);
            o.stack.push(el);
            el
        })
    }

    fn end_element(&self) {
        self.with_out(|o| {
            o.stack.pop();
        });
    }

    fn copy_node(&self, n: XNode) {
        let src = self.docs.get(n.doc);
        if n.ns != 0 {
            if let Some((p, u)) = src.in_scope_namespaces(n.node_id()).get(n.ns as usize - 1) {
                if p != "xml" {
                    self.out_namespace(p, u);
                }
            }
            return;
        }
        let id = n.node_id();
        match src.node_type(id) {
            NodeType::Attribute => self.out_attribute(src.name(id).clone(), src.value(id).to_string()),
            _ => self.with_out(|o| {
                let p = *o.stack.last().unwrap();
                o.doc.import_node(&src, id, p);
            }),
        }
    }

    fn apply_sets(&self, names: &[Key], depth: usize) -> XR<()> {
        if depth > 50 {
            return Err("Circular use-attribute-sets".into());
        }
        for n in names {
            let (uses, instrs) = self.ss.attribute_sets.get(n).ok_or_else(|| format!("Unknown attribute set '{}'", n.1))?;
            self.apply_sets(uses, depth + 1)?;
            let root = Frame { node: XNode::new(0, 0), pos: 1, size: 1 };
            let mut l = Vec::new();
            self.exec_body(instrs, &root, &mut l)?;
        }
        Ok(())
    }

    // ---- templates ----

    fn find_template(&self, n: XNode, mode: &Option<Key>, below: Option<usize>) -> XR<Option<usize>> {
        let mut best: Option<usize> = None;
        for (i, t) in self.ss.templates.iter().enumerate() {
            let Some(p) = &t.pattern else { continue };
            if t.mode != *mode {
                continue;
            }
            if let Some(b) = below {
                if t.precedence >= b {
                    continue;
                }
            }
            if let Some(bi) = best {
                let b = &self.ss.templates[bi];
                if (t.precedence, t.priority) < (b.precedence, b.priority) {
                    continue;
                }
            }
            let env = EvalEnv { ex: self, locals: &[], current: n };
            if p.matches(&self.docs, n, &env).map_err(xerr)? {
                match best {
                    Some(bi) => {
                        let b = &self.ss.templates[bi];
                        if (t.precedence, t.priority, t.order) >= (b.precedence, b.priority, b.order) {
                            best = Some(i);
                        }
                    }
                    None => best = Some(i),
                }
            }
        }
        Ok(best)
    }

    fn process_node(&self, f: Frame, mode: &Option<Key>, params: &[(Key, XValue)]) -> XR<()> {
        match self.find_template(f.node, mode, None)? {
            Some(t) => self.invoke(t, f, params, mode),
            None => self.builtin(f, mode),
        }
    }

    fn builtin(&self, f: Frame, mode: &Option<Key>) -> XR<()> {
        match self.docs.node_type(f.node) {
            Some(NodeType::Document) | Some(NodeType::Element) => {
                let kids = axis_nodes(&self.docs, f.node, crate::xpath::ast::Axis::Child);
                let size = kids.len();
                for (i, k) in kids.into_iter().enumerate() {
                    self.process_node(Frame { node: k, pos: i + 1, size }, mode, &[])?;
                }
                Ok(())
            }
            Some(NodeType::Text) | Some(NodeType::Attribute) => {
                self.out_text(&self.docs.string_value(f.node));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn invoke(&self, t: usize, f: Frame, params: &[(Key, XValue)], mode: &Option<Key>) -> XR<()> {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(format!("Template recursion is too deep (more than {} nested template calls)", MAX_DEPTH));
        }
        self.depth.set(d);
        let rule = &self.ss.templates[t];
        let mut locals: Vec<(Key, XValue)> = Vec::new();
        let r = (|| -> XR<()> {
            let tunnel = self.tunnel.borrow().last().cloned().unwrap_or_default();
            for p in rule.params.iter() {
                let given = if p.tunnel { tunnel.iter().find(|(k, _)| *k == p.name) } else { params.iter().find(|(k, _)| *k == p.name) };
                let v = match given {
                    Some((_, v)) => v.clone(),
                    None => self.var_value(&p.value, &f, &locals)?,
                };
                locals.push((p.name.clone(), v));
            }
            self.rules.borrow_mut().push((t, mode.clone()));
            let r = self.exec_body(&rule.body, &f, &mut locals);
            self.rules.borrow_mut().pop();
            r
        })();
        self.depth.set(d - 1);
        r
    }

    fn eval_params(&self, params: &[WithParam], f: &Frame, locals: &[(Key, XValue)]) -> XR<Vec<(Key, XValue)>> {
        let mut out = Vec::new();
        for p in params.iter().filter(|p| !p.tunnel) {
            out.push((p.name.clone(), self.var_value(&p.value, f, locals)?));
        }
        Ok(out)
    }

    /// Evaluate tunnel parameters; returns true when a new tunnel set was pushed.
    fn push_tunnel(&self, params: &[WithParam], f: &Frame, locals: &[(Key, XValue)]) -> XR<bool> {
        if !params.iter().any(|p| p.tunnel) {
            return Ok(false);
        }
        let mut set: Vec<(Key, XValue)> = self.tunnel.borrow().last().map(|t| t.as_ref().clone()).unwrap_or_default();
        for p in params.iter().filter(|p| p.tunnel) {
            let v = self.var_value(&p.value, f, locals)?;
            set.retain(|(k, _)| *k != p.name);
            set.push((p.name.clone(), v));
        }
        self.tunnel.borrow_mut().push(Rc::new(set));
        Ok(true)
    }

    fn with_tunnel<T>(&self, params: &[WithParam], f: &Frame, locals: &[(Key, XValue)], run: impl FnOnce() -> XR<T>) -> XR<T> {
        let pushed = self.push_tunnel(params, f, locals)?;
        let r = run();
        if pushed {
            self.tunnel.borrow_mut().pop();
        }
        r
    }

    fn sort(&self, nodes: Vec<XNode>, sorts: &[SortSpec], f: &Frame, locals: &[(Key, XValue)]) -> XR<Vec<XNode>> {
        if sorts.is_empty() || nodes.len() < 2 {
            return Ok(nodes);
        }
        let size = nodes.len();
        let order = self.sort_indices(size, sorts, f, locals, &mut |i, s| {
            let nf = Frame { node: nodes[i], pos: i + 1, size };
            self.eval_string(&s.select, &nf, locals)
        })?;
        Ok(order.into_iter().map(|i| nodes[i]).collect())
    }

    /// Sort `n` items by `xsl:sort` keys; `key(i, spec)` gives item `i`'s key string.
    fn sort_indices(&self, n: usize, sorts: &[SortSpec], f: &Frame, locals: &[(Key, XValue)], key: &mut dyn FnMut(usize, &SortSpec) -> XR<String>) -> XR<Vec<usize>> {
        enum K {
            Num(f64),
            Text(Vec<u32>, String, String),
        }
        let mut specs = Vec::new();
        for s in sorts {
            let dt = self.avt(&s.data_type, f, locals)?;
            let desc = self.avt(&s.order, f, locals)? == "descending";
            let lower_first = self.avt(&s.case_order, f, locals)? == "lower-first";
            let lang = match &s.lang {
                Some(a) => self.avt(a, f, locals)?,
                None => String::new(),
            };
            let collation = match &s.collation {
                Some(a) => Some(self.avt(a, f, locals)?),
                None => None,
            };
            let coll = Collation::new(collation.as_deref(), &lang)?;
            specs.push((dt == "number", desc, lower_first, coll));
        }
        let mut keys: Vec<(usize, Vec<K>)> = Vec::with_capacity(n);
        for i in 0..n {
            let mut ks = Vec::new();
            for (s, (num, _, _, coll)) in sorts.iter().zip(&specs) {
                let v = key(i, s)?;
                ks.push(if *num {
                    K::Num(crate::xpath::string_to_number(&v))
                } else {
                    let (p, sec) = coll.keys(&v);
                    K::Text(p, sec, v)
                });
            }
            keys.push((i, ks));
        }
        keys.sort_by(|a, b| {
            for (j, (_, desc, lower_first, coll)) in specs.iter().enumerate() {
                let o = match (&a.1[j], &b.1[j]) {
                    (K::Num(x), K::Num(y)) => match (x.is_nan(), y.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => x.partial_cmp(y).unwrap_or(Ordering::Equal),
                    },
                    (K::Text(pa, sa, x), K::Text(pb, sb, y)) => {
                        if coll.codepoint {
                            x.cmp(y)
                        } else {
                            let o = pa.cmp(pb);
                            let o = if coll.strength >= 2 { o.then_with(|| sa.cmp(sb)) } else { o };
                            if coll.strength >= 3 {
                                o.then_with(|| {
                                    let c = x.cmp(y); // uppercase sorts before lowercase in code point order
                                    if *lower_first {
                                        c.reverse()
                                    } else {
                                        c
                                    }
                                })
                            } else {
                                o
                            }
                        }
                    }
                    _ => Ordering::Equal,
                };
                let o = if *desc { o.reverse() } else { o };
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        Ok(keys.into_iter().map(|(i, _)| i).collect())
    }

    fn number(&self, spec: &NumberSpec, f: &Frame, locals: &[(Key, XValue)]) -> XR<String> {
        let nums: Vec<u64> = if let Some(v) = &spec.value {
            let x = self.eval(v, f, locals)?.to_number(&self.docs);
            if x.is_nan() || x.is_infinite() || x < 0.5 {
                return Ok(crate::xpath::format_number(x));
            }
            vec![(x + 0.5).floor() as u64]
        } else {
            let cur = f.node;
            let (cp, cl, cu) = self.docs.node_name(cur);
            let _ = cp;
            let cty = self.docs.node_type(cur);
            let count_match = |n: XNode| -> XR<bool> {
                match &spec.count {
                    Some(p) => self.pattern_matches(p, n),
                    None => {
                        let (_, l, u) = self.docs.node_name(n);
                        Ok(self.docs.node_type(n) == cty && l == cl && u == cu)
                    }
                }
            };
            let from_match = |n: XNode| -> XR<bool> {
                match &spec.from {
                    Some(p) => self.pattern_matches(p, n),
                    None => Ok(false),
                }
            };
            let ancestors_or_self = axis_nodes(&self.docs, cur, crate::xpath::ast::Axis::AncestorOrSelf);
            let single = |n: XNode| -> XR<u64> {
                let mut c = 1;
                for s in axis_nodes(&self.docs, n, crate::xpath::ast::Axis::PrecedingSibling) {
                    if count_match(s)? {
                        c += 1;
                    }
                }
                Ok(c)
            };
            match spec.level.as_str() {
                "multiple" => {
                    let mut v = Vec::new();
                    for &a in &ancestors_or_self {
                        if from_match(a)? {
                            break;
                        }
                        if count_match(a)? {
                            v.push(single(a)?);
                        }
                    }
                    v.reverse();
                    v
                }
                "any" => {
                    let mut c = 0u64;
                    let mut nodes: Vec<XNode> = axis_nodes(&self.docs, cur, crate::xpath::ast::Axis::Preceding);
                    nodes.extend(axis_nodes(&self.docs, cur, crate::xpath::ast::Axis::Ancestor));
                    nodes.sort();
                    nodes.push(cur);
                    // count back to the last node matching `from`
                    for &n in nodes.iter().rev() {
                        if count_match(n)? {
                            c += 1;
                        }
                        if spec.from.is_some() && from_match(n)? {
                            break;
                        }
                    }
                    if c == 0 {
                        Vec::new()
                    } else {
                        vec![c]
                    }
                }
                _ => {
                    let mut target = None;
                    for &a in &ancestors_or_self {
                        if count_match(a)? {
                            target = Some(a);
                            break;
                        }
                        if from_match(a)? {
                            break;
                        }
                    }
                    match target {
                        Some(t) => vec![single(t)?],
                        None => Vec::new(),
                    }
                }
            }
        };
        let format = self.avt(&spec.format, f, locals)?;
        let grouping = match (&spec.grouping_separator, &spec.grouping_size) {
            (Some(s), Some(z)) => {
                let sep = self.avt(s, f, locals)?.chars().next().unwrap_or(',');
                let size = self.avt(z, f, locals)?.trim().parse().unwrap_or(0);
                Some((sep, size))
            }
            _ => None,
        };
        let letter_value = match &spec.letter_value {
            Some(a) => self.avt(a, f, locals)?,
            None => String::new(),
        };
        let lang = match &spec.lang {
            Some(a) => self.avt(a, f, locals)?,
            None => String::new(),
        };
        Ok(format_number_list_lang(&nums, &format, grouping, NumberLang { letter_value: &letter_value, lang: &lang }))
    }

    fn resolve_name(&self, name: &str, ns: Option<String>, ns_ctx: &[(String, String)], element: bool) -> XR<QName> {
        let name = name.trim();
        if !crate::parser::is_name(name) || name.starts_with(':') || name.ends_with(':') || name.matches(':').count() > 1 {
            return Err(format!("Invalid {} name: '{}'", if element { "element" } else { "attribute" }, name));
        }
        let (prefix, local) = match name.split_once(':') {
            Some((p, l)) => (p.to_string(), l.to_string()),
            None => (String::new(), name.to_string()),
        };
        if !element && name == "xmlns" {
            return Err("An attribute cannot be named 'xmlns'".into());
        }
        let uri = match ns {
            Some(u) => u,
            None => {
                if prefix.is_empty() {
                    if element {
                        ns_ctx.iter().find(|(p, _)| p.is_empty()).map(|(_, u)| u.clone()).unwrap_or_default()
                    } else {
                        String::new()
                    }
                } else {
                    ns_ctx.iter().find(|(p, _)| *p == prefix).map(|(_, u)| u.clone()).ok_or_else(|| format!("Undeclared namespace prefix '{}' in name '{}'", prefix, name))?
                }
            }
        };
        let prefix = if !element && prefix.is_empty() && !uri.is_empty() { "ns0".to_string() } else { prefix };
        let prefix = if element && !uri.is_empty() && prefix.is_empty() { String::new() } else { prefix };
        let prefix = if uri.is_empty() { String::new() } else { prefix };
        Ok(QName::ns(prefix, local, uri))
    }

    /// Find the next template rule below `(precedence, priority, order)` (`xsl:next-match`).
    fn find_template_below(&self, n: XNode, mode: &Option<Key>, below: (usize, f64, usize)) -> XR<Option<usize>> {
        let mut best: Option<usize> = None;
        for (i, t) in self.ss.templates.iter().enumerate() {
            let Some(p) = &t.pattern else { continue };
            if t.mode != *mode {
                continue;
            }
            let k = (t.precedence, t.priority, t.order);
            if k.partial_cmp(&below) != Some(Ordering::Less) {
                continue;
            }
            if let Some(bi) = best {
                let b = &self.ss.templates[bi];
                if k.partial_cmp(&(b.precedence, b.priority, b.order)) != Some(Ordering::Greater) {
                    continue;
                }
            }
            let env = EvalEnv { ex: self, locals: &[], current: n };
            if p.matches(&self.docs, n, &env).map_err(xerr)? {
                best = Some(i);
            }
        }
        Ok(best)
    }

    /// Whether a node is an item of an atomic sequence (a text node directly under a
    /// document node).
    fn is_atomic_item(&self, n: XNode) -> bool {
        if n.ns != 0 {
            return false;
        }
        let d = self.docs.get(n.doc);
        let id = n.node_id();
        d.node_type(id) == NodeType::Text && d.parent(id) == Some(0)
    }

    /// `xsl:sequence`: add items to a captured sequence, or write them to the result tree.
    fn emit_sequence(&self, v: XValue) {
        let captured = self.with_out(|o| {
            if o.items.is_some() && o.stack.len() == 1 {
                o.flush();
                o.items.as_mut().unwrap().push(Item::Val(v.clone()));
                true
            } else {
                false
            }
        });
        if captured {
            return;
        }
        let mut prev_atomic = false;
        for item in crate::xpath::items(v) {
            match item {
                XValue::Nodes(ns) => {
                    for n in ns {
                        let atomic = self.is_atomic_item(n);
                        if atomic && prev_atomic {
                            self.out_text(" ");
                        }
                        self.copy_node(n);
                        prev_atomic = atomic;
                    }
                }
                other => {
                    if prev_atomic {
                        self.out_text(" ");
                    }
                    self.out_text(&other.to_string_value(&self.docs));
                    prev_atomic = true;
                }
            }
        }
    }

    /// Emit nodes in the given order (not document order) as a sequence.
    fn emit_ordered(&self, nodes: Vec<XNode>) {
        let captured = self.with_out(|o| o.items.is_some() && o.stack.len() == 1);
        if captured {
            // copy into the capture document so the order is preserved
            let strings: Vec<String> = nodes.iter().map(|n| self.docs.string_value(*n)).collect();
            if nodes.iter().all(|n| self.is_atomic_item(*n)) {
                self.emit_sequence(self.docs.atomic_sequence(&strings));
            } else {
                self.with_out(|o| o.flush());
                for n in nodes {
                    self.copy_node(n);
                }
            }
            return;
        }
        for n in nodes {
            self.emit_sequence(XValue::Nodes(vec![n]));
        }
    }

    fn for_each_group(&self, select: &XPath, kind: &GroupKind, sorts: &[SortSpec], body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<()> {
        let population = self.eval_nodes(select, f, locals)?;
        let size = population.len();
        let mut groups: Vec<Group> = Vec::new();
        match kind {
            GroupKind::By(key) => {
                let mut index: HashMap<String, usize> = HashMap::new();
                for (i, &n) in population.iter().enumerate() {
                    let nf = Frame { node: n, pos: i + 1, size };
                    let v = self.eval(key, &nf, locals)?;
                    let mut seen_here: Vec<usize> = Vec::new();
                    for k in crate::xpath::items(v) {
                        let ks = k.to_string_value(&self.docs);
                        let gi = match index.get(&ks) {
                            Some(&gi) => gi,
                            None => {
                                let key = match &k {
                                    XValue::Nodes(_) => XValue::Str(ks.clone()),
                                    other => other.clone(),
                                };
                                groups.push(Group { items: Vec::new(), key });
                                index.insert(ks, groups.len() - 1);
                                groups.len() - 1
                            }
                        };
                        if !seen_here.contains(&gi) {
                            groups[gi].items.push(n);
                            seen_here.push(gi);
                        }
                    }
                }
            }
            GroupKind::Adjacent(key) => {
                let mut last: Option<String> = None;
                for (i, &n) in population.iter().enumerate() {
                    let nf = Frame { node: n, pos: i + 1, size };
                    let v = self.eval(key, &nf, locals)?;
                    let ks = v.to_string_value(&self.docs);
                    if last.as_deref() == Some(ks.as_str()) {
                        groups.last_mut().unwrap().items.push(n);
                    } else {
                        let key = match &v {
                            XValue::Nodes(_) => XValue::Str(ks.clone()),
                            other => other.clone(),
                        };
                        groups.push(Group { items: vec![n], key });
                        last = Some(ks);
                    }
                }
            }
            GroupKind::StartingWith(p) => {
                for &n in &population {
                    if groups.is_empty() || self.pattern_matches(p, n)? {
                        groups.push(Group { items: vec![n], key: XValue::Nodes(Vec::new()) });
                    } else {
                        groups.last_mut().unwrap().items.push(n);
                    }
                }
            }
            GroupKind::EndingWith(p) => {
                let mut open = false;
                for &n in &population {
                    if !open {
                        groups.push(Group { items: Vec::new(), key: XValue::Nodes(Vec::new()) });
                        open = true;
                    }
                    groups.last_mut().unwrap().items.push(n);
                    if self.pattern_matches(p, n)? {
                        open = false;
                    }
                }
            }
        }
        let groups: Vec<Rc<Group>> = groups.into_iter().map(Rc::new).collect();
        let order: Vec<usize> = if sorts.is_empty() {
            (0..groups.len()).collect()
        } else {
            let gsize = groups.len();
            self.sort_indices(gsize, sorts, f, locals, &mut |i, s| {
                self.groups.borrow_mut().push(groups[i].clone());
                let nf = Frame { node: groups[i].items[0], pos: i + 1, size: gsize };
                let r = self.eval_string(&s.select, &nf, locals);
                self.groups.borrow_mut().pop();
                r
            })?
        };
        let gsize = order.len();
        for (pos, gi) in order.into_iter().enumerate() {
            let g = groups[gi].clone();
            let nf = Frame { node: g.items[0], pos: pos + 1, size: gsize };
            self.groups.borrow_mut().push(g);
            let r = self.exec_body(body, &nf, locals);
            self.groups.borrow_mut().pop();
            r?;
        }
        Ok(())
    }

    fn exec_body(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<()> {
        let mark = locals.len();
        let r = self.exec_instrs(body, f, locals);
        locals.truncate(mark);
        r
    }

    fn exec_instrs(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<()> {
        for ins in body {
            match ins {
                Instr::Text(s) => self.out_text(s),
                Instr::RawText(s) => self.out_raw(s),
                Instr::ValueOf { select, raw, separator, v2 } => {
                    let s = if *v2 {
                        let sep = match separator {
                            Some(a) => self.avt(a, f, locals)?,
                            None => " ".to_string(),
                        };
                        let v = self.eval(select, f, locals)?;
                        crate::xpath::items(v).iter().map(|i| i.to_string_value(&self.docs)).collect::<Vec<_>>().join(&sep)
                    } else {
                        self.eval_string(select, f, locals)?
                    };
                    if *raw {
                        self.out_raw(&s);
                    } else {
                        self.out_text(&s);
                    }
                }
                Instr::LiteralElement { name, ns, attrs, use_sets, body } => {
                    self.start_element(name.clone());
                    let r = (|| -> XR<()> {
                        for (p, u) in ns {
                            self.out_namespace(p, u);
                        }
                        self.apply_sets(use_sets, 0)?;
                        for (an, av) in attrs {
                            let v = self.avt(av, f, locals)?;
                            self.out_attribute(an.clone(), v);
                        }
                        self.exec_body(body, f, locals)
                    })();
                    self.end_element();
                    r?;
                }
                Instr::Element { name, namespace, ns_ctx, use_sets, body } => {
                    let n = self.avt(name, f, locals)?;
                    let ns = match namespace {
                        Some(a) => Some(self.avt(a, f, locals)?),
                        None => None,
                    };
                    let q = self.resolve_name(&n, ns, ns_ctx, true)?;
                    self.start_element(q);
                    let r = (|| -> XR<()> {
                        self.apply_sets(use_sets, 0)?;
                        self.exec_body(body, f, locals)
                    })();
                    self.end_element();
                    r?;
                }
                Instr::Attribute { name, namespace, ns_ctx, body } => {
                    let n = self.avt(name, f, locals)?;
                    let ns = match namespace {
                        Some(a) => Some(self.avt(a, f, locals)?),
                        None => None,
                    };
                    let q = self.resolve_name(&n, ns, ns_ctx, false)?;
                    let v = self.body_string(body, f, locals)?;
                    self.out_attribute(q, v);
                }
                Instr::Comment(body) => {
                    let mut s = self.body_string(body, f, locals)?.replace("--", "- -");
                    if s.ends_with('-') {
                        s.push(' ');
                    }
                    self.with_out(|o| {
                        let p = *o.stack.last().unwrap();
                        o.doc.append_comment(p, &s);
                    });
                }
                Instr::Pi { name, body } => {
                    let n = self.avt(name, f, locals)?;
                    if !crate::parser::is_ncname(&n) || n.eq_ignore_ascii_case("xml") {
                        return Err(format!("Invalid processing instruction name: '{}'", n));
                    }
                    let s = self.body_string(body, f, locals)?.replace("?>", "? >");
                    self.with_out(|o| {
                        let p = *o.stack.last().unwrap();
                        o.doc.append_pi(p, &n, s.trim_start());
                    });
                }
                Instr::ApplyTemplates { select, mode, sorts, params } => {
                    let nodes = match select {
                        Some(s) => self.eval_nodes(s, f, locals)?,
                        None => axis_nodes(&self.docs, f.node, crate::xpath::ast::Axis::Child),
                    };
                    let nodes = self.sort(nodes, sorts, f, locals)?;
                    let pv = self.eval_params(params, f, locals)?;
                    let size = nodes.len();
                    self.with_tunnel(params, f, locals, || {
                        for (i, n) in nodes.into_iter().enumerate() {
                            self.process_node(Frame { node: n, pos: i + 1, size }, mode, &pv)?;
                        }
                        Ok(())
                    })?;
                }
                Instr::CallTemplate { name, params } => {
                    let t = *self.ss.named.get(name).ok_or_else(|| format!("Could not find template named: {}", name.1))?;
                    let pv = self.eval_params(params, f, locals)?;
                    let mode = self.rules.borrow().last().and_then(|(_, m)| m.clone());
                    self.with_tunnel(params, f, locals, || self.invoke(t, *f, &pv, &mode))?;
                }
                Instr::NextMatch { params } => {
                    let cur = self.rules.borrow().last().cloned();
                    let Some((t, mode)) = cur else { return Err("xsl:next-match used outside of a template rule".into()) };
                    let pv = self.eval_params(params, f, locals)?;
                    let below = { let r = &self.ss.templates[t]; (r.precedence, r.priority, r.order) };
                    self.with_tunnel(params, f, locals, || match self.find_template_below(f.node, &mode, below)? {
                        Some(t2) => self.invoke(t2, *f, &pv, &mode),
                        None => self.builtin(*f, &mode),
                    })?;
                }
                Instr::ValueOfBody { body, separator, raw } => {
                    let v = self.capture(body, f, locals)?;
                    let sep = match separator {
                        Some(a) => self.avt(a, f, locals)?,
                        None => String::new(),
                    };
                    let s = crate::xpath::items(v).iter().map(|i| i.to_string_value(&self.docs)).collect::<Vec<_>>().join(&sep);
                    if *raw {
                        self.out_raw(&s);
                    } else {
                        self.out_text(&s);
                    }
                }
                Instr::Sequence(select) => {
                    let v = self.eval(select, f, locals)?;
                    self.emit_sequence(v);
                }
                Instr::PerformSort { select, sorts, body } => {
                    let nodes = match select {
                        Some(s) => self.eval_nodes(s, f, locals)?,
                        None => match self.capture(body, f, locals)? {
                            XValue::Nodes(n) => n,
                            other => match self.docs.atomic_sequence(&[other.to_string_value(&self.docs)]) {
                                XValue::Nodes(n) => n,
                                _ => Vec::new(),
                            },
                        },
                    };
                    let sorted = self.sort(nodes, sorts, f, locals)?;
                    // the sorted order is kept as an atomic / node sequence in sort order
                    self.emit_ordered(sorted);
                }
                Instr::ForEachGroup { select, kind, sorts, body } => self.for_each_group(select, kind, sorts, body, f, locals)?,
                Instr::AnalyzeString { select, regex, flags, matching, non_matching } => {
                    let input = self.eval_string(select, f, locals)?;
                    let pattern = self.avt(regex, f, locals)?;
                    let flags = self.avt(flags, f, locals)?;
                    let re = crate::xpath::f2::regex(&pattern, &flags).map_err(xerr)?;
                    if re.is_match("") {
                        return Err(format!("The regular expression in xsl:analyze-string must not match a zero-length string: {}", pattern));
                    }
                    let mut pieces: Vec<(bool, String, Vec<String>)> = Vec::new();
                    let mut last = 0;
                    for c in re.captures_iter(&input) {
                        let Some((s0, e0)) = c.span(0) else { continue };
                        if s0 > last {
                            pieces.push((false, input[last..s0].to_string(), Vec::new()));
                        }
                        let groups: Vec<String> = (0..c.len()).map(|i| c.get(i).unwrap_or("").to_string()).collect();
                        pieces.push((true, input[s0..e0].to_string(), groups));
                        last = e0;
                    }
                    if last < input.len() {
                        pieces.push((false, input[last..].to_string(), Vec::new()));
                    }
                    let size = pieces.len();
                    for (i, (m, text, groups)) in pieces.into_iter().enumerate() {
                        let node = match self.docs.atomic_sequence(&[text]) {
                            XValue::Nodes(n) => n[0],
                            _ => unreachable!(),
                        };
                        let nf = Frame { node, pos: i + 1, size };
                        if m {
                            self.regex_groups.borrow_mut().push(Rc::new(groups));
                            let r = self.exec_body(matching, &nf, locals);
                            self.regex_groups.borrow_mut().pop();
                            r?;
                        } else {
                            self.exec_body(non_matching, &nf, locals)?;
                        }
                    }
                }
                Instr::ResultDocument { href, method, indent, omit_decl, body } => {
                    let href = self.avt(href, f, locals)?;
                    if href.trim().is_empty() {
                        self.exec_body(body, f, locals)?;
                        continue;
                    }
                    let doc = self.build_tree(body, f, locals)?;
                    let mut opts = self.ss.output.clone();
                    if let Some(m) = method {
                        opts.method = Some(match self.avt(m, f, locals)?.trim() {
                            "html" => Method::Html,
                            "text" => Method::Text,
                            _ => Method::Xml,
                        });
                    }
                    if let Some(i) = indent {
                        opts.indent = self.avt(i, f, locals)?.trim() == "yes";
                    }
                    if let Some(o) = omit_decl {
                        opts.omit_xml_declaration = self.avt(o, f, locals)?.trim() == "yes";
                    }
                    let text = crate::serialize::serialize(&doc, 0, &opts);
                    if self.secondary.borrow().iter().any(|(h, _)| *h == href) {
                        return Err(format!("xsl:result-document: cannot write more than one result document to {}", href));
                    }
                    let path = resolve_href(&href, None);
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory {}: {}", parent.display(), e))?;
                        }
                    }
                    std::fs::write(&path, text.as_bytes()).map_err(|e| format!("Cannot write result document {}: {}", path.display(), e))?;
                    self.secondary.borrow_mut().push((href, text));
                }
                Instr::Namespace { name, select, body } => {
                    let prefix = self.avt(name, f, locals)?;
                    let uri = match select {
                        Some(x) => self.eval_string(x, f, locals)?,
                        None => self.body_string(body, f, locals)?,
                    };
                    self.out_namespace(prefix.trim(), &uri);
                }
                Instr::ApplyImports => {
                    let cur = self.rules.borrow().last().cloned();
                    let Some((t, mode)) = cur else { return Err("xsl:apply-imports used outside of a template rule".into()) };
                    let prec = self.ss.templates[t].precedence;
                    match self.find_template(f.node, &mode, Some(prec))? {
                        Some(t2) => self.invoke(t2, *f, &[], &mode)?,
                        None => self.builtin(*f, &mode)?,
                    }
                }
                Instr::ForEach { select, sorts, body } => {
                    let nodes = self.eval_nodes(select, f, locals)?;
                    let nodes = self.sort(nodes, sorts, f, locals)?;
                    let size = nodes.len();
                    for (i, n) in nodes.into_iter().enumerate() {
                        let nf = Frame { node: n, pos: i + 1, size };
                        self.exec_body(body, &nf, locals)?;
                    }
                }
                Instr::If { test, body } => {
                    if self.eval(test, f, locals)?.to_bool() {
                        self.exec_body(body, f, locals)?;
                    }
                }
                Instr::Choose { whens, otherwise } => {
                    let mut done = false;
                    for (t, b) in whens {
                        if self.eval(t, f, locals)?.to_bool() {
                            self.exec_body(b, f, locals)?;
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        if let Some(b) = otherwise {
                            self.exec_body(b, f, locals)?;
                        }
                    }
                }
                Instr::Variable { name, value } => {
                    let v = self.var_value(value, f, locals)?;
                    locals.push((name.clone(), v));
                }
                Instr::Copy { use_sets, body } => {
                    let n = f.node;
                    match self.docs.node_type(n) {
                        Some(NodeType::Element) => {
                            let src = self.docs.get(n.doc);
                            let id = n.node_id();
                            self.start_element(src.name(id).clone());
                            let r = (|| -> XR<()> {
                                for (p, u) in src.in_scope_namespaces(id) {
                                    if p != "xml" {
                                        self.out_namespace(&p, &u);
                                    }
                                }
                                self.apply_sets(use_sets, 0)?;
                                self.exec_body(body, f, locals)
                            })();
                            self.end_element();
                            r?;
                        }
                        Some(NodeType::Document) => self.exec_body(body, f, locals)?,
                        _ => self.copy_node(n),
                    }
                }
                Instr::CopyOf(select) => match self.eval(select, f, locals)? {
                    XValue::Nodes(ns) => {
                        for n in ns {
                            self.copy_node(n);
                        }
                    }
                    other => self.out_text(&other.to_string_value(&self.docs)),
                },
                Instr::Number(spec) => {
                    let s = self.number(spec, f, locals)?;
                    self.out_text(&s);
                }
                Instr::Message { terminate, body } => {
                    let msg = self.body_string(body, f, locals)?;
                    camel_core::camel_info!("xslt", "xsl:message: {}", msg);
                    self.messages.borrow_mut().push(msg.clone());
                    if self.avt(terminate, f, locals)? == "yes" {
                        return Err(format!("Stylesheet directed termination: {}", msg));
                    }
                }
                Instr::Unsupported(what) => {
                    return Err(format!("{} is not supported by this XSLT processor and has no xsl:fallback", what));
                }
            }
        }
        Ok(())
    }
}

/// A collation for `xsl:sort` (`lang` / `collation`).
struct Collation {
    codepoint: bool,
    /// 1 = primary (base letters), 2 = secondary (+ accents), 3 = tertiary (+ case).
    strength: u8,
    /// Letters sorted after `z` for the language (Nordic alphabets).
    tail: &'static [char],
}

impl Collation {
    fn new(uri: Option<&str>, lang: &str) -> XR<Collation> {
        let mut c = Collation { codepoint: false, strength: 3, tail: &[] };
        let mut lang = lang.to_ascii_lowercase();
        if let Some(u) = uri {
            let u = u.trim();
            if u == "http://www.w3.org/2005/xpath-functions/collation/codepoint" || u == "http://www.w3.org/2005/04/xpath-functions/collation/codepoint" {
                c.codepoint = true;
                return Ok(c);
            }
            let known = ["http://www.w3.org/2013/collation/UCA", "http://saxon.sf.net/collation", "http://www.w3.org/2005/xpath-functions/collation/html-ascii-case-insensitive"];
            if !known.iter().any(|k| u.starts_with(k)) {
                return Err(format!("Unknown collation {}", u));
            }
            if u.ends_with("html-ascii-case-insensitive") {
                c.strength = 2;
            }
            if let Some((_, q)) = u.split_once('?') {
                for kv in q.split([';', '&']) {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    match k {
                        "strength" => {
                            c.strength = match v {
                                "primary" | "1" => 1,
                                "secondary" | "2" => 2,
                                _ => 3,
                            }
                        }
                        "lang" => lang = v.to_ascii_lowercase(),
                        "ignore-case" if v == "yes" => c.strength = c.strength.min(2),
                        _ => {}
                    }
                }
            }
        }
        c.tail = match lang.split(['-', '_']).next().unwrap_or("") {
            "sv" | "fi" => &['å', 'ä', 'ö'],
            "da" | "no" | "nb" | "nn" => &['æ', 'ø', 'å'],
            _ => &[],
        };
        Ok(c)
    }

    /// Primary key (base letters, case-folded) and secondary key (accents kept, lower case).
    fn keys(&self, s: &str) -> (Vec<u32>, String) {
        let lower = s.to_lowercase();
        let mut primary = Vec::with_capacity(lower.len());
        for ch in lower.chars() {
            if let Some(i) = self.tail.iter().position(|&t| t == ch) {
                primary.push('z' as u32);
                primary.push(0x10_0000 + i as u32);
                continue;
            }
            match fold_accent(ch) {
                Some(b) => {
                    for c in b.chars() {
                        primary.push(c as u32);
                    }
                }
                None => primary.push(ch as u32),
            }
        }
        (primary, lower)
    }
}

/// The base letter(s) of an accented Latin letter.
fn fold_accent(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}
