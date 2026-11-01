//! DataSonnet: a Jsonnet subset parser producing the shared AST, plus the `std`, `cml` and `ds`
//! libraries.

use std::sync::Arc;

use crate::ast::*;
use crate::error::SyntaxError;
use crate::interp::{Interp, R};
use crate::value::*;

type PR<T> = Result<T, SyntaxError>;

#[derive(Clone, Debug, PartialEq)]
enum JT {
    Id(String),
    Num(f64),
    Int(i64),
    Str(String),
    Op(&'static str),
    Eof,
}

const OPS: &[&str] = &[":::", "::", "&&", "||", "==", "!=", "<=", ">=", "<<", ">>", "+:", "{", "}", "[", "]", "(", ")", ",", ".", ";", ":", "=", "+", "-", "*", "/", "%", "<", ">", "!", "~", "&", "|", "^", "$"];

fn tokenize(src: &str) -> PR<Vec<(JT, usize)>> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let err = |at: usize, m: &str| SyntaxError { pos: at, msg: m.to_string() };
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'#' || (c == b'/' && b.get(i + 1) == Some(&b'/')) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = src[i + 2..].find("*/").ok_or_else(|| err(i, "unterminated comment"))?;
            i = i + 2 + end + 2;
            continue;
        }
        let start = i;
        if c.is_ascii_digit() {
            let mut j = i;
            let mut float = false;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && b[j] == b'.' && b.get(j + 1).is_some_and(|x| x.is_ascii_digit()) {
                float = true;
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
                let mut k = j + 1;
                if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
                    k += 1;
                }
                if k < b.len() && b[k].is_ascii_digit() {
                    float = true;
                    j = k;
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                }
            }
            let t = &src[i..j];
            out.push((if float { JT::Num(t.parse().map_err(|_| err(i, "invalid number"))?) } else { t.parse::<i64>().map(JT::Int).unwrap_or_else(|_| JT::Num(t.parse().unwrap_or(f64::NAN))) }, start));
            i = j;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() {
            let mut j = i;
            while j < b.len() && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            out.push((JT::Id(src[i..j].to_string()), start));
            i = j;
            continue;
        }
        if c == b'|' && src[i..].starts_with("|||") {
            // text block
            let mut j = i + 3;
            while j < b.len() && b[j] != b'\n' {
                j += 1;
            }
            j += 1;
            let rest = &src[j.min(src.len())..];
            let indent: String = rest.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            if indent.is_empty() {
                return Err(err(i, "text block requires indentation"));
            }
            let mut text = String::new();
            let mut k = j;
            loop {
                if k >= b.len() {
                    return Err(err(i, "unterminated text block"));
                }
                let line_end = src[k..].find('\n').map(|e| k + e).unwrap_or(src.len());
                let line = &src[k..line_end];
                if let Some(l) = line.strip_prefix(indent.as_str()) {
                    text.push_str(l);
                    text.push('\n');
                    k = line_end + 1;
                } else if line.trim().is_empty() && !line.trim_start().starts_with("|||") {
                    text.push('\n');
                    k = line_end + 1;
                } else {
                    let t = line.trim_start();
                    if !t.starts_with("|||") {
                        return Err(err(k, "text block not terminated with |||"));
                    }
                    k = k + (line.len() - t.len()) + 3;
                    break;
                }
            }
            out.push((JT::Str(text), start));
            i = k;
            continue;
        }
        if c == b'@' && matches!(b.get(i + 1), Some(b'\'' | b'"')) {
            let q = b[i + 1];
            let mut j = i + 2;
            let mut s = String::new();
            loop {
                if j >= b.len() {
                    return Err(err(i, "unterminated string"));
                }
                if b[j] == q {
                    if b.get(j + 1) == Some(&q) {
                        s.push(q as char);
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                let ch = src[j..].chars().next().unwrap();
                s.push(ch);
                j += ch.len_utf8();
            }
            out.push((JT::Str(s), start));
            i = j;
            continue;
        }
        if c == b'\'' || c == b'"' {
            let mut j = i + 1;
            let mut s = String::new();
            loop {
                if j >= b.len() {
                    return Err(err(i, "unterminated string"));
                }
                if b[j] == c {
                    j += 1;
                    break;
                }
                if b[j] == b'\\' {
                    let (e, nj) = crate::lexer::escape(src, j + 1).map_err(|m| err(j, &m))?;
                    s.push_str(&e);
                    j = nj;
                    continue;
                }
                let ch = src[j..].chars().next().unwrap();
                s.push(ch);
                j += ch.len_utf8();
            }
            out.push((JT::Str(s), start));
            i = j;
            continue;
        }
        match OPS.iter().find(|o| src[i..].starts_with(**o)) {
            Some(o) => {
                out.push((JT::Op(o), start));
                i += o.len();
            }
            None => return Err(err(i, &format!("unexpected character '{}'", src[i..].chars().next().unwrap()))),
        }
    }
    out.push((JT::Eof, src.len()));
    Ok(out)
}

/// Parse a DataSonnet / Jsonnet program.
pub fn parse_program(src: &str) -> PR<Program> {
    let toks = tokenize(src)?;
    let mut p = P { toks, i: 0 };
    let e = p.expr()?;
    if p.peek() != &JT::Eof {
        return p.err(format!("unexpected {} after end of expression", describe(p.peek())));
    }
    Ok(Program { dialect: Dialect::Jsonnet, body: vec![Stmt::Expr(e)] })
}

struct P {
    toks: Vec<(JT, usize)>,
    i: usize,
}

fn describe(t: &JT) -> String {
    match t {
        JT::Id(s) => format!("'{}'", s),
        JT::Num(f) => format!("number {}", f),
        JT::Int(i) => format!("number {}", i),
        JT::Str(_) => "string".into(),
        JT::Op(o) => format!("'{}'", o),
        JT::Eof => "end of input".into(),
    }
}

