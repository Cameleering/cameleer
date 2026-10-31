//! XSLT 1.0 execution.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use camel_core::error::CamelError;
use camel_core::value::Value;

use super::number::{format_number, format_number_list, DecimalFormat};
use super::pattern::Pattern;
use super::{resolve_href, Avt, AvtPart, Instr, Key, NumberSpec, SortSpec, Stylesheet, VarValue, WithParam};
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
                "version" => XValue::Num(1.0),
                "vendor" => XValue::Str("rust-camel".into()),
                "vendor-url" => XValue::Str("https://camel.apache.org".into()),
                _ => XValue::Str(String::new()),
            }),
            ("", "element-available") => {
                let n = s(0);
                let l = local_part(&n);
                Ok(XValue::Bool(n.contains(':') && SUPPORTED_INSTRUCTIONS.contains(&l)))
            }
            ("", "function-available") => Ok(XValue::Bool(FUNCTIONS.contains(&local_part(&s(0))))),
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
        outs: RefCell::new(vec![Out { doc: Document::new(), stack: vec![0] }]),
        globals: RefCell::new(HashMap::new()),
        ext_params: ext,
        key_index: RefCell::new(HashMap::new()),
        loaded: RefCell::new(HashMap::new()),
        stylesheet_doc: Cell::new(None),
        messages: RefCell::new(Vec::new()),
        depth: Cell::new(0),
        rules: RefCell::new(Vec::new()),
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
    Ok(TransformResult { output, tree: out.doc, messages: ex.messages.into_inner(), method, media_type })
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
        }
    }

    fn build_tree(&self, body: &[Instr], f: &Frame, locals: &mut Vec<(Key, XValue)>) -> XR<Document> {
        self.outs.borrow_mut().push(Out { doc: Document::new(), stack: vec![0] });
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
            for p in rule.params.iter() {
                let v = match params.iter().find(|(k, _)| *k == p.name) {
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
        for p in params {
            out.push((p.name.clone(), self.var_value(&p.value, f, locals)?));
        }
        Ok(out)
    }

    fn sort(&self, nodes: Vec<XNode>, sorts: &[SortSpec], f: &Frame, locals: &[(Key, XValue)]) -> XR<Vec<XNode>> {
        if sorts.is_empty() || nodes.len() < 2 {
            return Ok(nodes);
        }
        enum K {
            Num(f64),
            Text(String),
        }
        let size = nodes.len();
        let mut specs = Vec::new();
        for s in sorts {
            let dt = self.avt(&s.data_type, f, locals)?;
            let desc = self.avt(&s.order, f, locals)? == "descending";
            let lower_first = self.avt(&s.case_order, f, locals)? == "lower-first";
            specs.push((dt == "number", desc, lower_first));
        }
        let mut keys: Vec<(usize, Vec<K>)> = Vec::with_capacity(size);
        for (i, &n) in nodes.iter().enumerate() {
            let nf = Frame { node: n, pos: i + 1, size };
            let mut ks = Vec::new();
            for (s, (num, _, _)) in sorts.iter().zip(&specs) {
                let v = self.eval_string(&s.select, &nf, locals)?;
                ks.push(if *num { K::Num(crate::xpath::string_to_number(&v)) } else { K::Text(v) });
            }
            keys.push((i, ks));
        }
        keys.sort_by(|a, b| {
            for (j, (_, desc, lower_first)) in specs.iter().enumerate() {
                let o = match (&a.1[j], &b.1[j]) {
                    (K::Num(x), K::Num(y)) => match (x.is_nan(), y.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => x.partial_cmp(y).unwrap_or(Ordering::Equal),
                    },
                    (K::Text(x), K::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()).then_with(|| {
                        let c = x.cmp(y); // uppercase sorts before lowercase in code point order
                        if *lower_first {
                            c.reverse()
                        } else {
                            c
                        }
                    }),
                    _ => Ordering::Equal,
                };
                let o = if *desc { o.reverse() } else { o };
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        Ok(keys.into_iter().map(|(i, _)| nodes[i]).collect())
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
        Ok(format_number_list(&nums, &format, grouping))
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
                Instr::ValueOf { select, raw } => {
                    let s = self.eval_string(select, f, locals)?;
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
                    for (i, n) in nodes.into_iter().enumerate() {
                        self.process_node(Frame { node: n, pos: i + 1, size }, mode, &pv)?;
                    }
                }
                Instr::CallTemplate { name, params } => {
                    let t = *self.ss.named.get(name).ok_or_else(|| format!("Could not find template named: {}", name.1))?;
                    let pv = self.eval_params(params, f, locals)?;
                    let mode = self.rules.borrow().last().and_then(|(_, m)| m.clone());
                    self.invoke(t, *f, &pv, &mode)?;
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
                    return Err(format!("{} is not supported by this XSLT 1.0 processor and has no xsl:fallback", what));
                }
            }
        }
        Ok(())
    }
}
