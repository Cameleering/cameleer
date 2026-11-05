//! The `xpath` language: an XPath 1.0 subset evaluated over the [`super::xml`] DOM.
//!
//! Supported: absolute/relative location paths, `//`, `.`, `..`, `*`, `@attr`, `@*`,
//! node tests `text()`, `node()`, `comment()`, axes (`child`, `descendant`,
//! `descendant-or-self`, `parent`, `ancestor`, `ancestor-or-self`, `following-sibling`,
//! `preceding-sibling`, `attribute`, `self`), predicates (`[n]`, `[last()]`, `[@a='v']`,
//! `[name='v']`, `[contains(., 'x')]`, ...), unions `|`, `and`/`or`, comparisons
//! (`= != < <= > >=`), arithmetic (`+ - * div mod`), variables are not supported.
//! Functions: `last position count name local-name namespace-uri string concat starts-with
//! ends-with contains substring substring-before substring-after string-length
//! normalize-space translate upper-case lower-case boolean not true false number sum floor
//! ceiling round`.
//!
//! Namespaces are ignored: name tests compare local names.
//!
//! Language options: `resultType` (`String`, `Boolean`, `Number`/`Double`, `Integer`/`Long`,
//! `NodeList`/`NodeSet`, `Node`), `headerName`, `source`.
//! Default result: a node-set with exactly one node becomes a string (elements serialized
//! as XML, other nodes as their string value); otherwise an array of such strings.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::xml::{local_name, Document, NodeKind};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Expression, Language, Predicate};
use crate::value::Value;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Slash,
    DSlash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    At,
    Comma,
    Pipe,
    Dot,
    DDot,
    Axis(String),
    Op(String),
    Name(String),
    NodeType(String),
    Func(String),
    Str(String),
    Num(f64),
    Var(String),
}

fn xerr(expr: &str, msg: &str) -> CamelError {
    CamelError::new(crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid xpath: {}. Reason: {}", expr, msg))
}

fn tokenize(expr: &str) -> Result<Vec<Tok>> {
    let c: Vec<char> = expr.chars().collect();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    // whether the previous token allows an operator interpretation of `*` / names
    let op_context = |toks: &Vec<Tok>| -> bool {
        match toks.last() {
            None => false,
            Some(Tok::At) | Some(Tok::Axis(_)) | Some(Tok::LParen) | Some(Tok::LBracket) | Some(Tok::Comma) | Some(Tok::Op(_)) | Some(Tok::Slash)
            | Some(Tok::DSlash) | Some(Tok::Pipe) => false,
            _ => true,
        }
    };
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        match ch {
            '/' => {
                if i + 1 < c.len() && c[i + 1] == '/' {
                    toks.push(Tok::DSlash);
                    i += 2;
                } else {
                    toks.push(Tok::Slash);
                    i += 1;
                }
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            '[' => {
                toks.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                toks.push(Tok::RBracket);
                i += 1;
            }
            '@' => {
                toks.push(Tok::At);
                i += 1;
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            '|' => {
                toks.push(Tok::Pipe);
                i += 1;
            }
            '.' if i + 1 < c.len() && c[i + 1] == '.' => {
                toks.push(Tok::DDot);
                i += 2;
            }
            '.' if !(i + 1 < c.len() && c[i + 1].is_ascii_digit()) => {
                toks.push(Tok::Dot);
                i += 1;
            }
            '\'' | '"' => {
                let q = ch;
                let st = i + 1;
                let mut j = st;
                while j < c.len() && c[j] != q {
                    j += 1;
                }
                if j >= c.len() {
                    return Err(xerr(expr, "unterminated string literal"));
                }
                toks.push(Tok::Str(c[st..j].iter().collect()));
                i = j + 1;
            }
            '=' => {
                toks.push(Tok::Op("=".into()));
                i += 1;
            }
            '!' if i + 1 < c.len() && c[i + 1] == '=' => {
                toks.push(Tok::Op("!=".into()));
                i += 2;
            }
            '<' | '>' => {
                if i + 1 < c.len() && c[i + 1] == '=' {
                    toks.push(Tok::Op(format!("{}=", ch)));
                    i += 2;
                } else {
                    toks.push(Tok::Op(ch.to_string()));
                    i += 1;
                }
            }
            '+' | '-' => {
                toks.push(Tok::Op(ch.to_string()));
                i += 1;
            }
            '*' => {
                if op_context(&toks) {
                    toks.push(Tok::Op("*".into()));
                } else {
                    toks.push(Tok::Name("*".into()));
                }
                i += 1;
            }
            '$' => {
                let st = i + 1;
                let mut j = st;
                while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '-' || c[j] == '.') {
                    j += 1;
                }
                toks.push(Tok::Var(c[st..j].iter().collect()));
                i = j;
            }
            ch if ch.is_ascii_digit() || ch == '.' => {
                let st = i;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                    i += 1;
                }
                let t: String = c[st..i].iter().collect();
                toks.push(Tok::Num(t.parse().map_err(|_| xerr(expr, "invalid number"))?));
            }
            ch if ch.is_alphabetic() || ch == '_' => {
                let st = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '-' || c[i] == '.') {
                    i += 1;
                }
                // prefix:name or prefix:*
                if i + 1 < c.len() && c[i] == ':' && c[i + 1] != ':' {
                    i += 1;
                    if i < c.len() && c[i] == '*' {
                        i += 1;
                    } else {
                        while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '-' || c[i] == '.') {
                            i += 1;
                        }
                    }
                }
                let name: String = c[st..i].iter().collect();
                if op_context(&toks) && matches!(name.as_str(), "and" | "or" | "div" | "mod") {
                    toks.push(Tok::Op(name));
                    continue;
                }
                // lookahead
                let mut j = i;
                while j < c.len() && c[j].is_whitespace() {
                    j += 1;
                }
                if j + 1 < c.len() && c[j] == ':' && c[j + 1] == ':' {
                    toks.push(Tok::Axis(name));
                    i = j + 2;
                } else if j < c.len() && c[j] == '(' {
                    if matches!(name.as_str(), "text" | "node" | "comment" | "processing-instruction") {
                        toks.push(Tok::NodeType(name));
                    } else {
                        toks.push(Tok::Func(name));
                    }
                } else {
                    toks.push(Tok::Name(name));
                }
            }
            _ => return Err(xerr(expr, &format!("unexpected character '{}'", ch))),
        }
    }
    Ok(toks)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Child,
    Descendant,
    DescendantOrSelf,
    Parent,
    Ancestor,
    AncestorOrSelf,
    FollowingSibling,
    PrecedingSibling,
    Attribute,
    SelfAxis,
}