impl P {
    fn peek(&self) -> &JT {
        &self.toks[self.i].0
    }
    fn peek_at(&self, n: usize) -> &JT {
        &self.toks[(self.i + n).min(self.toks.len() - 1)].0
    }
    fn pos(&self) -> u32 {
        self.toks[self.i].1 as u32
    }
    fn next(&mut self) -> JT {
        let t = self.toks[self.i].0.clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), JT::Op(x) if *x == o)
    }
    fn eat_op(&mut self, o: &str) -> bool {
        if self.is_op(o) {
            self.next();
            true
        } else {
            false
        }
    }
    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), JT::Id(x) if x == k)
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.next();
            true
        } else {
            false
        }
    }
    fn err<X>(&self, m: impl Into<String>) -> PR<X> {
        Err(SyntaxError { pos: self.toks[self.i].1, msg: m.into() })
    }
    fn expect_op(&mut self, o: &str) -> PR<()> {
        if self.eat_op(o) {
            Ok(())
        } else {
            self.err(format!("expected '{}' but found {}", o, describe(self.peek())))
        }
    }
    fn id(&mut self) -> PR<String> {
        match self.next() {
            JT::Id(s) => Ok(s),
            other => {
                self.i -= 1;
                self.err(format!("expected identifier but found {}", describe(&other)))
            }
        }
    }

    fn params(&mut self) -> PR<Vec<Param>> {
        self.expect_op("(")?;
        let mut v = Vec::new();
        while !self.is_op(")") {
            let n = self.id()?;
            let default = if self.eat_op("=") { Some(self.expr()?) } else { None };
            v.push(Param { pat: Pat::Id(n), default, ty: None });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(v)
    }

    fn func(params: Vec<Param>, body: Expr) -> Expr {
        Expr::Func(Arc::new(FuncDef { name: None, params, rest: None, body: FuncBody::Expr(Box::new(body)), arrow: true, implicit_it: false, implicit_return: false, generator: false, is_async: false }))
    }

    fn bind(&mut self) -> PR<(String, Expr)> {
        let n = self.id()?;
        if self.is_op("(") {
            let ps = self.params()?;
            self.expect_op("=")?;
            let body = self.expr()?;
            return Ok((n, Self::func(ps, body)));
        }
        self.expect_op("=")?;
        Ok((n, self.expr()?))
    }

    fn expr(&mut self) -> PR<Expr> {
        let pos = self.pos();
        if self.eat_kw("local") {
            let mut binds = vec![self.bind()?];
            while self.eat_op(",") {
                binds.push(self.bind()?);
            }
            self.expect_op(";")?;
            let body = self.expr()?;
            return Ok(Expr::Let(binds, Box::new(body)));
        }
        if self.eat_kw("if") {
            let c = self.expr()?;
            if !self.eat_kw("then") {
                return self.err(format!("expected 'then' but found {}", describe(self.peek())));
            }
            let a = self.expr()?;
            let b = if self.eat_kw("else") { self.expr()? } else { Expr::Null };
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        if self.eat_kw("function") {
            let ps = self.params()?;
            let body = self.expr()?;
            return Ok(Self::func(ps, body));
        }
        if self.eat_kw("assert") {
            let c = self.expr()?;
            let msg = if self.eat_op(":") { self.expr()? } else { Expr::Str("Assertion failed".into()) };
            self.expect_op(";")?;
            let body = self.expr()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(body), Box::new(Expr::Error(Box::new(msg), pos))));
        }
        if self.eat_kw("error") {
            let e = self.expr()?;
            return Ok(Expr::Error(Box::new(e), pos));
        }
        if self.is_kw("import") || self.is_kw("importstr") || self.is_kw("importbin") {
            let kind = match self.next() {
                JT::Id(k) if k == "importstr" => 1,
                JT::Id(k) if k == "importbin" => 2,
                _ => 0,
            };
            return match self.next() {
                JT::Str(path) => Ok(Expr::Import(kind, path, pos)),
                other => {
                    self.i -= 1;
                    self.err(format!("expected a string literal after import but found {}", describe(&other)))
                }
            };
        }
        self.binary(1)
    }

    fn binop(&self) -> Option<(u8, Option<BinOp>, &'static str)> {
        Some(match self.peek() {
            JT::Op("||") => (1, None, "||"),
            JT::Op("&&") => (2, None, "&&"),
            JT::Op("|") => (3, Some(BinOp::BitOr), "|"),
            JT::Op("^") => (4, Some(BinOp::BitXor), "^"),
            JT::Op("&") => (5, Some(BinOp::BitAnd), "&"),
            JT::Op("==") => (6, Some(BinOp::Eq), "=="),
            JT::Op("!=") => (6, Some(BinOp::Ne), "!="),
            JT::Op("<") => (7, Some(BinOp::Lt), "<"),
            JT::Op("<=") => (7, Some(BinOp::Le), "<="),
            JT::Op(">") => (7, Some(BinOp::Gt), ">"),
            JT::Op(">=") => (7, Some(BinOp::Ge), ">="),
            JT::Id(k) if k == "in" => (7, Some(BinOp::In), "in"),
            JT::Op("<<") => (8, Some(BinOp::Shl), "<<"),
            JT::Op(">>") => (8, Some(BinOp::Shr), ">>"),
            JT::Op("+") => (9, Some(BinOp::Add), "+"),
            JT::Op("-") => (9, Some(BinOp::Sub), "-"),
            JT::Op("*") => (10, Some(BinOp::Mul), "*"),
            JT::Op("/") => (10, Some(BinOp::Div), "/"),
            JT::Op("%") => (10, Some(BinOp::Mod), "%"),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> PR<Expr> {
        let mut l = self.unary()?;
        while let Some((prec, op, sym)) = self.binop() {
            if prec < min {
                break;
            }
            let pos = self.pos();
            self.next();
            // `x in super` is not supported; `k in obj` checks fields
            let r = self.binary(prec + 1)?;
            l = match op {
                None if sym == "||" => Expr::Or(Box::new(l), Box::new(r)),
                None => Expr::And(Box::new(l), Box::new(r)),
                Some(op) => Expr::Bin(op, Box::new(l), Box::new(r), pos),
            };
        }
        Ok(l)
    }

    fn unary(&mut self) -> PR<Expr> {
        let pos = self.pos();
        let op = match self.peek() {
            JT::Op("-") => Some(UnOp::Neg),
            JT::Op("+") => Some(UnOp::Plus),
            JT::Op("!") => Some(UnOp::Not),
            JT::Op("~") => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let e = self.unary()?;
            return Ok(match (op, e) {
                (UnOp::Neg, Expr::Int(i)) => Expr::Int(-i),
                (UnOp::Neg, Expr::Num(f)) => Expr::Num(-f),
                (op, e) => Expr::Unary(op, Box::new(e), pos),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PR<Expr> {
        let mut e = self.primary()?;
        loop {
            let pos = self.pos();
            if self.eat_op(".") {
                let name = self.id()?;
                e = Expr::Member { obj: Box::new(e), name, optional: false, pos };
            } else if self.eat_op("[") {
                let mut parts: [Option<Box<Expr>>; 3] = [None, None, None];
                let mut idx = 0;
                let mut slice = false;
                loop {
                    if self.eat_op(":") {
                        slice = true;
                        idx += 1;
                        if idx > 2 {
                            return self.err("invalid slice");
                        }
                        continue;
                    }
                    if self.is_op("]") {
                        break;
                    }
                    parts[idx] = Some(Box::new(self.expr()?));
                }
                self.expect_op("]")?;
                if slice {
                    let [a, b, c] = parts;
                    e = Expr::Slice { obj: Box::new(e), start: a, end: b, step: c, pos };
                } else {
                    let i = parts[0].take().ok_or_else(|| SyntaxError { pos: pos as usize, msg: "empty index".into() })?;
                    e = Expr::Index { obj: Box::new(e), index: i, optional: false, pos };
                }
            } else if self.eat_op("(") {
                let mut args = Vec::new();
                let mut named = Vec::new();
                while !self.is_op(")") {
                    if matches!(self.peek(), JT::Id(_)) && matches!(self.peek_at(1), JT::Op("=")) {
                        let n = self.id()?;
                        self.next();
                        named.push(Prop::KV(PropKey::Name(n), self.expr()?));
                    } else {
                        args.push(Elem::Item(self.expr()?));
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(")")?;
                if !named.is_empty() {
                    args.push(Elem::Item(Expr::Call { callee: Box::new(Expr::Ident("__kwargs".into(), pos)), args: vec![Elem::Item(Expr::Object(named))], optional: false, pos }));
                }
                e = Expr::Call { callee: Box::new(e), args, optional: false, pos };
            } else if self.is_op("{") {
                // object extension: e { ... } == e + { ... }
                let o = self.primary()?;
                e = Expr::Bin(BinOp::Add, Box::new(e), Box::new(o), pos);
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn comp_clauses(&mut self) -> PR<Vec<CompClause>> {
        let mut v = Vec::new();
        loop {
            if self.eat_kw("for") {
                let n = self.id()?;
                if !self.eat_kw("in") {
                    return self.err("expected 'in' in comprehension");
                }
                let e = self.expr()?;
                v.push(CompClause::For(Pat::Id(n), e));
            } else if self.eat_kw("if") {
                v.push(CompClause::If(self.expr()?));
            } else {
                break;
            }
        }
        Ok(v)
    }

    fn primary(&mut self) -> PR<Expr> {
        let pos = self.pos();
        match self.next() {
            JT::Int(i) => Ok(Expr::Int(i)),
            JT::Num(f) => Ok(Expr::Num(f)),
            JT::Str(s) => Ok(Expr::Str(s)),
            JT::Id(k) => Ok(match k.as_str() {
                "null" => Expr::Null,
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "self" => Expr::JSelf(0, pos),
                "super" => Expr::JSelf(1, pos),
                _ => Expr::Ident(k, pos),
            }),
            JT::Op("$") => Ok(Expr::JSelf(2, pos)),
            JT::Op("(") => {
                let e = self.expr()?;
                self.expect_op(")")?;
                Ok(e)
            }
            JT::Op("[") => {
                if self.eat_op("]") {
                    return Ok(Expr::Array(vec![]));
                }
                let first = self.expr()?;
                if self.is_kw("for") {
                    let clauses = self.comp_clauses()?;
                    self.expect_op("]")?;
                    return Ok(Expr::Comp { kind: CompKind::List, key: Box::new(first), value: None, clauses });
                }
                let mut items = vec![Elem::Item(first)];
                while self.eat_op(",") {
                    if self.is_op("]") {
                        break;
                    }
                    items.push(Elem::Item(self.expr()?));
                }
                self.expect_op("]")?;
                Ok(Expr::Array(items))
            }
            JT::Op("{") => self.object(),
            other => {
                self.i -= 1;
                self.err(format!("unexpected {}", describe(&other)))
            }
        }
    }

    fn object(&mut self) -> PR<Expr> {
        let mut locals: Vec<(String, Expr)> = Vec::new();
        let mut fields: Vec<JFieldDef> = Vec::new();
        let mut asserts: Vec<(Expr, Expr, u32)> = Vec::new();
        while !self.is_op("}") {
            let pos = self.pos();
            if self.eat_kw("local") {
                locals.push(self.bind()?);
            } else if self.eat_kw("assert") {
                let c = self.expr()?;
                let m = if self.eat_op(":") { self.expr()? } else { Expr::Str("Object assertion failed".into()) };
                asserts.push((c, m, pos));
            } else {
                let key = match self.next() {
                    JT::Id(s) | JT::Str(s) => PropKey::Name(s),
                    JT::Op("[") => {
                        let k = self.expr()?;
                        self.expect_op("]")?;
                        PropKey::Computed(k)
                    }
                    other => {
                        self.i -= 1;
                        return self.err(format!("expected field name but found {}", describe(&other)));
                    }
                };
                let params = if self.is_op("(") { Some(self.params()?) } else { None };
                let mut plus = false;
                let vis;
                if self.eat_op(":") {
                    vis = Vis::Default;
                } else if self.eat_op("::") {
                    vis = Vis::Hidden;
                } else if self.eat_op(":::") {
                    vis = Vis::Visible;
                } else if self.eat_op("+:") {
                    // `+:`, `+::` (lexed `+:` `:`), `+:::` (`+:` `::`)
                    plus = true;
                    vis = if self.eat_op("::") {
                        Vis::Visible
                    } else if self.eat_op(":") {
                        Vis::Hidden
                    } else {
                        Vis::Default
                    };
                } else if self.eat_op("+") {
                    plus = true;
                    vis = if self.eat_op(":") {
                        Vis::Default
                    } else if self.eat_op("::") {
                        Vis::Hidden
                    } else if self.eat_op(":::") {
                        Vis::Visible
                    } else {
                        return self.err(format!("expected ':' but found {}", describe(self.peek())));
                    };
                } else {
                    return self.err(format!("expected ':' but found {}", describe(self.peek())));
                }
                let mut v = self.expr()?;
                if let Some(ps) = params {
                    v = Self::func(ps, v);
                }
                // object comprehension
                if self.is_kw("for") {
                    let clauses = self.comp_clauses()?;
                    self.eat_op(",");
                    self.expect_op("}")?;
                    let k = match key {
                        PropKey::Computed(k) => k,
                        PropKey::Name(_) => return self.err("object comprehension requires a computed [field]"),
                    };
                    let (k, v) = if locals.is_empty() { (k, v) } else { (Expr::Let(locals.clone(), Box::new(k)), Expr::Let(locals, Box::new(v))) };
                    return Ok(Expr::Comp { kind: CompKind::Dict, key: Box::new(k), value: Some(Box::new(v)), clauses });
                }
                fields.push(JFieldDef { key, vis, plus, value: v });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op("}")?;
        Ok(Expr::JObject(Arc::new(JObjDef { locals, fields, asserts })))
    }
}

// ====================================================================== libraries

fn arg(args: &[V], i: usize) -> V {
    args.get(i).cloned().unwrap_or(V::Null)
}

fn str_arg(it: &Interp, args: &[V], i: usize) -> String {
    match args.get(i) {
        Some(V::Str(s)) => s.to_string(),
        Some(v) => crate::builtins::jsonnet_to_string(it, v),
        None => String::new(),
    }
}

fn num(it: &Interp, args: &[V], i: usize, name: &str, pos: u32) -> R<f64> {
    match args.get(i) {
        Some(v) if v.is_number() => Ok(v.as_f64().unwrap()),
        Some(other) => it.throw("RUNTIME ERROR", format!("std.{} expected number, got {}", name, crate::builtins::jsonnet_type(other)), Some(pos)),
        None => it.throw("RUNTIME ERROR", format!("std.{}: missing argument", name), Some(pos)),
    }
}

fn arr(it: &mut Interp, v: &V, name: &str, pos: u32) -> R<Vec<V>> {
    match v {
        V::Arr(a) => Ok(a.borrow().clone()),
        V::Str(s) => Ok(s.chars().map(|c| V::string(c.to_string())).collect()),
        other => it.throw("RUNTIME ERROR", format!("std.{} expected array, got {}", name, crate::builtins::jsonnet_type(other)), Some(pos)),
    }
}

fn fields(v: &V) -> Vec<String> {
    match v {
        V::Obj(o) => {
            let mut k: Vec<String> = o.borrow().props.keys().map(|k| k.to_string()).collect();
            k.sort();
            k
        }
        _ => vec![],
    }
}

/// Members of `std` / `cml` / `ds` that are values.
pub fn std_member(_it: &mut Interp, ns: &'static str, name: &str) -> R<V> {
    Ok(match (ns, name) {
        ("std", "thisFile") => V::str(""),
        ("std", "pi") => V::Num(std::f64::consts::PI),
        ("ds", "datetime") => V::host(Host::Ns("ds.datetime")),
        _ => V::host(Host::NsFn(ns, name.to_string())),
    })
}

fn manifest(v: &V, indent: Option<String>, newline: &'static str, key_sep: &'static str) -> String {
    crate::json::stringify(v, &crate::json::JsonOpts { indent, item_sep: "", key_sep, sort_keys: true, js: false, newline }).unwrap_or_else(|| "null".into())
}

/// Call a `std.*`, `cml.*` or `ds.*` function.
pub fn std_call(it: &mut Interp, ns: &str, name: &str, args: Vec<V>, pos: u32) -> R<V> {
    let a0 = arg(&args, 0);
    let a1 = arg(&args, 1);
    match ns {
        "cml" => {
            let k = str_arg(it, &args, 0);
            return Ok(match name {
                "header" => it.host.headers.borrow().get(&k).map(from_value).unwrap_or(V::Null),
                "exchangeProperty" => it.host.properties.borrow().get(&k).map(from_value).unwrap_or(V::Null),
                "variable" => it.host.properties.borrow().get(&format!("CamelVariable.{}", k)).map(from_value).unwrap_or(V::Null),
                "properties" => match &it.host.context {
                    Some(c) => match c.resolve_placeholders(&format!("{{{{{}}}}}", k)) {
                        Ok(v) => V::string(v),
                        Err(_) => V::Null,
                    },
                    None => V::Null,
                },
                _ => return it.throw("RUNTIME ERROR", format!("field does not exist: cml.{}", name), Some(pos)),
            });
        }
        "ds" => {
            // DataSonnet `ds` library subset (argument order: collection first)
            let mapped: Option<(&str, Vec<V>)> = match name {
                "upper" | "uppercase" => Some(("asciiUpper", vec![a0.clone()])),
                "lower" | "lowercase" => Some(("asciiLower", vec![a0.clone()])),
                "trim" => Some(("trim", vec![a0.clone()])),
                "sizeOf" => Some(("length", vec![a0.clone()])),
                "keysOf" => Some(("objectFields", vec![a0.clone()])),
                "valuesOf" => Some(("objectValues", vec![a0.clone()])),
                "map" => Some(("map", vec![a1.clone(), a0.clone()])),
                "filter" => Some(("filter", vec![a1.clone(), a0.clone()])),
                "reduce" => Some(("foldl", vec![a1.clone(), a0.clone(), arg(&args, 2)])),
                "joinBy" => Some(("join", vec![a1.clone(), a0.clone()])),
                "splitBy" => Some(("split", vec![a0.clone(), a1.clone()])),
                "replace" => Some(("strReplace", vec![a0.clone(), a1.clone(), arg(&args, 2)])),
                "startsWith" => Some(("startsWith", vec![a0.clone(), a1.clone()])),
                "endsWith" => Some(("endsWith", vec![a0.clone(), a1.clone()])),
                "contains" => Some(("member", vec![a0.clone(), a1.clone()])),
                "toString" => Some(("toString", vec![a0.clone()])),
                "isEmpty" => Some(("isEmpty", vec![a0.clone()])),
                "flatten" => Some(("flattenArrays", vec![a0.clone()])),
                "sum" => Some(("sum", vec![a0.clone()])),
                "max" => Some(("maxArray", vec![a0.clone()])),
                "min" => Some(("minArray", vec![a0.clone()])),
                "abs" | "floor" | "ceil" | "round" => Some((name, vec![a0.clone()])),
                _ => None,
            };
            return match mapped {
                Some((n, a)) => std_call(it, "std", n, a, pos),
                None => it.throw("RUNTIME ERROR", format!("field does not exist: ds.{}", name), Some(pos)),
            };
        }
        _ => {}
    }
    let call = |it: &mut Interp, f: &V, a: Vec<V>| it.call(f, V::Undef, a, pos);
    Ok(match name {
        "length" => match &a0 {
            V::Str(s) => V::Int(s.chars().count() as i64),
            V::Arr(a) => V::Int(a.borrow().len() as i64),
            V::Obj(o) => V::Int(o.borrow().props.len() as i64),
            V::Func(c) => V::Int(c.def.params.len() as i64),
            other => return it.throw("RUNTIME ERROR", format!("length operates on strings, objects, and arrays, got {}", crate::builtins::jsonnet_type(other)), Some(pos)),
        },
        "type" => V::str(crate::builtins::jsonnet_type(&a0)),
        "isString" => V::Bool(matches!(a0, V::Str(_))),
        "isNumber" => V::Bool(a0.is_number()),
        "isBoolean" => V::Bool(matches!(a0, V::Bool(_))),
        "isArray" => V::Bool(matches!(a0, V::Arr(_))),
        "isObject" => V::Bool(matches!(a0, V::Obj(_))),
        "isFunction" => V::Bool(a0.is_callable()),
        "isEmpty" => V::Bool(match &a0 {
            V::Str(s) => s.is_empty(),
            V::Arr(a) => a.borrow().is_empty(),
            V::Obj(o) => o.borrow().props.is_empty(),
            V::Null => true,
            _ => false,
        }),
        "toString" => V::string(crate::builtins::jsonnet_to_string(it, &a0)),
        "parseJson" => match crate::json::parse(&str_arg(it, &args, 0)) {
            Ok(v) => v,
            Err(m) => return it.throw("RUNTIME ERROR", format!("failed to parse JSON: {}", m), Some(pos)),
        },
        "parseInt" => {
            let s = str_arg(it, &args, 0);
            match s.trim().parse::<i64>() {
                Ok(i) => V::Int(i),
                Err(_) => return it.throw("RUNTIME ERROR", format!("{} is not a base 10 integer", s), Some(pos)),
            }
        }
        "parseHex" => V::Int(i64::from_str_radix(&str_arg(it, &args, 0), 16).unwrap_or(0)),
        "manifestJson" => V::string(manifest(&a0, Some("    ".into()), "\n", " ")),
        "manifestJsonMinified" => V::string(manifest(&a0, None, "\n", "")),
        "manifestJsonEx" => {
            let ind = str_arg(it, &args, 1);
            let nl = if args.len() > 2 { str_arg(it, &args, 2) } else { "\n".into() };
            let nl: &'static str = if nl.is_empty() { "" } else { "\n" };
            if ind.is_empty() && nl.is_empty() {
                V::string(manifest(&a0, None, "", ""))
            } else {
                V::string(manifest(&a0, Some(ind), nl, " "))
            }
        }
        "escapeStringJson" => V::string(crate::json::quote(&str_arg(it, &args, 0))),
        "map" => {
            let mut out = Vec::new();
            for x in arr(it, &a1, name, pos)? {
                out.push(call(it, &a0, vec![x])?);
            }
            V::arr(out)
        }
        "mapWithIndex" => {
            let mut out = Vec::new();
            for (i, x) in arr(it, &a1, name, pos)?.into_iter().enumerate() {
                out.push(call(it, &a0, vec![V::Int(i as i64), x])?);
            }
            V::arr(out)
        }
        "mapWithKey" => {
            let mut pm = PropMap::default();
            for k in fields(&a1) {
                let v = it.get_member(&a1, &k, pos)?;
                let r = call(it, &a0, vec![V::str(&k), v])?;
                pm.set(&k, r);
            }
            V::obj(pm)
        }
        "filter" => {
            let mut out = Vec::new();
            for x in arr(it, &a1, name, pos)? {
                let r = call(it, &a0, vec![x.clone()])?;
                if !matches!(r, V::Bool(_)) {
                    return it.throw("RUNTIME ERROR", format!("filter function must return boolean, got: {}", crate::builtins::jsonnet_type(&r)), Some(pos));
                }
                if it.truthy(&r) {
                    out.push(x);
                }
            }
            V::arr(out)
        }
        "filterMap" => {
            let f = a0.clone();
            let m = a1.clone();
            let mut out = Vec::new();
            for x in arr(it, &arg(&args, 2), name, pos)? {
                let r = call(it, &f, vec![x.clone()])?;
                if it.truthy(&r) {
                    out.push(call(it, &m, vec![x])?);
                }
            }
            V::arr(out)
        }
        "flatMap" => {
            let mut out = Vec::new();
            for x in arr(it, &a1, name, pos)? {
                let r = call(it, &a0, vec![x])?;
                match r {
                    V::Arr(a) => out.extend(a.borrow().iter().cloned()),
                    V::Str(s) => out.push(V::Str(s)),
                    V::Null => {}
                    other => out.push(other),
                }
            }
            if matches!(a1, V::Str(_)) {
                V::string(out.iter().map(|x| crate::builtins::jsonnet_to_string(it, x)).collect())
            } else {
                V::arr(out)
            }
        }
        "foldl" | "foldr" => {
            let mut items = arr(it, &a1, name, pos)?;
            let mut acc = arg(&args, 2);
            if name == "foldr" {
                items.reverse();
                for x in items {
                    acc = call(it, &a0, vec![x, acc])?;
                }
            } else {
                for x in items {
                    acc = call(it, &a0, vec![acc, x])?;
                }
            }
            acc
        }
        "range" => {
            let (a, b) = (num(it, &args, 0, name, pos)? as i64, num(it, &args, 1, name, pos)? as i64);
            V::arr((a..=b).map(V::Int).collect())
        }
        "makeArray" => {
            let n = num(it, &args, 0, name, pos)? as i64;
            let mut out = Vec::new();
            for i in 0..n.max(0) {
                out.push(call(it, &a1, vec![V::Int(i)])?);
            }
            V::arr(out)
        }
        "repeat" => match &a0 {
            V::Str(s) => V::string(s.repeat(num(it, &args, 1, name, pos)?.max(0.0) as usize)),
            V::Arr(a) => {
                let items = a.borrow().clone();
                let mut out = Vec::new();
                for _ in 0..num(it, &args, 1, name, pos)?.max(0.0) as usize {
                    out.extend(items.iter().cloned());
                }
                V::arr(out)
            }
            _ => V::Null,
        },
        "join" => {
            let items = arr(it, &a1, name, pos)?;
            match &a0 {
                V::Str(sep) => V::string(items.iter().filter(|x| !x.is_nullish()).map(|x| crate::builtins::jsonnet_to_string(it, x)).collect::<Vec<_>>().join(sep)),
                V::Arr(sep) => {
                    let sep = sep.borrow().clone();
                    let mut out = Vec::new();
                    let mut first = true;
                    for x in items {
                        if x.is_nullish() {
                            continue;
                        }
                        if !first {
                            out.extend(sep.iter().cloned());
                        }
                        first = false;
                        out.extend(arr(it, &x, name, pos)?);
                    }
                    V::arr(out)
                }
                other => return it.throw("RUNTIME ERROR", format!("join first parameter should be a string or array, got {}", crate::builtins::jsonnet_type(other)), Some(pos)),
            }
        }
        "lines" => {
            let items = arr(it, &a0, name, pos)?;
            let mut s: String = items.iter().map(|x| crate::builtins::jsonnet_to_string(it, x)).collect::<Vec<_>>().join("\n");
            s.push('\n');
            V::string(s)
        }
        "split" | "splitLimit" => {
            let s = str_arg(it, &args, 0);
            let c = str_arg(it, &args, 1);
            let lim = if name == "splitLimit" { num(it, &args, 2, name, pos)? as i64 } else { -1 };
            let parts: Vec<V> = if lim < 0 { s.split(c.as_str()).map(V::str).collect() } else { s.splitn(lim as usize + 1, c.as_str()).map(V::str).collect() };
            V::arr(parts)
        }
        "strReplace" => V::string(str_arg(it, &args, 0).replace(&str_arg(it, &args, 1), &str_arg(it, &args, 2))),
        "substr" => {
            let s = str_arg(it, &args, 0);
            let from = num(it, &args, 1, name, pos)?.max(0.0) as usize;
            let len = num(it, &args, 2, name, pos)?.max(0.0) as usize;
            V::string(s.chars().skip(from).take(len).collect())
        }
        "startsWith" => V::Bool(str_arg(it, &args, 0).starts_with(&str_arg(it, &args, 1))),
        "endsWith" => V::Bool(str_arg(it, &args, 0).ends_with(&str_arg(it, &args, 1))),
        "asciiUpper" => V::string(str_arg(it, &args, 0).to_ascii_uppercase()),
        "asciiLower" => V::string(str_arg(it, &args, 0).to_ascii_lowercase()),
        "stringChars" => V::arr(str_arg(it, &args, 0).chars().map(|c| V::string(c.to_string())).collect()),
        "char" => V::string(char::from_u32(num(it, &args, 0, name, pos)? as u32).map(|c| c.to_string()).unwrap_or_default()),
        "codepoint" => V::Int(str_arg(it, &args, 0).chars().next().map(|c| c as i64).unwrap_or(0)),
        "trim" => V::string(str_arg(it, &args, 0).trim().to_string()),
        "stripChars" | "lstripChars" | "rstripChars" => {
            let s = str_arg(it, &args, 0);
            let cs: Vec<char> = str_arg(it, &args, 1).chars().collect();
            V::string(match name {
                "stripChars" => s.trim_matches(|c| cs.contains(&c)).to_string(),
                "lstripChars" => s.trim_start_matches(|c| cs.contains(&c)).to_string(),
                _ => s.trim_end_matches(|c| cs.contains(&c)).to_string(),
            })
        }
        "equalsIgnoreCase" => V::Bool(str_arg(it, &args, 0).to_lowercase() == str_arg(it, &args, 1).to_lowercase()),
        "format" => {
            let f = str_arg(it, &args, 0);
            let vals = match &a1 {
                V::Arr(a) => a.borrow().clone(),
                other => vec![other.clone()],
            };
            match crate::format::printf(it, &f, &vals, Some(&a1)) {
                Ok(s) => V::string(s),
                Err(m) => return it.throw("RUNTIME ERROR", m, Some(pos)),
            }
        }
        "objectFields" => V::arr(fields(&a0).into_iter().map(V::string).collect()),
        "objectFieldsAll" => match &a0 {
            V::Obj(o) => V::arr(it.jfields_all(o).into_iter().map(V::string).collect()),
            _ => V::arr(vec![]),
        },
        "objectValues" | "objectValuesAll" => {
            let mut out = Vec::new();
            let keys = match (&a0, name.ends_with("All")) {
                (V::Obj(o), true) => it.jfields_all(o),
                _ => fields(&a0),
            };
            for k in keys {
                out.push(it.get_member(&a0, &k, pos)?);
            }
            V::arr(out)
        }
        "objectKeysValues" | "objectKeysValuesAll" => {
            let mut out = Vec::new();
            for k in fields(&a0) {
                let v = it.get_member(&a0, &k, pos)?;
                let mut p = PropMap::default();
                p.set("key", V::str(&k));
                p.set("value", v);
                out.push(V::obj(p));
            }
            V::arr(out)
        }
        "objectHas" | "objectHasAll" => {
            let k = str_arg(it, &args, 1);
            V::Bool(matches!(&a0, V::Obj(o) if it.jhas(o, &k, name == "objectHasAll")))
        }
        "get" => {
            let k = str_arg(it, &args, 1);
            match &a0 {
                V::Obj(o) => o.borrow().props.get(&k).cloned().unwrap_or_else(|| arg(&args, 2)),
                _ => arg(&args, 2),
            }
        }
        "member" | "contains" => match &a0 {
            V::Str(s) => V::Bool(s.contains(&str_arg(it, &args, 1))),
            _ => {
                let items = arr(it, &a0, name, pos)?;
                V::Bool(items.iter().any(|x| it.equals(x, &a1)))
            }
        },
        "count" => {
            let items = arr(it, &a0, name, pos)?;
            V::Int(items.iter().filter(|x| it.equals(x, &a1)).count() as i64)
        }
        "find" => {
            let items = arr(it, &a1, name, pos)?;
            V::arr(items.iter().enumerate().filter(|(_, x)| it.equals(x, &a0)).map(|(i, _)| V::Int(i as i64)).collect())
        }
        "all" | "any" => {
            let items = arr(it, &a0, name, pos)?;
            V::Bool(if name == "all" { items.iter().all(|x| it.truthy(x)) } else { items.iter().any(|x| it.truthy(x)) })
        }
        "sort" | "set" | "uniq" => {
            let items = arr(it, &a0, name, pos)?;
            let key = match &args.get(1) {
                Some(V::Obj(o)) if o.borrow().kind == ObjKind::Kwargs => o.borrow().props.get("keyF").cloned(),
                Some(f) if f.is_callable() => Some((*f).clone()),
                _ => None,
            };
            let keyed: Vec<V> = {
                let mut v = Vec::new();
                for x in items {
                    let k = match &key {
                        Some(f) => call(it, f, vec![x.clone()])?,
                        None => x.clone(),
                    };
                    v.push(V::arr(vec![k, x]));
                }
                v
            };
            let k0 = |v: &V| if let V::Arr(a) = v { a.borrow()[0].clone() } else { V::Null };
            let k1 = |v: &V| if let V::Arr(a) = v { a.borrow()[1].clone() } else { V::Null };
            let sorted = if name == "uniq" { keyed } else { crate::builtins::sort_vals(it, keyed, &mut |it, a, b| Ok(crate::builtins::natural(it, &k0(a), &k0(b))))? };
            let mut out: Vec<V> = Vec::new();
            let mut last: Option<V> = None;
            for p in sorted {
                let k = k0(&p);
                if name != "sort" {
                    if let Some(l) = &last {
                        if it.equals(l, &k) {
                            continue;
                        }
                    }
                }
                last = Some(k);
                out.push(k1(&p));
            }
            V::arr(out)
        }
        "reverse" => {
            let mut items = arr(it, &a0, name, pos)?;
            items.reverse();
            V::arr(items)
        }
        "flattenArrays" => {
            let mut out = Vec::new();
            for x in arr(it, &a0, name, pos)? {
                match x {
                    V::Arr(a) => out.extend(a.borrow().iter().cloned()),
                    other => out.push(other),
                }
            }
            V::arr(out)
        }
        "slice" => {
            let s = if a1.is_nullish() { None } else { Some(a1.clone()) };
            let e = args.get(2).filter(|v| !v.is_nullish()).cloned();
            let st = args.get(3).filter(|v| !v.is_nullish()).cloned();
            crate::builtins::py_slice(it, &a0, s, e, st, pos)?
        }
        "setMember" => {
            let items = arr(it, &a1, name, pos)?;
            V::Bool(items.iter().any(|x| it.equals(x, &a0)))
        }
        "setUnion" | "setInter" | "setDiff" => {
            let x = arr(it, &a0, name, pos)?;
            let y = arr(it, &a1, name, pos)?;
            let out: Vec<V> = match name {
                "setUnion" => x.into_iter().chain(y).collect(),
                "setInter" => x.into_iter().filter(|v| y.iter().any(|w| it.equals(v, w))).collect(),
                _ => x.into_iter().filter(|v| !y.iter().any(|w| it.equals(v, w))).collect(),
            };
            return std_call(it, "std", "set", vec![V::arr(out)], pos);
        }
        "abs" => it.mk_num(num(it, &args, 0, name, pos)?.abs()),
        "sign" => it.mk_num(num(it, &args, 0, name, pos)?.signum()),
        "floor" => it.mk_num(num(it, &args, 0, name, pos)?.floor()),
        "ceil" => it.mk_num(num(it, &args, 0, name, pos)?.ceil()),
        "round" => it.mk_num(num(it, &args, 0, name, pos)?.round()),
        "sqrt" => it.mk_num(num(it, &args, 0, name, pos)?.sqrt()),
        "exp" => it.mk_num(num(it, &args, 0, name, pos)?.exp()),
        "log" => it.mk_num(num(it, &args, 0, name, pos)?.ln()),
        "pow" => it.mk_num(num(it, &args, 0, name, pos)?.powf(num(it, &args, 1, name, pos)?)),
        "max" => it.mk_num(num(it, &args, 0, name, pos)?.max(num(it, &args, 1, name, pos)?)),
        "min" => it.mk_num(num(it, &args, 0, name, pos)?.min(num(it, &args, 1, name, pos)?)),
        "mod" => it.binary(BinOp::Mod, a0, a1, pos)?,
        "sum" => {
            let items = arr(it, &a0, name, pos)?;
            it.mk_num(items.iter().map(|x| x.as_f64().unwrap_or(0.0)).sum())
        }
        "avg" => {
            let items = arr(it, &a0, name, pos)?;
            if items.is_empty() {
                return it.throw("RUNTIME ERROR", "Cannot calculate average of an empty array".into(), Some(pos));
            }
            it.mk_num(items.iter().map(|x| x.as_f64().unwrap_or(0.0)).sum::<f64>() / items.len() as f64)
        }
        "maxArray" | "minArray" => {
            let items = arr(it, &a0, name, pos)?;
            let mut best: Option<V> = None;
            for x in items {
                best = Some(match best {
                    None => x,
                    Some(b) => {
                        let o = crate::builtins::natural(it, &x, &b);
                        if (name == "maxArray" && o == std::cmp::Ordering::Greater) || (name == "minArray" && o == std::cmp::Ordering::Less) {
                            x
                        } else {
                            b
                        }
                    }
                });
            }
            match best {
                Some(b) => b,
                None => return it.throw("RUNTIME ERROR", "Expected at least one element in array".into(), Some(pos)),
            }
        }
        "base64" => V::string(base64(str_arg(it, &args, 0).as_bytes())),
        "base64Decode" => V::string(String::from_utf8_lossy(&unbase64(&str_arg(it, &args, 0))).into_owned()),
        "md5" => V::string(camel_core::language::support::to_hex(&camel_core::language::support::md5(str_arg(it, &args, 0).as_bytes()))),
        "mergePatch" => merge_patch(&a0, &a1),
        "prune" => prune(&a0).unwrap_or(V::Null),
        "assertEqual" => {
            if it.equals(&a0, &a1) {
                V::Bool(true)
            } else {
                return it.throw("RUNTIME ERROR", format!("Assertion failed. {} != {}", crate::builtins::jsonnet_manifest(&a0), crate::builtins::jsonnet_manifest(&a1)), Some(pos));
            }
        }
        _ => return it.throw("RUNTIME ERROR", format!("field does not exist: {}", name), Some(pos)),
    })
}

fn merge_patch(target: &V, patch: &V) -> V {
    match patch {
        V::Obj(p) => {
            let mut pm = match target {
                V::Obj(t) => t.borrow().props.clone(),
                _ => PropMap::default(),
            };
            for (k, v) in p.borrow().props.iter() {
                if v.is_nullish() {
                    pm.remove(k);
                } else {
                    let cur = pm.get(k).cloned().unwrap_or(V::Null);
                    pm.set(k, merge_patch(&cur, v));
                }
            }
            V::obj(pm)
        }
        other => other.clone(),
    }
}

fn prune(v: &V) -> Option<V> {
    match v {
        V::Null | V::Undef => None,
        V::Arr(a) => {
            let items: Vec<V> = a.borrow().iter().filter_map(prune).collect();
            if items.is_empty() {
                None
            } else {
                Some(V::arr(items))
            }
        }
        V::Obj(o) => {
            let mut pm = PropMap::default();
            for (k, x) in o.borrow().props.iter() {
                if let Some(p) = prune(x) {
                    pm.set(k, p);
                }
            }
            if pm.is_empty() {
                None
            } else {
                Some(V::obj(pm))
            }
        }
        other => Some(other.clone()),
    }
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

fn unbase64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match B64.iter().position(|x| *x == c) {
            Some(v) => v as u32,
            None => continue,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}
