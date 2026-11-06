//! XQuery evaluation (tree-walking interpreter over [`super::ast`]).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::dom::{Document, Node, NodeType, QName};
use crate::xpath::ast::{Axis, NodeTest};
use crate::xpath::{axis_nodes, node_test_matches, sort_nodes, Docs, XNode};

use super::ast::*;
use super::types::*;
use super::ERR_NS;

pub(crate) type R<T> = XqResult<T>;
pub(crate) type Seq = Vec<Item>;

/// Maximum depth of user-defined function calls.
pub(crate) const MAX_DEPTH: usize = 3000;
/// Stack kept free between two stack checks (one `eval` level plus the builtins it calls).
/// Below it, evaluation continues on a new segment of [`SEGMENT`] bytes.
pub(crate) const RED_ZONE: usize = 512 * 1024;
/// Size of an additional stack segment (enough for [`MAX_DEPTH`] nested calls).
pub(crate) const SEGMENT: usize = 256 << 20;

/// The focus (context item, position, size).
#[derive(Clone, Debug)]
pub(crate) struct Focus {
    pub item: Option<Item>,
    pub pos: usize,
    pub size: usize,
}

impl Focus {
    pub fn none() -> Focus {
        Focus { item: None, pos: 0, size: 0 }
    }
    pub fn of(item: Item, pos: usize, size: usize) -> Focus {
        Focus { item: Some(item), pos, size }
    }
}

/// The interpreter state of one evaluation.
pub(crate) struct Interp<'a> {
    pub docs: &'a Docs,
    pub module: &'a Module,
    pub vars: Vec<(QN, Rc<Seq>)>,
    pub globals: HashMap<QN, Rc<Seq>>,
    pub depth: usize,
    pub doc_cache: HashMap<String, XNode>,
    pub base_dir: Option<PathBuf>,
    /// Why the context item is absent (e.g. the body is not XML).
    pub context_error: Option<String>,
    pub now: String,
    /// Pending update list (XQuery Update Facility).
    pub pul: Vec<super::update::Update>,
    /// Stack address below which evaluation moves to a new segment
    /// (`cameleer_core::stack::floor(RED_ZONE)` of the thread evaluating).
    pub stack_floor: usize,
}

pub(crate) fn new_root(ty: NodeType, name: QName, value: String) -> Document {
    let mut d = Document::new();
    d.nodes.clear();
    d.nodes.push(Node { ty, parent: None, children: Vec::new(), attrs: Vec::new(), ns_decls: Vec::new(), name, value, line: 0, col: 0 });
    d
}

impl<'a> Interp<'a> {
    // ---------------------------------------------------------------- helpers

    pub fn node_type(&self, n: XNode) -> Option<NodeType> {
        self.docs.node_type(n)
    }