#[derive(Debug, Clone)]
enum NodeTest {
    Name(String),
    Any,
    Text,
    Node,
    Comment,
    Pi,
}

#[derive(Debug, Clone)]
struct Step {
    axis: Axis,
    test: NodeTest,
    preds: Vec<X>,
}

#[derive(Debug, Clone)]
enum X {
    Path { absolute: bool, steps: Vec<Step> },
    /// filter expression followed by a relative path
    Filter(Box<X>, Vec<X>, Vec<Step>),
    Or(Box<X>, Box<X>),
    And(Box<X>, Box<X>),
    Cmp(String, Box<X>, Box<X>),
    Arith(String, Box<X>, Box<X>),
    Neg(Box<X>),
    Union(Box<X>, Box<X>),
    Str(String),
    Num(f64),
    Func(String, Vec<X>),
}

struct Parser<'a> {
    t: Vec<Tok>,
    i: usize,
    expr: &'a str,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn err(&self, m: &str) -> CamelError {
        xerr(self.expr, m)
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if o == op)
    }

    fn or(&mut self) -> Result<X> {
        let mut l = self.and()?;
        while self.is_op("or") {
            self.i += 1;
            let r = self.and()?;
            l = X::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<X> {
        let mut l = self.eq()?;
        while self.is_op("and") {
            self.i += 1;
            let r = self.eq()?;
            l = X::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn eq(&mut self) -> Result<X> {
        let mut l = self.rel()?;
        while self.is_op("=") || self.is_op("!=") {
            let op = if let Some(Tok::Op(o)) = self.peek() { o.clone() } else { unreachable!() };
            self.i += 1;
            let r = self.rel()?;
            l = X::Cmp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn rel(&mut self) -> Result<X> {
        let mut l = self.add()?;
        while self.is_op("<") || self.is_op("<=") || self.is_op(">") || self.is_op(">=") {
            let op = if let Some(Tok::Op(o)) = self.peek() { o.clone() } else { unreachable!() };
            self.i += 1;
            let r = self.add()?;
            l = X::Cmp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<X> {
        let mut l = self.mul()?;
        while self.is_op("+") || self.is_op("-") {
            let op = if let Some(Tok::Op(o)) = self.peek() { o.clone() } else { unreachable!() };
            self.i += 1;
            let r = self.mul()?;
            l = X::Arith(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<X> {
        let mut l = self.unary()?;
        while self.is_op("*") || self.is_op("div") || self.is_op("mod") {
            let op = if let Some(Tok::Op(o)) = self.peek() { o.clone() } else { unreachable!() };
            self.i += 1;
            let r = self.unary()?;
            l = X::Arith(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<X> {
        if self.is_op("-") {
            self.i += 1;
            return Ok(X::Neg(Box::new(self.unary()?)));
        }
        let mut l = self.path_expr()?;
        while self.peek() == Some(&Tok::Pipe) {
            self.i += 1;
            let r = self.path_expr()?;
            l = X::Union(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn path_expr(&mut self) -> Result<X> {
        match self.peek() {
            Some(Tok::Str(_)) | Some(Tok::Num(_)) | Some(Tok::LParen) | Some(Tok::Func(_)) | Some(Tok::Var(_)) => {
                let prim = self.primary()?;
                let mut preds = Vec::new();
                while self.peek() == Some(&Tok::LBracket) {
                    preds.push(self.predicate()?);
                }
                let mut steps = Vec::new();
                loop {
                    match self.peek() {
                        Some(Tok::Slash) => {
                            self.i += 1;
                            steps.push(self.step()?);
                        }
                        Some(Tok::DSlash) => {
                            self.i += 1;
                            steps.push(Step { axis: Axis::DescendantOrSelf, test: NodeTest::Node, preds: vec![] });
                            steps.push(self.step()?);
                        }
                        _ => break,
                    }
                }
                if preds.is_empty() && steps.is_empty() {
                    Ok(prim)
                } else {
                    Ok(X::Filter(Box::new(prim), preds, steps))
                }
            }
            _ => self.location_path(),
        }
    }

    fn primary(&mut self) -> Result<X> {
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(X::Str(s))
            }
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(X::Num(n))
            }
            Some(Tok::LParen) => {
                self.i += 1;
                let e = self.or()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err(self.err("expected ')'"));
                }
                self.i += 1;
                Ok(e)
            }
            Some(Tok::Var(v)) => Err(self.err(&format!("variables are not supported (${})", v))),
            Some(Tok::Func(name)) => {
                self.i += 2; // name (
                let mut args = Vec::new();
                if self.peek() != Some(&Tok::RParen) {
                    loop {
                        args.push(self.or()?);
                        match self.peek() {
                            Some(Tok::Comma) => self.i += 1,
                            Some(Tok::RParen) => break,
                            _ => return Err(self.err("expected ',' or ')' in function call")),
                        }
                    }
                }
                self.i += 1;
                if !KNOWN_FUNCS.contains(&name.as_str()) {
                    return Err(self.err(&format!("unknown function {}()", name)));
                }
                Ok(X::Func(name, args))
            }
            _ => Err(self.err("expected primary expression")),
        }
    }

    fn predicate(&mut self) -> Result<X> {
        self.i += 1; // [
        let e = self.or()?;
        if self.peek() != Some(&Tok::RBracket) {
            return Err(self.err("expected ']'"));
        }
        self.i += 1;
        Ok(e)
    }

    fn location_path(&mut self) -> Result<X> {
        let mut steps = Vec::new();
        let absolute;
        match self.peek() {
            Some(Tok::Slash) => {
                absolute = true;
                self.i += 1;
                if self.can_start_step() {
                    steps.push(self.step()?);
                } else {
                    return Ok(X::Path { absolute, steps });
                }
            }
            Some(Tok::DSlash) => {
                absolute = true;
                self.i += 1;
                steps.push(Step { axis: Axis::DescendantOrSelf, test: NodeTest::Node, preds: vec![] });
                steps.push(self.step()?);
            }
            _ => {
                absolute = false;
                steps.push(self.step()?);
            }
        }
        loop {
            match self.peek() {
                Some(Tok::Slash) => {
                    self.i += 1;
                    steps.push(self.step()?);
                }
                Some(Tok::DSlash) => {
                    self.i += 1;
                    steps.push(Step { axis: Axis::DescendantOrSelf, test: NodeTest::Node, preds: vec![] });
                    steps.push(self.step()?);
                }
                _ => break,
            }
        }
        Ok(X::Path { absolute, steps })
    }

    fn can_start_step(&self) -> bool {
        matches!(self.peek(), Some(Tok::Name(_)) | Some(Tok::At) | Some(Tok::Dot) | Some(Tok::DDot) | Some(Tok::Axis(_)) | Some(Tok::NodeType(_)))
    }

    fn step(&mut self) -> Result<Step> {
        match self.peek() {
            Some(Tok::Dot) => {
                self.i += 1;
                return Ok(Step { axis: Axis::SelfAxis, test: NodeTest::Node, preds: vec![] });
            }
            Some(Tok::DDot) => {
                self.i += 1;
                return Ok(Step { axis: Axis::Parent, test: NodeTest::Node, preds: vec![] });
            }
            _ => {}
        }
        let mut axis = Axis::Child;
        match self.peek().cloned() {
            Some(Tok::At) => {
                self.i += 1;
                axis = Axis::Attribute;
            }
            Some(Tok::Axis(a)) => {
                self.i += 1;
                axis = match a.as_str() {
                    "child" => Axis::Child,
                    "descendant" => Axis::Descendant,
                    "descendant-or-self" => Axis::DescendantOrSelf,
                    "parent" => Axis::Parent,
                    "ancestor" => Axis::Ancestor,
                    "ancestor-or-self" => Axis::AncestorOrSelf,
                    "following-sibling" => Axis::FollowingSibling,
                    "preceding-sibling" => Axis::PrecedingSibling,
                    "attribute" => Axis::Attribute,
                    "self" => Axis::SelfAxis,
                    other => return Err(self.err(&format!("unsupported axis {}", other))),
                };
            }
            _ => {}
        }
        let test = match self.peek().cloned() {
            Some(Tok::Name(n)) => {
                self.i += 1;
                if n == "*" || n.ends_with(":*") {
                    NodeTest::Any
                } else {
                    NodeTest::Name(local_name(&n).to_string())
                }
            }
            Some(Tok::NodeType(t)) => {
                self.i += 1;
                if self.peek() != Some(&Tok::LParen) {
                    return Err(self.err("expected '('"));
                }
                self.i += 1;
                if let Some(Tok::Str(_)) = self.peek() {
                    self.i += 1;
                }
                if self.peek() != Some(&Tok::RParen) {
                    return Err(self.err("expected ')'"));
                }
                self.i += 1;
                match t.as_str() {
                    "text" => NodeTest::Text,
                    "node" => NodeTest::Node,
                    "comment" => NodeTest::Comment,
                    _ => NodeTest::Pi,
                }
            }
            _ => return Err(self.err("expected node test")),
        };
        let mut preds = Vec::new();
        while self.peek() == Some(&Tok::LBracket) {
            preds.push(self.predicate()?);
        }
        Ok(Step { axis, test, preds })
    }
}

const KNOWN_FUNCS: &[&str] = &[
    "last",
    "position",
    "count",
    "name",
    "local-name",
    "namespace-uri",
    "string",
    "concat",
    "starts-with",
    "ends-with",
    "contains",
    "substring",
    "substring-before",
    "substring-after",
    "string-length",
    "normalize-space",
    "translate",
    "upper-case",
    "lower-case",
    "boolean",
    "not",
    "true",
    "false",
    "number",
    "sum",
    "floor",
    "ceiling",
    "round",
];

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// A node reference: a DOM node or an attribute (element index, attribute index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NRef {
    Node(usize),
    Attr(usize, usize),
}

impl NRef {
    fn order(&self) -> (usize, usize) {
        match self {
            NRef::Node(n) => (*n, 0),
            NRef::Attr(n, a) => (*n, a + 1),
        }
    }
}

#[derive(Debug, Clone)]
enum XVal {
    Nodes(Vec<NRef>),
    Str(String),
    Num(f64),
    Bool(bool),
}

struct Ctx<'a> {
    doc: &'a Document,
    node: NRef,
    pos: usize,
    size: usize,
}

fn str_value(doc: &Document, n: NRef) -> String {
    match n {
        NRef::Node(i) => doc.text_content(i),
        NRef::Attr(e, a) => doc.attrs(e)[a].1.clone(),
    }
}

fn to_num_str(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(f64::NAN)
}

fn to_str(doc: &Document, v: &XVal) -> String {
    match v {
        XVal::Nodes(ns) => ns.first().map(|n| str_value(doc, *n)).unwrap_or_default(),
        XVal::Str(s) => s.clone(),
        XVal::Num(n) => fmt_num(*n),
        XVal::Bool(b) => b.to_string(),
    }
}

fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.fract() == 0.0 && n.abs() < 1e16 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn to_num(doc: &Document, v: &XVal) -> f64 {
    match v {
        XVal::Num(n) => *n,
        XVal::Bool(b) => *b as i32 as f64,
        other => to_num_str(&to_str(doc, other)),
    }
}

fn to_bool(v: &XVal) -> bool {
    match v {
        XVal::Nodes(ns) => !ns.is_empty(),
        XVal::Str(s) => !s.is_empty(),
        XVal::Num(n) => *n != 0.0 && !n.is_nan(),
        XVal::Bool(b) => *b,
    }
}

fn test_matches(doc: &Document, n: NRef, test: &NodeTest, axis: Axis) -> bool {
    match n {
        NRef::Attr(e, a) => match test {
            NodeTest::Any | NodeTest::Node => true,
            NodeTest::Name(name) => local_name(&doc.attrs(e)[a].0) == name,
            _ => false,
        },
        NRef::Node(i) => {
            let kind = &doc.nodes[i].kind;
            match test {
                NodeTest::Node => true,
                NodeTest::Any => matches!(kind, NodeKind::Element { .. }) && axis != Axis::Attribute,
                NodeTest::Name(name) => match kind {
                    NodeKind::Element { name: en, .. } => local_name(en) == name,
                    _ => false,
                },
                NodeTest::Text => matches!(kind, NodeKind::Text(_)),
                NodeTest::Comment => matches!(kind, NodeKind::Comment(_)),
                NodeTest::Pi => matches!(kind, NodeKind::ProcessingInstruction { .. }),
            }
        }
    }
}

fn descendants(doc: &Document, i: usize, out: &mut Vec<NRef>) {
    for &c in &doc.nodes[i].children {
        out.push(NRef::Node(c));
        descendants(doc, c, out);
    }
}

fn axis_nodes(doc: &Document, n: NRef, axis: Axis) -> Vec<NRef> {
    let mut out = Vec::new();
    let node_idx = match n {
        NRef::Node(i) => Some(i),
        NRef::Attr(..) => None,
    };
    let parent = match n {
        NRef::Node(i) => doc.nodes[i].parent,
        NRef::Attr(e, _) => Some(e),
    };
    match axis {
        Axis::Child => {
            if let Some(i) = node_idx {
                out.extend(doc.nodes[i].children.iter().map(|&c| NRef::Node(c)));
            }
        }
        Axis::Descendant => {
            if let Some(i) = node_idx {
                descendants(doc, i, &mut out);
            }
        }
        Axis::DescendantOrSelf => {
            out.push(n);
            if let Some(i) = node_idx {
                descendants(doc, i, &mut out);
            }
        }
        Axis::Parent => {
            if let Some(p) = parent {
                out.push(NRef::Node(p));
            }
        }
        Axis::Ancestor | Axis::AncestorOrSelf => {
            if axis == Axis::AncestorOrSelf {
                out.push(n);
            }
            let mut p = parent;
            while let Some(x) = p {
                out.push(NRef::Node(x));
                p = doc.nodes[x].parent;
            }
        }
        Axis::FollowingSibling | Axis::PrecedingSibling => {
            if let (Some(i), Some(p)) = (node_idx, parent) {
                let sibs = &doc.nodes[p].children;
                let k = sibs.iter().position(|&x| x == i).unwrap_or(0);
                if axis == Axis::FollowingSibling {
                    out.extend(sibs[k + 1..].iter().map(|&c| NRef::Node(c)));
                } else {
                    out.extend(sibs[..k].iter().rev().map(|&c| NRef::Node(c)));
                }
            }
        }
        Axis::Attribute => {
            if let Some(i) = node_idx {
                for (a, (k, _)) in doc.attrs(i).iter().enumerate() {
                    if k != "xmlns" && !k.starts_with("xmlns:") {
                        out.push(NRef::Attr(i, a));
                    }
                }
            }
        }
        Axis::SelfAxis => out.push(n),
    }
    out
}

fn sort_dedup(mut v: Vec<NRef>) -> Vec<NRef> {
    v.sort_by_key(|n| n.order());
    v.dedup();
    v
}

fn eval_steps(doc: &Document, start: Vec<NRef>, steps: &[Step]) -> Result<Vec<NRef>> {
    let mut cur = start;
    for step in steps {
        let mut next = Vec::new();
        for &n in &cur {
            let mut cands: Vec<NRef> = axis_nodes(doc, n, step.axis).into_iter().filter(|c| test_matches(doc, *c, &step.test, step.axis)).collect();
            for p in &step.preds {
                let size = cands.len();
                let mut kept = Vec::new();
                for (k, c) in cands.iter().enumerate() {
                    let ctx = Ctx { doc, node: *c, pos: k + 1, size };
                    let v = eval(p, &ctx)?;
                    let keep = match v {
                        XVal::Num(num) => num == (k + 1) as f64,
                        other => to_bool(&other),
                    };
                    if keep {
                        kept.push(*c);
                    }
                }
                cands = kept;
            }
            next.extend(cands);
        }
        cur = sort_dedup(next);
    }
    Ok(cur)
}

fn compare(doc: &Document, op: &str, a: &XVal, b: &XVal) -> bool {
    let cmp_atoms = |x: &XVal, y: &XVal| -> bool {
        match op {
            "=" | "!=" => {
                let eq = match (x, y) {
                    (XVal::Bool(_), _) | (_, XVal::Bool(_)) => to_bool(x) == to_bool(y),
                    (XVal::Num(_), _) | (_, XVal::Num(_)) => to_num(doc, x) == to_num(doc, y),
                    _ => to_str(doc, x) == to_str(doc, y),
                };
                if op == "=" {
                    eq
                } else {
                    !eq
                }
            }
            _ => {
                let (l, r) = (to_num(doc, x), to_num(doc, y));
                match op {
                    "<" => l < r,
                    "<=" => l <= r,
                    ">" => l > r,
                    _ => l >= r,
                }
            }
        }
    };
    match (a, b) {
        (XVal::Nodes(xs), XVal::Nodes(ys)) => xs.iter().any(|x| ys.iter().any(|y| cmp_atoms(&XVal::Str(str_value(doc, *x)), &XVal::Str(str_value(doc, *y))))),
        (XVal::Nodes(xs), XVal::Bool(_)) => cmp_atoms(&XVal::Bool(!xs.is_empty()), b),
        (XVal::Bool(_), XVal::Nodes(ys)) => cmp_atoms(a, &XVal::Bool(!ys.is_empty())),
        (XVal::Nodes(xs), other) => xs.iter().any(|x| {
            let sv = str_value(doc, *x);
            match other {
                XVal::Num(_) => cmp_atoms(&XVal::Num(to_num_str(&sv)), other),
                _ => cmp_atoms(&XVal::Str(sv), other),
            }
        }),
        (other, XVal::Nodes(ys)) => ys.iter().any(|y| {
            let sv = str_value(doc, *y);
            match other {
                XVal::Num(_) => cmp_atoms(other, &XVal::Num(to_num_str(&sv))),
                _ => cmp_atoms(other, &XVal::Str(sv)),
            }
        }),
        _ => cmp_atoms(a, b),
    }
}

fn eval(x: &X, ctx: &Ctx) -> Result<XVal> {
    let doc = ctx.doc;
    Ok(match x {
        X::Path { absolute, steps } => {
            let start = if *absolute { vec![NRef::Node(0)] } else { vec![ctx.node] };
            XVal::Nodes(eval_steps(doc, start, steps)?)
        }
        X::Filter(prim, preds, steps) => {
            let v = eval(prim, ctx)?;
            let mut nodes = match v {
                XVal::Nodes(n) => n,
                _ => return Err(CamelError::expression("XPath: predicates/steps can only be applied to node-sets")),
            };
            for p in preds {
                let size = nodes.len();
                let mut kept = Vec::new();
                for (k, n) in nodes.iter().enumerate() {
                    let c = Ctx { doc, node: *n, pos: k + 1, size };
                    let keep = match eval(p, &c)? {
                        XVal::Num(num) => num == (k + 1) as f64,
                        other => to_bool(&other),
                    };
                    if keep {
                        kept.push(*n);
                    }
                }
                nodes = kept;
            }
            XVal::Nodes(eval_steps(doc, nodes, steps)?)
        }
        X::Or(a, b) => XVal::Bool(to_bool(&eval(a, ctx)?) || to_bool(&eval(b, ctx)?)),
        X::And(a, b) => XVal::Bool(to_bool(&eval(a, ctx)?) && to_bool(&eval(b, ctx)?)),
        X::Cmp(op, a, b) => XVal::Bool(compare(doc, op, &eval(a, ctx)?, &eval(b, ctx)?)),
        X::Arith(op, a, b) => {
            let (l, r) = (to_num(doc, &eval(a, ctx)?), to_num(doc, &eval(b, ctx)?));
            XVal::Num(match op.as_str() {
                "+" => l + r,
                "-" => l - r,
                "*" => l * r,
                "div" => l / r,
                _ => l % r,
            })
        }
        X::Neg(a) => XVal::Num(-to_num(doc, &eval(a, ctx)?)),
        X::Union(a, b) => {
            let (l, r) = (eval(a, ctx)?, eval(b, ctx)?);
            match (l, r) {
                (XVal::Nodes(mut x), XVal::Nodes(y)) => {
                    x.extend(y);
                    XVal::Nodes(sort_dedup(x))
                }
                _ => return Err(CamelError::expression("XPath: union operands must be node-sets")),
            }
        }
        X::Str(s) => XVal::Str(s.clone()),
        X::Num(n) => XVal::Num(*n),
        X::Func(name, args) => call(name, args, ctx)?,
    })
}

fn call(name: &str, args: &[X], ctx: &Ctx) -> Result<XVal> {
    let doc = ctx.doc;
    let arg = |i: usize| -> Result<XVal> { eval(&args[i], ctx) };
    let sarg = |i: usize| -> Result<String> {
        if args.len() > i {
            Ok(to_str(doc, &arg(i)?))
        } else {
            Ok(str_value(doc, ctx.node))
        }
    };
    let nodes_arg = |i: usize| -> Result<Vec<NRef>> {
        if args.len() > i {
            match arg(i)? {
                XVal::Nodes(n) => Ok(n),
                _ => Err(CamelError::expression(format!("XPath function {}() expects a node-set", name))),
            }
        } else {
            Ok(vec![ctx.node])
        }
    };
    let qname = |n: NRef| -> String {
        match n {
            NRef::Node(i) => doc.name(i).to_string(),
            NRef::Attr(e, a) => doc.attrs(e)[a].0.clone(),
        }
    };
    Ok(match name {
        "last" => XVal::Num(ctx.size as f64),
        "position" => XVal::Num(ctx.pos as f64),
        "count" => XVal::Num(nodes_arg(0)?.len() as f64),
        "name" => XVal::Str(nodes_arg(0)?.first().map(|n| qname(*n)).unwrap_or_default()),
        "local-name" => XVal::Str(nodes_arg(0)?.first().map(|n| local_name(&qname(*n)).to_string()).unwrap_or_default()),
        "namespace-uri" => XVal::Str(String::new()),
        "string" => XVal::Str(sarg(0)?),
        "concat" => {
            let mut s = String::new();
            for i in 0..args.len() {
                s.push_str(&to_str(doc, &arg(i)?));
            }
            XVal::Str(s)
        }
        "starts-with" => XVal::Bool(sarg(0)?.starts_with(&sarg(1)?)),
        "ends-with" => XVal::Bool(sarg(0)?.ends_with(&sarg(1)?)),
        "contains" => XVal::Bool(sarg(0)?.contains(&sarg(1)?)),
        "substring-before" => {
            let (s, t) = (sarg(0)?, sarg(1)?);
            XVal::Str(s.find(&t).map(|k| s[..k].to_string()).unwrap_or_default())
        }
        "substring-after" => {
            let (s, t) = (sarg(0)?, sarg(1)?);
            XVal::Str(s.find(&t).map(|k| s[k + t.len()..].to_string()).unwrap_or_default())
        }
        "substring" => {
            let s: Vec<char> = sarg(0)?.chars().collect();
            let start = to_num(doc, &arg(1)?).round();
            let end = if args.len() > 2 { start + to_num(doc, &arg(2)?).round() } else { f64::INFINITY };
            let out: String = s.iter().enumerate().filter(|(k, _)| {
                let p = (*k + 1) as f64;
                p >= start && p < end
            }).map(|(_, c)| *c).collect();
            XVal::Str(out)
        }
        "string-length" => XVal::Num(sarg(0)?.chars().count() as f64),
        "normalize-space" => XVal::Str(sarg(0)?.split_whitespace().collect::<Vec<_>>().join(" ")),
        "translate" => {
            let (s, from, to) = (sarg(0)?, sarg(1)?, sarg(2)?);
            let from: Vec<char> = from.chars().collect();
            let to: Vec<char> = to.chars().collect();
            XVal::Str(s.chars().filter_map(|c| match from.iter().position(|&f| f == c) {
                Some(k) => to.get(k).copied(),
                None => Some(c),
            }).collect())
        }
        "upper-case" => XVal::Str(sarg(0)?.to_uppercase()),
        "lower-case" => XVal::Str(sarg(0)?.to_lowercase()),
        "boolean" => XVal::Bool(to_bool(&arg(0)?)),
        "not" => XVal::Bool(!to_bool(&arg(0)?)),
        "true" => XVal::Bool(true),
        "false" => XVal::Bool(false),
        "number" => XVal::Num(if args.is_empty() { to_num_str(&str_value(doc, ctx.node)) } else { to_num(doc, &arg(0)?) }),
        "sum" => XVal::Num(nodes_arg(0)?.iter().map(|n| to_num_str(&str_value(doc, *n))).sum()),
        "floor" => XVal::Num(to_num(doc, &arg(0)?).floor()),
        "ceiling" => XVal::Num(to_num(doc, &arg(0)?).ceil()),
        "round" => XVal::Num(to_num(doc, &arg(0)?).round()),
        other => return Err(CamelError::expression(format!("Unknown XPath function {}()", other))),
    })
}

/// A compiled XPath expression.
#[derive(Debug, Clone)]
pub struct XPath {
    text: String,
    expr: X,
}

fn node_to_string(doc: &Document, n: NRef) -> String {
    match n {
        NRef::Node(i) => match &doc.nodes[i].kind {
            NodeKind::Element { .. } => doc.serialize(i),
            NodeKind::Document => doc.root_element().map(|r| doc.serialize(r)).unwrap_or_default(),
            _ => doc.text_content(i),
        },
        NRef::Attr(..) => str_value(doc, n),
    }
}

impl XPath {
    /// Compile an XPath expression.
    pub fn compile(text: &str) -> Result<XPath> {
        let toks = tokenize(text)?;
        if toks.is_empty() {
            return Err(xerr(text, "empty expression"));
        }
        let mut p = Parser { t: toks, i: 0, expr: text };
        let expr = p.or()?;
        if p.i < p.t.len() {
            return Err(p.err(&format!("unexpected token {:?}", p.t[p.i])));
        }
        Ok(XPath { text: text.to_string(), expr })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn raw(&self, doc: &Document) -> Result<XVal> {
        let ctx = Ctx { doc, node: NRef::Node(0), pos: 1, size: 1 };
        eval(&self.expr, &ctx)
    }

    /// Evaluate as a boolean (XPath `boolean()` semantics).
    pub fn matches(&self, doc: &Document) -> Result<bool> {
        Ok(to_bool(&self.raw(doc)?))
    }

    /// Evaluate with a result type (`String`, `Boolean`, `Number`, `Integer`, `NodeList`,
    /// `Node`, or None for the default mapping).
    pub fn evaluate(&self, doc: &Document, result_type: Option<&str>) -> Result<Value> {
        let v = self.raw(doc)?;
        let rt = result_type.map(|t| t.rsplit('.').next().unwrap_or(t).to_ascii_lowercase());
        Ok(match rt.as_deref() {
            Some("string") => Value::String(to_str(doc, &v)),
            Some("boolean") => Value::Bool(to_bool(&v)),
            Some("number") | Some("double") | Some("float") => Value::Float(to_num(doc, &v)),
            Some("integer") | Some("int") | Some("long") => {
                let n = to_num(doc, &v);
                if n.is_nan() {
                    Value::Null
                } else {
                    Value::Int(n as i64)
                }
            }
            Some("nodelist") | Some("nodeset") => match &v {
                XVal::Nodes(ns) => Value::Array(ns.iter().map(|n| Value::String(node_to_string(doc, *n))).collect()),
                other => Value::Array(vec![Value::String(to_str(doc, other))]),
            },
            Some("node") => match &v {
                XVal::Nodes(ns) => ns.first().map(|n| Value::String(node_to_string(doc, *n))).unwrap_or(Value::Null),
                other => Value::String(to_str(doc, other)),
            },
            _ => match v {
                XVal::Nodes(ns) => {
                    if ns.len() == 1 {
                        Value::String(node_to_string(doc, ns[0]))
                    } else {
                        Value::Array(ns.iter().map(|n| Value::String(node_to_string(doc, *n))).collect())
                    }
                }
                XVal::Str(s) => Value::String(s),
                XVal::Num(n) => Value::Float(n),
                XVal::Bool(b) => Value::Bool(b),
            },
        })
    }
}

/// Evaluate an XPath against XML text.
pub fn query(xml: &str, expr: &str, result_type: Option<&str>) -> Result<Value> {
    let doc = Document::parse(xml)?;
    XPath::compile(expr)?.evaluate(&doc, result_type)
}

struct XPathExpression {
    xpath: XPath,
    options: BTreeMap<String, String>,
}

impl XPathExpression {
    fn doc(&self, ex: &Exchange) -> Result<Document> {
        let input = super::jsonpath::source_value(ex, &self.options);
        if input.is_null() {
            return Err(CamelError::new("InvalidPayloadException", "No body available to evaluate xpath"));
        }
        Document::parse(&input.to_string())
    }
}

impl Expression for XPathExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let doc = self.doc(ex)?;
        self.xpath.evaluate(&doc, self.options.get("resultType").map(|s| s.as_str()).filter(|s| !s.is_empty()))
    }
}

impl Predicate for XPathExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        let doc = self.doc(ex)?;
        self.xpath.matches(&doc)
    }
}

/// The `xpath` language.
#[derive(Default)]
pub struct XPathLanguage;

impl Language for XPathLanguage {
    fn name(&self) -> &str {
        "xpath"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(XPathExpression { xpath: XPath::compile(text)?, options: options.clone() }))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(XPathExpression { xpath: XPath::compile(text)?, options: options.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let xml = "<a><b id='1'>x</b><b id='2'>y</b></a>";
        assert_eq!(query(xml, "/a/b[2]/text()", None).unwrap(), Value::from("y"));
        assert_eq!(query(xml, "count(//b)", None).unwrap(), Value::Float(2.0));
        assert_eq!(query(xml, "//b[@id='1']", Some("String")).unwrap(), Value::from("x"));
        assert!(XPath::compile("/a/[").is_err());
    }
}