    pub fn string_value(&self, it: &Item) -> String {
        match it {
            Item::Node(n) => self.docs.string_value(*n),
            Item::Atomic(a) => a.to_string(),
            other => self.atomize(std::slice::from_ref(other)).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" "),
        }
    }

    pub fn atomize_item(&self, it: &Item) -> Atomic {
        match it {
            Item::Node(n) => Atomic::Untyped(self.docs.string_value(*n)),
            Item::Atomic(a) => a.clone(),
            Item::Array(_) => self.atomize(std::slice::from_ref(it)).into_iter().next().unwrap_or(Atomic::Str(String::new())),
            Item::Map(_) => Atomic::Str("map(*)".into()),
            Item::Func(_) => Atomic::Str("function(*)".into()),
        }
    }

    /// Atomization (arrays are flattened).
    pub fn atomize(&self, s: &[Item]) -> Vec<Atomic> {
        let mut out = Vec::with_capacity(s.len());
        for i in s {
            match i {
                Item::Array(members) => {
                    for m in members.iter() {
                        out.extend(self.atomize(m));
                    }
                }
                other => out.push(self.atomize_item(other)),
            }
        }
        out
    }

    /// Atomize to at most one value.
    pub fn atomize_opt(&self, s: &[Item], what: &str) -> R<Option<Atomic>> {
        if s.iter().any(|i| matches!(i, Item::Map(_) | Item::Func(_))) {
            return Err(XqError::new("FOTY0013", format!("A map or function item cannot be atomized ({})", what)));
        }
        let v = if s.iter().any(|i| matches!(i, Item::Array(_))) { self.atomize(s) } else { s.iter().map(|i| self.atomize_item(i)).collect() };
        match v.len() {
            0 => Ok(None),
            1 => Ok(v.into_iter().next()),
            n => Err(XqError::type_err(format!("A sequence of more than one item ({}) is not allowed as {}", n, what))),
        }
    }

    /// Effective boolean value.
    pub fn ebv(&self, s: &[Item]) -> R<bool> {
        match s.first() {
            None => Ok(false),
            Some(Item::Node(_)) => Ok(true),
            Some(Item::Func(_) | Item::Map(_) | Item::Array(_)) => Err(XqError::new("FORG0006", "Effective boolean value is not defined for a function, map or array")),
            Some(Item::Atomic(a)) => {
                if s.len() > 1 {
                    return Err(XqError::new("FORG0006", "Effective boolean value is not defined for a sequence of two or more items starting with an atomic value"));
                }
                Ok(match a {
                    Atomic::Bool(b) => *b,
                    Atomic::Str(s) | Atomic::Untyped(s) | Atomic::AnyUri(s) => !s.is_empty(),
                    x if x.is_numeric() => {
                        let f = x.as_f64();
                        f != 0.0 && !f.is_nan()
                    }
                    other => return Err(XqError::new("FORG0006", format!("Effective boolean value is not defined for {}", other.type_of().name()))),
                })
            }
        }
    }

    fn lookup_var(&self, q: &QN) -> R<Seq> {
        if let Some((_, v)) = self.vars.iter().rev().find(|(n, _)| n == q) {
            return Ok((**v).clone());
        }
        if let Some(v) = self.globals.get(q) {
            return Ok((**v).clone());
        }
        Err(XqError::static_err("XPST0008", format!("Variable ${} is not declared", q.local)))
    }

    fn context_item(&self, f: &Focus) -> R<Item> {
        match &f.item {
            Some(i) => Ok(i.clone()),
            None => Err(XqError::new(
                "XPDY0002",
                match &self.context_error {
                    Some(e) => format!("The context item is absent: {}", e),
                    None => "The context item is absent".to_string(),
                },
            )),
        }
    }

    pub fn add_doc(&self, d: Document) -> u32 {
        self.docs.add(d)
    }

    // ---------------------------------------------------------------- sequence types

    pub fn item_matches(&self, it: &Item, t: &ItemType) -> bool {
        match (t, it) {
            (ItemType::Item, _) => true,
            (ItemType::Empty, _) => false,
            (ItemType::Function, Item::Func(_) | Item::Map(_) | Item::Array(_)) => true,
            (ItemType::Map, Item::Map(_)) => true,
            (ItemType::Array, Item::Array(_)) => true,
            (ItemType::Function | ItemType::Map | ItemType::Array, _) => false,
            (_, Item::Func(_) | Item::Map(_) | Item::Array(_)) => false,
            (ItemType::Atomic(at), Item::Atomic(a)) => a.instance_of(*at),
            (ItemType::Atomic(_), Item::Node(_)) => false,
            (_, Item::Atomic(_)) => false,
            (kind, Item::Node(n)) => {
                let Some(nt) = self.node_type(*n) else { return matches!(kind, ItemType::AnyNode) };
                match kind {
                    ItemType::AnyNode => true,
                    ItemType::Text => nt == NodeType::Text,
                    ItemType::Comment => nt == NodeType::Comment,
                    ItemType::Pi(name) => nt == NodeType::ProcessingInstruction && name.as_ref().is_none_or(|t| self.docs.node_name(*n).1 == *t),
                    ItemType::Element(name) | ItemType::Attribute(name) => {
                        let want = if matches!(kind, ItemType::Element(_)) { NodeType::Element } else { NodeType::Attribute };
                        nt == want
                            && name.as_ref().is_none_or(|(u, l)| {
                                let (_, nl, nu) = self.docs.node_name(*n);
                                nl == *l && nu == *u
                            })
                    }
                    ItemType::Document(inner) => {
                        nt == NodeType::Document
                            && inner.as_ref().is_none_or(|i| {
                                let doc = self.docs.get(n.doc);
                                let els = doc.element_children(n.node_id());
                                els.len() == 1 && self.item_matches(&Item::Node(XNode::new(n.doc, els[0])), i)
                            })
                    }
                    _ => false,
                }
            }
        }
    }

    pub fn seq_matches(&self, s: &[Item], t: &SeqType) -> bool {
        if t.item == ItemType::Empty {
            return s.is_empty();
        }
        let occ_ok = match t.occ {
            Occ::One => s.len() == 1,
            Occ::Opt => s.len() <= 1,
            Occ::Star => true,
            Occ::Plus => !s.is_empty(),
        };
        occ_ok && s.iter().all(|i| self.item_matches(i, &t.item))
    }

    /// Apply the function conversion rules (atomization, untyped casting, numeric
    /// promotion) and check the type.
    pub fn convert_to(&self, s: Seq, t: &SeqType, what: &str) -> R<Seq> {
        let s = match &t.item {
            ItemType::Atomic(at) => {
                let mut out = Vec::with_capacity(s.len());
                for it in &s {
                    let a = self.atomize_item(it);
                    let a = match (&a, at) {
                        (Atomic::Untyped(_), AtomicType::AnyAtomic | AtomicType::Untyped) => a,
                        (Atomic::Untyped(_), AtomicType::Numeric) => cast(&a, AtomicType::Double)?,
                        (Atomic::Untyped(_), t) => cast(&a, *t)?,
                        (Atomic::Int(_) | Atomic::Dec(_) | Atomic::Flt(_), AtomicType::Double) => Atomic::Dbl(a.as_f64()),
                        (Atomic::Int(_) | Atomic::Dec(_), AtomicType::Float) => Atomic::Flt(a.as_f64()),
                        (Atomic::AnyUri(u), AtomicType::String) => Atomic::Str(u.clone()),
                        _ => a,
                    };
                    out.push(Item::Atomic(a));
                }
                out
            }
            _ => s,
        };
        if !self.seq_matches(&s, t) {
            return Err(XqError::type_err(format!("Required type of {} is {}; supplied value has {} item(s){}", what, t.describe(), s.len(), self.describe_first(&s))));
        }
        Ok(s)
    }

    fn describe_first(&self, s: &[Item]) -> String {
        match s.first() {
            Some(Item::Atomic(a)) => format!(" of type {}", a.type_of().name()),
            Some(Item::Node(n)) => format!(" starting with a {:?} node", self.node_type(*n).map(|t| format!("{:?}", t).to_lowercase()).unwrap_or_else(|| "namespace".into())),
            Some(Item::Func(_)) => " starting with a function".into(),
            Some(Item::Map(_)) => " starting with a map".into(),
            Some(Item::Array(_)) => " starting with an array".into(),
            None => String::new(),
        }
    }

    // ---------------------------------------------------------------- evaluation

    /// Evaluate `e`. Runs on the current thread while enough stack is left; deep recursion
    /// continues on a new stack segment (see [`RED_ZONE`]).
    #[inline]
    pub fn eval(&mut self, e: &Expr, f: &Focus) -> R<Seq> {
        if cameleer_core::stack::here() >= self.stack_floor {
            return self.eval_inner(e, f);
        }
        self.eval_on_new_segment(e, f)
    }

    /// The rare path of [`Interp::eval`], kept out of line so it does not enlarge every
    /// evaluation frame.
    #[cold]
    #[inline(never)]
    fn eval_on_new_segment(&mut self, e: &Expr, f: &Focus) -> R<Seq> {
        let saved = self.stack_floor;
        // SAFETY: the interpreter state (`Rc`s, the documents) is used by the segment thread
        // only while this thread waits for it; the evaluator uses no thread-locals or locks.
        let r = unsafe {
            cameleer_core::stack::on_new_stack(SEGMENT, || {
                self.stack_floor = cameleer_core::stack::floor(RED_ZONE);
                self.eval_inner(e, f)
            })
        };
        self.stack_floor = saved;
        r
    }

    fn eval_inner(&mut self, e: &Expr, f: &Focus) -> R<Seq> {
        match e {
            Expr::Seq(v) => {
                let mut out = Vec::new();
                for x in v {
                    out.extend(self.eval(x, f)?);
                }
                Ok(out)
            }
            Expr::Lit(a) => Ok(vec![Item::Atomic(a.clone())]),
            Expr::Var(q) => self.lookup_var(q),
            Expr::ContextItem => Ok(vec![self.context_item(f)?]),
            Expr::Flwor { clauses, ret } => self.flwor(clauses, ret, f),
            Expr::Quantified { every, binds, sat } => {
                let r = self.quantified(*every, binds, sat, f)?;
                Ok(vec![Item::bool(r)])
            }
            Expr::If(c, t, e) => {
                let cv = self.eval(c, f)?;
                if self.ebv(&cv)? {
                    self.eval(t, f)
                } else {
                    self.eval(e, f)
                }
            }
            Expr::Typeswitch { operand, cases, default_var, default } => {
                let v = self.eval(operand, f)?;
                for c in cases {
                    if self.seq_matches(&v, &c.ty) {
                        return self.with_binding(c.var.as_ref(), v, |me| me.eval(&c.ret, f));
                    }
                }
                self.with_binding(default_var.as_ref(), v, |me| me.eval(default, f))
            }
            Expr::TryCatch { body, catches } => {
                let base = self.vars.len();
                let depth = self.depth;
                match self.eval(body, f) {
                    Ok(v) => Ok(v),
                    Err(err) => {
                        self.vars.truncate(base);
                        self.depth = depth;
                        for c in catches {
                            if c.codes.iter().any(|code| code.as_ref().is_none_or(|l| *l == err.code)) {
                                self.vars.push((QN::new(ERR_NS, "code"), Rc::new(vec![Item::Atomic(Atomic::QName("err".into(), err.code.clone(), ERR_NS.into()))])));
                                self.vars.push((QN::new(ERR_NS, "description"), Rc::new(vec![Item::str(err.message.clone())])));
                                self.vars.push((QN::new(ERR_NS, "value"), Rc::new(Vec::new())));
                                let r = self.eval(&c.body, f);
                                self.vars.truncate(base);
                                return r;
                            }
                        }
                        Err(err)
                    }
                }
            }
            Expr::Or(a, b) => {
                let av = self.eval(a, f)?;
                if self.ebv(&av)? {
                    return Ok(vec![Item::bool(true)]);
                }
                let bv = self.eval(b, f)?;
                Ok(vec![Item::bool(self.ebv(&bv)?)])
            }
            Expr::And(a, b) => {
                let av = self.eval(a, f)?;
                if !self.ebv(&av)? {
                    return Ok(vec![Item::bool(false)]);
                }
                let bv = self.eval(b, f)?;
                Ok(vec![Item::bool(self.ebv(&bv)?)])
            }
            Expr::GenCmp(op, a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                Ok(vec![Item::bool(self.general_compare(*op, &av, &bv)?)])
            }
            Expr::ValCmp(op, a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                let (Some(x), Some(y)) = (self.atomize_opt(&av, "an operand of a value comparison")?, self.atomize_opt(&bv, "an operand of a value comparison")?) else {
                    return Ok(Vec::new());
                };
                let x = untyped_to_string(x);
                let y = untyped_to_string(y);
                Ok(vec![Item::bool(cmp_result(*op, compare_atomic(&x, &y)?))])
            }
            Expr::NodeCmp(op, a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                if av.is_empty() || bv.is_empty() {
                    return Ok(Vec::new());
                }
                let (Some(Item::Node(x)), Some(Item::Node(y)), 1, 1) = (av.first(), bv.first(), av.len(), bv.len()) else {
                    return Err(XqError::type_err("The operands of a node comparison must be single nodes"));
                };
                Ok(vec![Item::bool(match op {
                    NodeCmp::Is => x == y,
                    NodeCmp::Before => x < y,
                    NodeCmp::After => x > y,
                })])
            }
            Expr::Concat(a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                let sa = self.atomize_opt(&av, "an operand of ||")?.map(|a| a.to_string()).unwrap_or_default();
                let sb = self.atomize_opt(&bv, "an operand of ||")?.map(|a| a.to_string()).unwrap_or_default();
                Ok(vec![Item::str(sa + &sb)])
            }
            Expr::Range(a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                let (Some(x), Some(y)) = (self.atomize_opt(&av, "a range bound")?, self.atomize_opt(&bv, "a range bound")?) else {
                    return Ok(Vec::new());
                };
                let x = to_integer(&x)?;
                let y = to_integer(&y)?;
                if y < x {
                    return Ok(Vec::new());
                }
                if y - x > 50_000_000 {
                    return Err(XqError::new("XPDY0130", "Range too large"));
                }
                Ok((x..=y).map(Item::int).collect())
            }
            Expr::Arith(op, a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                let (Some(x), Some(y)) = (self.atomize_opt(&av, "an arithmetic operand")?, self.atomize_opt(&bv, "an arithmetic operand")?) else {
                    return Ok(Vec::new());
                };
                Ok(vec![Item::Atomic(arith(*op, &x, &y)?)])
            }
            Expr::Neg(a) | Expr::Plus(a) => {
                let av = self.eval(a, f)?;
                let Some(x) = self.atomize_opt(&av, "an arithmetic operand")? else { return Ok(Vec::new()) };
                let x = if let Atomic::Untyped(_) = x { cast(&x, AtomicType::Double)? } else { x };
                if !x.is_numeric() {
                    return Err(XqError::type_err(format!("Unary minus/plus is not defined for {}", x.type_of().name())));
                }
                if matches!(e, Expr::Plus(_)) {
                    return Ok(vec![Item::Atomic(x)]);
                }
                Ok(vec![Item::Atomic(match x {
                    Atomic::Int(i) => Atomic::Int(-i),
                    Atomic::Dec(d) => Atomic::Dec(d.neg()),
                    Atomic::Flt(d) => Atomic::Flt(-d),
                    other => Atomic::Dbl(-other.as_f64()),
                })])
            }
            Expr::Set(op, a, b) => {
                let av = self.eval(a, f)?;
                let bv = self.eval(b, f)?;
                let av = self.nodes_of(av, "union/intersect/except")?;
                let bv = self.nodes_of(bv, "union/intersect/except")?;
                let mut out: Vec<XNode> = match op {
                    SetOp::Union => av.into_iter().chain(bv).collect(),
                    SetOp::Intersect => av.into_iter().filter(|n| bv.contains(n)).collect(),
                    SetOp::Except => av.into_iter().filter(|n| !bv.contains(n)).collect(),
                };
                sort_nodes(&mut out);
                Ok(out.into_iter().map(Item::Node).collect())
            }
            Expr::InstanceOf(a, t) => {
                let v = self.eval(a, f)?;
                Ok(vec![Item::bool(self.seq_matches(&v, t))])
            }
            Expr::Treat(a, t) => {
                let v = self.eval(a, f)?;
                if !self.seq_matches(&v, t) {
                    return Err(XqError::new("XPDY0050", format!("The value does not match the required type {}", t.describe())));
                }
                Ok(v)
            }
            Expr::Castable(a, t, opt) => {
                let v = self.eval(a, f)?;
                let r = match self.atomize_opt(&v, "a cast operand") {
                    Ok(None) => *opt,
                    Ok(Some(x)) => cast(&x, *t).is_ok(),
                    Err(_) => false,
                };
                Ok(vec![Item::bool(r)])
            }
            Expr::Cast(a, t, opt) => {
                let v = self.eval(a, f)?;
                match self.atomize_opt(&v, "a cast operand")? {
                    None => {
                        if *opt {
                            Ok(Vec::new())
                        } else {
                            Err(XqError::type_err(format!("An empty sequence cannot be cast to {}", t.name())))
                        }
                    }
                    Some(x) => Ok(vec![Item::Atomic(cast(&x, *t)?)]),
                }
            }
            Expr::Path { start, steps } => self.path(start, steps, f),
            Expr::Filter(p, preds) => {
                let v = self.eval(p, f)?;
                self.apply_preds(v, preds)
            }
            Expr::SimpleMap(a, b) => {
                let av = self.eval(a, f)?;
                let size = av.len();
                let mut out = Vec::new();
                for (i, it) in av.into_iter().enumerate() {
                    out.extend(self.eval(b, &Focus::of(it, i + 1, size))?);
                }
                Ok(out)
            }
            Expr::Call { name, args } => self.call(name, args, f),
            Expr::Elem { name, ns, attrs, content } => self.construct_element(name, ns, attrs, content, f),
            Expr::Attr { name, value } => {
                let q = self.ctor_qname(name, f, false)?;
                let v = self.eval(value, f)?;
                let text = self.atomize(&v).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
                let d = new_root(NodeType::Attribute, q, text);
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::TextLit(s) => {
                let d = new_root(NodeType::Text, QName::default(), s.clone());
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::Text(v) => {
                let s = self.eval(v, f)?;
                if s.is_empty() {
                    return Ok(Vec::new());
                }
                let text = self.atomize(&s).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
                let d = new_root(NodeType::Text, QName::default(), text);
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::Comment(v) => {
                let s = self.eval(v, f)?;
                let text = self.atomize(&s).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
                if text.contains("--") || text.ends_with('-') {
                    return Err(XqError::new("XQDY0072", "A comment must not contain '--' or end with '-'"));
                }
                let d = new_root(NodeType::Comment, QName::default(), text);
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::Pi { target, value } => {
                let q = self.ctor_qname(target, f, false)?;
                let s = self.eval(value, f)?;
                let text = self.atomize(&s).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
                if text.contains("?>") {
                    return Err(XqError::new("XQDY0026", "A processing instruction must not contain '?>'"));
                }
                let d = new_root(NodeType::ProcessingInstruction, QName::local(q.local), text.trim_start().to_string());
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::Document(v) => {
                let s = self.eval(v, f)?;
                let mut d = Document::new();
                self.add_content(&mut d, 0, s, false)?;
                Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
            }
            Expr::MapCtor(..) | Expr::ArrayCtor { .. } | Expr::InlineFunc { .. } | Expr::FuncRef(..) | Expr::DynCall(..) | Expr::PartialCall { .. } | Expr::Lookup(..) => self.eval31(e, f),
            Expr::Insert { .. } | Expr::Delete(_) | Expr::Replace { .. } | Expr::Rename { .. } | Expr::Transform { .. } => self.eval_update(e, f),
        }
    }

    fn with_binding(&mut self, var: Option<&QN>, v: Seq, body: impl FnOnce(&mut Self) -> R<Seq>) -> R<Seq> {
        let base = self.vars.len();
        if let Some(q) = var {
            self.vars.push((q.clone(), Rc::new(v)));
        }
        let r = body(self);
        self.vars.truncate(base);
        r
    }

    fn nodes_of(&self, s: Seq, what: &str) -> R<Vec<XNode>> {
        s.into_iter()
            .map(|i| match i {
                Item::Node(n) => Ok(n),
                Item::Atomic(a) => Err(XqError::type_err(format!("The operands of {} must be nodes, found {}", what, a.type_of().name()))),
                _ => Err(XqError::type_err(format!("The operands of {} must be nodes, found a function, map or array", what))),
            })
            .collect()
    }

    fn quantified(&mut self, every: bool, binds: &[(QN, Expr)], sat: &Expr, f: &Focus) -> R<bool> {
        let Some(((var, expr), rest)) = binds.split_first() else {
            let v = self.eval(sat, f)?;
            return self.ebv(&v);
        };
        let s = self.eval(expr, f)?;
        for it in s {
            let base = self.vars.len();
            self.vars.push((var.clone(), Rc::new(vec![it])));
            let r = self.quantified(every, rest, sat, f);
            self.vars.truncate(base);
            let r = r?;
            if every && !r {
                return Ok(false);
            }
            if !every && r {
                return Ok(true);
            }
        }
        Ok(every)
    }

    // ---------------------------------------------------------------- FLWOR

    fn eval_in(&mut self, tuple: &[(QN, Rc<Seq>)], e: &Expr, f: &Focus) -> R<Seq> {
        let base = self.vars.len();
        self.vars.extend(tuple.iter().cloned());
        let r = self.eval(e, f);
        self.vars.truncate(base);
        r
    }

    fn flwor(&mut self, clauses: &[Clause], ret: &Expr, f: &Focus) -> R<Seq> {
        type Tuple = Vec<(QN, Rc<Seq>)>;
        let mut tuples: Vec<Tuple> = vec![Vec::new()];
        for c in clauses {
            match c {
                Clause::For { var, ty, pos, expr } => {
                    let mut next = Vec::new();
                    for t in tuples {
                        let s = self.eval_in(&t, expr, f)?;
                        for (i, it) in s.into_iter().enumerate() {
                            if let Some(ty) = ty {
                                if !self.item_matches(&it, &ty.item) {
                                    return Err(XqError::type_err(format!("Value bound to ${} does not match {}", var.local, ty.describe())));
                                }
                            }
                            let mut nt = t.clone();
                            nt.push((var.clone(), Rc::new(vec![it])));
                            if let Some(p) = pos {
                                nt.push((p.clone(), Rc::new(vec![Item::int(i as i64 + 1)])));
                            }
                            next.push(nt);
                        }
                    }
                    tuples = next;
                }
                Clause::Let { var, ty, expr } => {
                    for t in tuples.iter_mut() {
                        let mut s = self.eval_in(t, expr, f)?;
                        if let Some(ty) = ty {
                            s = self.convert_to(s, ty, &format!("${}", var.local))?;
                        }
                        t.push((var.clone(), Rc::new(s)));
                    }
                }
                Clause::Where(e) => {
                    let mut next = Vec::new();
                    for t in tuples {
                        let v = self.eval_in(&t, e, f)?;
                        if self.ebv(&v)? {
                            next.push(t);
                        }
                    }
                    tuples = next;
                }
                Clause::Count(var) => {
                    for (i, t) in tuples.iter_mut().enumerate() {
                        t.push((var.clone(), Rc::new(vec![Item::int(i as i64 + 1)])));
                    }
                }
                Clause::OrderBy { specs } => {
                    let mut keyed: Vec<(Vec<Option<Atomic>>, Tuple)> = Vec::with_capacity(tuples.len());
                    for t in tuples {
                        let mut keys = Vec::new();
                        for s in specs {
                            let v = self.eval_in(&t, &s.expr, f)?;
                            let k = self.atomize_opt(&v, "an order by key")?.map(|a| if let Atomic::Untyped(s) = a { Atomic::Str(s) } else { a });
                            keys.push(k);
                        }
                        keyed.push((keys, t));
                    }
                    let mut err = None;
                    keyed.sort_by(|a, b| {
                        for (i, s) in specs.iter().enumerate() {
                            let o = match order_keys(&a.0[i], &b.0[i], s.empty_greatest) {
                                Ok(o) => o,
                                Err(e) => {
                                    err.get_or_insert(e);
                                    Ordering::Equal
                                }
                            };
                            let o = if s.desc { o.reverse() } else { o };
                            if o != Ordering::Equal {
                                return o;
                            }
                        }
                        Ordering::Equal
                    });
                    if let Some(e) = err {
                        return Err(e);
                    }
                    tuples = keyed.into_iter().map(|(_, t)| t).collect();
                }
                Clause::GroupBy(keys) => {
                    let mut groups: Vec<(Vec<Option<Atomic>>, Vec<Tuple>)> = Vec::new();
                    for mut t in tuples {
                        let mut kv = Vec::new();
                        for (var, e) in keys {
                            let v = match e {
                                Some(e) => {
                                    let v = self.eval_in(&t, e, f)?;
                                    t.push((var.clone(), Rc::new(v.clone())));
                                    v
                                }
                                None => t.iter().rev().find(|(n, _)| n == var).map(|(_, v)| (**v).clone()).ok_or_else(|| XqError::static_err("XQST0094", format!("Grouping variable ${} is not bound", var.local)))?,
                            };
                            kv.push(self.atomize_opt(&v, "a grouping key")?.map(untyped_to_string));
                        }
                        let found = groups.iter_mut().find(|(k, _)| k.len() == kv.len() && k.iter().zip(&kv).all(|(a, b)| keys_equal(a, b)));
                        match found {
                            Some((_, members)) => members.push(t),
                            None => groups.push((kv, vec![t])),
                        }
                    }
                    let key_names: Vec<&QN> = keys.iter().map(|(v, _)| v).collect();
                    tuples = groups
                        .into_iter()
                        .map(|(kv, members)| {
                            let mut nt: Tuple = Vec::new();
                            // non-grouping variables: concatenation over the group
                            let mut names: Vec<QN> = Vec::new();
                            for (n, _) in &members[0] {
                                if !key_names.contains(&n) && !names.contains(n) {
                                    names.push(n.clone());
                                }
                            }
                            for n in names {
                                let mut all = Vec::new();
                                for m in &members {
                                    if let Some((_, v)) = m.iter().rev().find(|(x, _)| *x == n) {
                                        all.extend((**v).clone());
                                    }
                                }
                                nt.push((n, Rc::new(all)));
                            }
                            for (i, k) in kv.into_iter().enumerate() {
                                nt.push((key_names[i].clone(), Rc::new(k.into_iter().map(Item::Atomic).collect())));
                            }
                            nt
                        })
                        .collect();
                }
            }
        }
        let mut out = Vec::new();
        for t in tuples {
            out.extend(self.eval_in(&t, ret, f)?);
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- comparisons

    pub fn general_compare(&self, op: Cmp, a: &[Item], b: &[Item]) -> R<bool> {
        let av = self.atomize(a);
        let bv = self.atomize(b);
        for x in &av {
            for y in &bv {
                let (x2, y2) = match (x, y) {
                    (Atomic::Untyped(_), Atomic::Untyped(_)) => (untyped_to_string(x.clone()), untyped_to_string(y.clone())),
                    (Atomic::Untyped(_), n) if n.is_numeric() => (cast(x, AtomicType::Double)?, y.clone()),
                    (n, Atomic::Untyped(_)) if n.is_numeric() => (x.clone(), cast(y, AtomicType::Double)?),
                    (Atomic::Untyped(_), other) => (cast(x, other.type_of())?, y.clone()),
                    (other, Atomic::Untyped(_)) => (x.clone(), cast(y, other.type_of())?),
                    _ => (x.clone(), y.clone()),
                };
                if cmp_result(op, compare_atomic(&x2, &y2)?) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    // ---------------------------------------------------------------- paths

    fn root_of(&self, f: &Focus) -> R<XNode> {
        match self.context_item(f)? {
            Item::Node(n) => {
                let doc = self.docs.get(n.doc);
                Ok(XNode::new(n.doc, doc.tree_root(n.node_id())))
            }
            _ => Err(XqError::type_err("The context item for '/' is not a node")),
        }
    }

    fn step_test(&self, n: XNode, test: &StepTest, axis: Axis) -> bool {
        match test {
            StepTest::Name(nt) => node_test_matches(self.docs, n, nt, axis),
            StepTest::LocalWildcard(l) => {
                let principal = if axis == Axis::Attribute { NodeType::Attribute } else { NodeType::Element };
                self.node_type(n) == Some(principal) && self.docs.node_name(n).1 == *l
            }
            StepTest::Kind(t) => self.item_matches(&Item::Node(n), &t.item),
        }
    }

    fn axis_step(&mut self, ctx: &[Item], axis: Axis, test: &StepTest, preds: &[Expr]) -> R<Seq> {
        let mut out: Vec<XNode> = Vec::new();
        for it in ctx {
            let Item::Node(n) = it else {
                return Err(XqError::type_err("An axis step requires the context item to be a node"));
            };
            if matches!(axis, Axis::Following | Axis::Preceding) && self.node_type(*n) == Some(NodeType::Attribute) && self.docs.get(n.doc).parent(n.node_id()).is_none() {
                continue;
            }
            let cands: Vec<XNode> = axis_nodes(self.docs, *n, axis).into_iter().filter(|c| self.step_test(*c, test, axis)).collect();
            if preds.is_empty() {
                out.extend(cands);
            } else {
                let items: Seq = cands.into_iter().map(Item::Node).collect();
                for i in self.apply_preds(items, preds)? {
                    if let Item::Node(x) = i {
                        out.push(x);
                    }
                }
            }
        }
        if ctx.len() > 1 || axis.is_reverse() {
            sort_nodes(&mut out);
        }
        Ok(out.into_iter().map(Item::Node).collect())
    }

    fn path(&mut self, start: &PathStart, steps: &[(bool, Step)], f: &Focus) -> R<Seq> {
        let mut cur: Option<Seq> = match start {
            PathStart::Root => Some(vec![Item::Node(self.root_of(f)?)]),
            PathStart::RootDesc => {
                let r = self.root_of(f)?;
                Some(vec![Item::Node(r)])
            }
            PathStart::Relative => None,
        };
        for (k, (desc, step)) in steps.iter().enumerate() {
            let desc = *desc || (k == 0 && *start == PathStart::RootDesc);
            let ctx: Seq = match cur.take() {
                Some(c) => c,
                None => {
                    // first step of a relative path: evaluated against the focus
                    match step {
                        Step::Filter(e) => {
                            cur = Some(self.eval(e, f)?);
                            continue;
                        }
                        Step::Axis { .. } => vec![self.context_item(f)?],
                    }
                }
            };
            let next = match step {
                Step::Axis { axis: Axis::Child, test, preds } if desc && preds.is_empty() => self.axis_step(&ctx, Axis::Descendant, test, preds)?,
                Step::Axis { axis, test, preds } => {
                    let ctx = if desc { self.axis_step(&ctx, Axis::DescendantOrSelf, &StepTest::Name(NodeTest::Node), &[])? } else { ctx };
                    self.axis_step(&ctx, *axis, test, preds)?
                }
                Step::Filter(e) => {
                    let ctx = if desc { self.axis_step(&ctx, Axis::DescendantOrSelf, &StepTest::Name(NodeTest::Node), &[])? } else { ctx };
                    let size = ctx.len();
                    let mut out = Vec::new();
                    for (i, it) in ctx.into_iter().enumerate() {
                        if !matches!(it, Item::Node(_)) {
                            return Err(XqError::type_err("The context item of a path step is not a node"));
                        }
                        out.extend(self.eval(e, &Focus::of(it, i + 1, size))?);
                    }
                    let all_nodes = out.iter().all(|i| matches!(i, Item::Node(_)));
                    if all_nodes {
                        let mut ns: Vec<XNode> = out.into_iter().filter_map(|i| if let Item::Node(n) = i { Some(n) } else { None }).collect();
                        sort_nodes(&mut ns);
                        ns.into_iter().map(Item::Node).collect()
                    } else if out.iter().all(|i| !matches!(i, Item::Node(_))) {
                        if k + 1 < steps.len() {
                            return Err(XqError::type_err("Atomic values can only be returned by the last step of a path"));
                        }
                        out
                    } else {
                        return Err(XqError::type_err("A path step returned a mix of nodes and atomic values"));
                    }
                }
            };
            cur = Some(next);
        }
        Ok(cur.unwrap_or_default())
    }

    pub fn apply_preds(&mut self, items: Seq, preds: &[Expr]) -> R<Seq> {
        let mut cur = items;
        for p in preds {
            if let Expr::Lit(Atomic::Int(n)) = p {
                let n = *n;
                cur = if n >= 1 && (n as usize) <= cur.len() { vec![cur.swap_remove(n as usize - 1)] } else { Vec::new() };
                continue;
            }
            let size = cur.len();
            let mut next = Vec::new();
            for (i, it) in cur.into_iter().enumerate() {
                let v = self.eval(p, &Focus::of(it.clone(), i + 1, size))?;
                let keep = match v.as_slice() {
                    [Item::Atomic(a)] if a.is_numeric() => a.as_f64() == (i + 1) as f64,
                    _ => self.ebv(&v)?,
                };
                if keep {
                    next.push(it);
                }
            }
            cur = next;
        }
        Ok(cur)
    }

    // ---------------------------------------------------------------- constructors

    fn ctor_qname(&mut self, name: &CtorName, f: &Focus, elem: bool) -> R<QName> {
        match name {
            CtorName::Static(q, p) => Ok(QName::ns(p.clone(), q.local.clone(), q.uri.clone())),
            CtorName::Computed(e) => {
                let v = self.eval(e, f)?;
                let a = self.atomize_opt(&v, "a constructor name")?.ok_or_else(|| XqError::type_err("The name of a computed constructor is an empty sequence"))?;
                if let Atomic::QName(p, l, u) = a {
                    return Ok(QName::ns(p, l, u));
                }
                let s = a.to_string();
                let s = s.trim();
                let (p, l) = s.split_once(':').unwrap_or(("", s));
                if l.is_empty() || !l.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') || l.contains(|c: char| c.is_whitespace()) {
                    return Err(XqError::new("XQDY0074", format!("'{}' is not a valid name", s)));
                }
                let uri = if p.is_empty() {
                    if elem {
                        self.module.namespaces.iter().rev().find(|(q, _)| q.is_empty()).map(|(_, u)| u.clone()).unwrap_or_default()
                    } else {
                        String::new()
                    }
                } else {
                    self.module
                        .namespaces
                        .iter()
                        .rev()
                        .find(|(q, _)| q == p)
                        .map(|(_, u)| u.clone())
                        .ok_or_else(|| XqError::new("XQDY0074", format!("Namespace prefix '{}' is not declared", p)))?
                };
                Ok(QName::ns(p, l, uri))
            }
        }
    }

    fn construct_element(&mut self, name: &CtorName, ns: &[(String, String)], attrs: &[(QN, String, Vec<AttrPart>)], content: &[Expr], f: &Focus) -> R<Seq> {
        let q = self.ctor_qname(name, f, true)?;
        let mut d = new_root(NodeType::Element, q.clone(), String::new());
        for (p, u) in ns {
            d.declare_namespace(0, p, u);
        }
        if !q.uri.is_empty() || !q.prefix.is_empty() {
            d.declare_namespace(0, &q.prefix, &q.uri);
        } else if ns.iter().any(|(p, u)| p.is_empty() && !u.is_empty()) {
            d.declare_namespace(0, "", "");
        }
        for (aq, prefix, parts) in attrs {
            let mut v = String::new();
            for p in parts {
                match p {
                    AttrPart::Lit(s) => v.push_str(s),
                    AttrPart::Expr(e) => {
                        let s = self.eval(e, f)?;
                        v.push_str(&self.atomize(&s).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" "));
                    }
                }
            }
            if !prefix.is_empty() {
                d.declare_namespace(0, prefix, &aq.uri);
            }
            d.set_attribute(0, QName::ns(prefix.clone(), aq.local.clone(), aq.uri.clone()), v);
        }
        for c in content {
            if let Expr::TextLit(s) = c {
                d.append_text(0, s);
                continue;
            }
            let s = self.eval(c, f)?;
            self.add_content(&mut d, 0, s, true)?;
        }
        Ok(vec![Item::Node(XNode::new(self.add_doc(d), 0))])
    }

    /// Add constructor content under `parent` of `d` (adjacent atomics joined with spaces).
    fn add_content(&mut self, d: &mut Document, parent: usize, s: Seq, allow_attrs: bool) -> R<()> {
        let mut atoms: Vec<String> = Vec::new();
        let flush = |d: &mut Document, atoms: &mut Vec<String>| {
            if !atoms.is_empty() {
                d.append_text(parent, &atoms.join(" "));
                atoms.clear();
            }
        };
        for it in s {
            match it {
                Item::Atomic(a) => atoms.push(a.to_string()),
                Item::Array(_) => {
                    for a in self.atomize(std::slice::from_ref(&it)) {
                        atoms.push(a.to_string());
                    }
                }
                Item::Map(_) | Item::Func(_) => return Err(XqError::new("XQTY0105", "A function or map item cannot be used as element content")),
                Item::Node(n) => {
                    flush(d, &mut atoms);
                    if n.is_namespace() {
                        continue;
                    }
                    let src = self.docs.get(n.doc);
                    match src.node_type(n.node_id()) {
                        NodeType::Attribute => {
                            if !allow_attrs {
                                return Err(XqError::new("XPTY0004", "An attribute node cannot be added to a document node"));
                            }
                            if !d.children(parent).is_empty() {
                                return Err(XqError::new("XQTY0024", "An attribute node cannot follow non-attribute content of an element constructor"));
                            }
                            let an = src.name(n.node_id()).clone();
                            if !an.prefix.is_empty() {
                                d.declare_namespace(parent, &an.prefix, &an.uri);
                            }
                            d.set_attribute(parent, an, src.value(n.node_id()).to_string());
                        }
                        _ => {
                            d.import_node(&src, n.node_id(), parent);
                        }
                    }
                }
            }
        }
        flush(d, &mut atoms);
        Ok(())
    }

    // ---------------------------------------------------------------- functions

    /// Call a user-defined function with argument values.
    pub(crate) fn call_user(&mut self, func: &Function, vals: Vec<Seq>) -> R<Seq> {
        let mut conv = Vec::with_capacity(vals.len());
        for (i, v) in vals.into_iter().enumerate() {
            conv.push(match &func.params[i].1 {
                Some(t) => self.convert_to(v, t, &format!("parameter ${} of {}()", func.params[i].0.local, func.name.local))?,
                None => v,
            });
        }
        self.invoke_user(func, conv)
    }

    pub(crate) fn call(&mut self, name: &QN, args: &[Expr], f: &Focus) -> R<Seq> {
        if let Some(r) = self.call_hof(name, args, f) {
            return r;
        }
        if let Some(func) = self.module.functions.get(&(name.clone(), args.len())) {
            let mut vals = Vec::with_capacity(args.len());
            for a in args.iter() {
                vals.push(self.eval(a, f)?);
            }
            return self.call_user(func, vals);
        }
        self.call_static(name, args, f)
    }

    fn invoke_user(&mut self, func: &Function, vals: Vec<Seq>) -> R<Seq> {
        {
            if self.depth >= MAX_DEPTH {
                return Err(XqError::new("XPDY0130", format!("Too many nested function calls (> {}) in {}()", MAX_DEPTH, func.name.local)));
            }
            self.depth += 1;
            let saved = std::mem::take(&mut self.vars);
            for (i, v) in vals.into_iter().enumerate() {
                self.vars.push((func.params[i].0.clone(), Rc::new(v)));
            }
            let r = self.eval(&func.body, &Focus::none());
            self.vars = saved;
            self.depth -= 1;
            let r = r?;
            match &func.ret {
                Some(t) => self.convert_to(r, t, &format!("the result of {}()", func.name.local)),
                None => Ok(r),
            }
        }
    }

    fn call_static(&mut self, name: &QN, args: &[Expr], f: &Focus) -> R<Seq> {
        if (name.uri == super::XS_NS && name.local == "QName" && args.len() == 1) || (name.uri == super::FN_NS && name.local == "QName" && args.len() == 2) {
            let mut vals = Vec::new();
            for a in args {
                let v = self.eval(a, f)?;
                vals.push(self.atomize_opt(&v, "a QName argument")?.map(|a| a.to_string()).unwrap_or_default());
            }
            let lex = vals.last().cloned().unwrap_or_default();
            let (p, l) = lex.trim().split_once(':').map(|(p, l)| (p.to_string(), l.to_string())).unwrap_or((String::new(), lex.trim().to_string()));
            let uri = if vals.len() == 2 {
                vals[0].clone()
            } else if p.is_empty() {
                String::new()
            } else {
                self.module.namespaces.iter().rev().find(|(q, _)| *q == p).map(|(_, u)| u.clone()).ok_or_else(|| XqError::new("FONS0004", format!("Namespace prefix '{}' is not declared", p)))?
            };
            return Ok(vec![Item::Atomic(Atomic::QName(p, l, uri))]);
        }
        if name.uri == super::XS_NS {
            if args.len() != 1 {
                return Err(XqError::static_err("XPST0017", format!("Constructor function xs:{} takes one argument", name.local)));
            }
            let t = AtomicType::from_local(&name.local).ok_or_else(|| XqError::static_err("XPST0017", format!("Unknown constructor function xs:{}", name.local)))?;
            let v = self.eval(&args[0], f)?;
            return match self.atomize_opt(&v, "a constructor function argument")? {
                None => Ok(Vec::new()),
                Some(a) => Ok(vec![Item::Atomic(cast(&a, t)?)]),
            };
        }
        super::funcs::call_builtin(self, name, args, f)
    }
}

/// Untyped atomic values compare as strings in value comparisons.
pub(crate) fn untyped_to_string(a: Atomic) -> Atomic {
    match a {
        Atomic::Untyped(s) => Atomic::Str(s),
        other => other,
    }
}

fn cmp_result(op: Cmp, o: Option<Ordering>) -> bool {
    match o {
        None => op == Cmp::Ne,
        Some(o) => match op {
            Cmp::Eq => o == Ordering::Equal,
            Cmp::Ne => o != Ordering::Equal,
            Cmp::Lt => o == Ordering::Less,
            Cmp::Le => o != Ordering::Greater,
            Cmp::Gt => o == Ordering::Greater,
            Cmp::Ge => o != Ordering::Less,
        },
    }
}

fn keys_equal(a: &Option<Atomic>, b: &Option<Atomic>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            if x.is_numeric() && y.is_numeric() && x.as_f64().is_nan() && y.as_f64().is_nan() {
                return true;
            }
            matches!(compare_atomic(x, y), Ok(Some(Ordering::Equal)))
        }
        _ => false,
    }
}

fn order_keys(a: &Option<Atomic>, b: &Option<Atomic>, empty_greatest: bool) -> R<Ordering> {
    let rank = |x: &Option<Atomic>| -> i32 {
        match x {
            None => {
                if empty_greatest {
                    2
                } else {
                    -2
                }
            }
            Some(v) if v.is_numeric() && v.as_f64().is_nan() => {
                if empty_greatest {
                    1
                } else {
                    -1
                }
            }
            _ => 0,
        }
    };
    let (ra, rb) = (rank(a), rank(b));
    if ra != 0 || rb != 0 {
        return Ok(ra.cmp(&rb));
    }
    let (x, y) = (a.as_ref().unwrap(), b.as_ref().unwrap());
    Ok(compare_atomic(x, y)?.unwrap_or(Ordering::Equal))
}

pub(crate) fn to_integer(a: &Atomic) -> R<i64> {
    match a {
        Atomic::Int(i) => Ok(*i),
        Atomic::Untyped(_) => match cast(a, AtomicType::Integer)? {
            Atomic::Int(i) => Ok(i),
            _ => unreachable!(),
        },
        Atomic::Dec(d) if d.is_integer() => d.to_i64().ok_or_else(|| XqError::new("FOAR0002", "Integer overflow")),
        Atomic::Dbl(d) | Atomic::Flt(d) if d.fract() == 0.0 => Ok(*d as i64),
        other => Err(XqError::type_err(format!("Expected an xs:integer, found {}", other.type_of().name()))),
    }
}

/// Numeric arithmetic with XQuery type promotion.
pub(crate) fn arith(op: Arith, x: &Atomic, y: &Atomic) -> R<Atomic> {
    let opc = match op {
        Arith::Add => '+',
        Arith::Sub => '-',
        Arith::Mul => '*',
        Arith::Div => '/',
        Arith::IDiv => 'i',
        Arith::Mod => '%',
    };
    if let Some(r) = super::datetime::arith(opc, x, y) {
        return r;
    }
    let promote = |a: &Atomic| -> R<Atomic> {
        match a {
            Atomic::Untyped(_) => cast(a, AtomicType::Double),
            n if n.is_numeric() => Ok(n.clone()),
            other => Err(XqError::type_err(format!("Arithmetic is not defined for {}", other.type_of().name()))),
        }
    };
    let x = promote(x)?;
    let y = promote(y)?;
    // rank: 0 int, 1 decimal, 2 float, 3 double
    let rank = |a: &Atomic| match a {
        Atomic::Int(_) => 0,
        Atomic::Dec(_) => 1,
        Atomic::Flt(_) => 2,
        _ => 3,
    };
    let r = rank(&x).max(rank(&y));
    if r == 0 {
        let (Atomic::Int(a), Atomic::Int(b)) = (&x, &y) else { unreachable!() };
        let (a, b) = (*a, *b);
        let overflow = || XqError::new("FOAR0002", "Integer overflow");
        return Ok(match op {
            Arith::Add => Atomic::Int(a.checked_add(b).ok_or_else(overflow)?),
            Arith::Sub => Atomic::Int(a.checked_sub(b).ok_or_else(overflow)?),
            Arith::Mul => Atomic::Int(a.checked_mul(b).ok_or_else(overflow)?),
            Arith::Div => {
                if b == 0 {
                    return Err(XqError::new("FOAR0001", "Division by zero"));
                }
                Atomic::Dec(Decimal::from_i64(a).div(&Decimal::from_i64(b)).unwrap_or_default())
            }
            Arith::IDiv => {
                if b == 0 {
                    return Err(XqError::new("FOAR0001", "Integer division by zero"));
                }
                Atomic::Int(a.checked_div(b).ok_or_else(overflow)?)
            }
            Arith::Mod => {
                if b == 0 {
                    return Err(XqError::new("FOAR0001", "Modulus by zero"));
                }
                Atomic::Int(a.checked_rem(b).unwrap_or(0))
            }
        });
    }
    if r == 1 {
        let (a, b) = (x.as_decimal().unwrap_or_default(), y.as_decimal().unwrap_or_default());
        let zero = || XqError::new("FOAR0001", "Division by zero");
        return Ok(match op {
            Arith::Add => Atomic::Dec(a.add(&b)),
            Arith::Sub => Atomic::Dec(a.sub(&b)),
            Arith::Mul => Atomic::Dec(a.mul(&b)),
            Arith::Div => Atomic::Dec(a.div(&b).ok_or_else(zero)?),
            Arith::Mod => Atomic::Dec(a.rem(&b).ok_or_else(zero)?),
            Arith::IDiv => Atomic::Int(a.idiv(&b).ok_or_else(zero)?.to_i64().ok_or_else(|| XqError::new("FOAR0002", "Integer overflow"))?),
        });
    }
    let (a, b) = (x.as_f64(), y.as_f64());
    if op == Arith::IDiv {
        if b == 0.0 {
            return Err(XqError::new("FOAR0001", "Integer division by zero"));
        }
        let q = a / b;
        if q.is_nan() || q.is_infinite() {
            return Err(XqError::new("FOAR0002", "Invalid idiv operands"));
        }
        return Ok(Atomic::Int(q.trunc() as i64));
    }
    let v = match op {
        Arith::Add => a + b,
        Arith::Sub => a - b,
        Arith::Mul => a * b,
        Arith::Div => a / b,
        Arith::Mod => a % b,
        Arith::IDiv => unreachable!(),
    };
    Ok(match r {
        2 => Atomic::Flt(v),
        _ => Atomic::Dbl(v),
    })
}
