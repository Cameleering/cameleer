//! Python subset: indentation-aware tokenizer and parser producing the shared AST.

use std::sync::Arc;

use crate::ast::*;
use crate::error::SyntaxError;

type R<T> = Result<T, SyntaxError>;

#[derive(Clone, Debug, PartialEq)]
enum PT {
    Name(String),
    Int(i64),
    Num(f64),
    Str(String),
    /// f-string parts: literal text or (code, offset).
    FStr(Vec<(bool, String, usize)>),
    Op(&'static str),
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Clone, Debug)]
struct Tok {
    t: PT,
    pos: usize,
}

const OPS: &[&str] = &[
    "**=", "//=", ">>=", "<<=", "...", "**", "//", "==", "!=", "<=", ">=", "->", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", ":=", "<<", ">>", "+", "-", "*", "/", "%",
    "@", "&", "|", "^", "~", "<", ">", "(", ")", "[", "]", "{", "}", ",", ":", ".", ";", "=",
];

fn tokenize(src: &str, base: usize) -> R<Vec<Tok>> {
    let b = src.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut indents: Vec<usize> = vec![0];
    let mut depth = 0i32;
    let mut i = 0;
    let mut at_line_start = true;
    let err = |at: usize, msg: &str| SyntaxError { pos: base + at, msg: msg.to_string() };
    while i <= b.len() {
        if at_line_start && depth == 0 {
            // measure indentation
            let mut col = 0;
            let mut j = i;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                col += if b[j] == b'\t' { 8 - col % 8 } else { 1 };
                j += 1;
            }
            if j >= b.len() {
                break;
            }
            if b[j] == b'\n' || b[j] == b'\r' || b[j] == b'#' {
                // blank or comment line
                while j < b.len() && b[j] != b'\n' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            let cur = *indents.last().unwrap();
            if col > cur {
                indents.push(col);
                toks.push(Tok { t: PT::Indent, pos: base + j });
            } else {
                while col < *indents.last().unwrap() {
                    indents.pop();
                    toks.push(Tok { t: PT::Dedent, pos: base + j });
                }
                if col != *indents.last().unwrap() {
                    return Err(err(j, "unindent does not match any outer indentation level"));
                }
            }
            i = j;
            at_line_start = false;
        }
        if i >= b.len() {
            break;
        }
        let c = b[i];
        if c == b'\n' {
            if depth == 0 {
                if !matches!(toks.last().map(|t| &t.t), Some(PT::Newline) | None) {
                    toks.push(Tok { t: PT::Newline, pos: base + i });
                }
                at_line_start = true;
            }
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'\\' && b.get(i + 1) == Some(&b'\n') {
            i += 2;
            continue;
        }
        let start = i;
        // string prefixes
        let mut j = i;
        while j < b.len() && j - i < 2 && matches!(b[j].to_ascii_lowercase(), b'r' | b'b' | b'f' | b'u') {
            j += 1;
        }
        if j < b.len() && (b[j] == b'\'' || b[j] == b'"') && (j == i || b[i].is_ascii_alphabetic()) {
            let prefix = src[i..j].to_ascii_lowercase();
            let raw = prefix.contains('r');
            let fstr = prefix.contains('f');
            let q = b[j];
            let triple = b.get(j + 1) == Some(&q) && b.get(j + 2) == Some(&q);
            let mut k = j + if triple { 3 } else { 1 };
            let mut text = String::new();
            let mut parts: Vec<(bool, String, usize)> = Vec::new();
            loop {
                if k >= b.len() {
                    return Err(err(start, "unterminated string literal"));
                }
                let ch = b[k];
                if ch == q {
                    if !triple {
                        k += 1;
                        break;
                    }
                    if b.get(k + 1) == Some(&q) && b.get(k + 2) == Some(&q) {
                        k += 3;
                        break;
                    }
                }
                if ch == b'\n' && !triple {
                    return Err(err(start, "unterminated string literal"));
                }
                if ch == b'\\' && !raw {
                    let (s, nk) = crate::lexer::escape(src, k + 1).map_err(|m| err(k, &m))?;
                    text.push_str(&s);
                    k = nk;
                    continue;
                }
                if ch == b'\\' && raw && k + 1 < b.len() {
                    text.push('\\');
                    let c2 = src[k + 1..].chars().next().unwrap();
                    text.push(c2);
                    k += 1 + c2.len_utf8();
                    continue;
                }
                if fstr && ch == b'{' {
                    if b.get(k + 1) == Some(&b'{') {
                        text.push('{');
                        k += 2;
                        continue;
                    }
                    let end = crate::lexer::matching_brace(src, k + 1).ok_or_else(|| err(k, "f-string: expecting '}'"))?;
                    if !text.is_empty() {
                        parts.push((false, std::mem::take(&mut text), 0));
                    }
                    parts.push((true, src[k + 1..end].to_string(), base + k + 1));
                    k = end + 1;
                    continue;
                }
                if fstr && ch == b'}' && b.get(k + 1) == Some(&b'}') {
                    text.push('}');
                    k += 2;
                    continue;
                }
                let chr = src[k..].chars().next().unwrap();
                text.push(chr);
                k += chr.len_utf8();
            }
            if fstr {
                if !text.is_empty() {
                    parts.push((false, text, 0));
                }
                toks.push(Tok { t: PT::FStr(parts), pos: base + start });
            } else {
                toks.push(Tok { t: PT::Str(text), pos: base + start });
            }
            i = k;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|x| x.is_ascii_digit())) {
            let mut k = i;
            if c == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')) {
                let radix = match b[i + 1] {
                    b'x' | b'X' => 16,
                    b'o' | b'O' => 8,
                    _ => 2,
                };
                k += 2;
                let s0 = k;
                while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                    k += 1;
                }
                let digits: String = src[s0..k].chars().filter(|c| *c != '_').collect();
                let v = i64::from_str_radix(&digits, radix).map_err(|_| err(i, "invalid number literal"))?;
                toks.push(Tok { t: PT::Int(v), pos: base + start });
                i = k;
                continue;
            }
            let mut float = false;
            while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'_') {
                k += 1;
            }
            if k < b.len() && b[k] == b'.' {
                float = true;
                k += 1;
                while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'_') {
                    k += 1;
                }
            }
            if k < b.len() && (b[k] == b'e' || b[k] == b'E') {
                let mut m = k + 1;
                if m < b.len() && (b[m] == b'+' || b[m] == b'-') {
                    m += 1;
                }
                if m < b.len() && b[m].is_ascii_digit() {
                    float = true;
                    k = m;
                    while k < b.len() && b[k].is_ascii_digit() {
                        k += 1;
                    }
                }
            }
            let text: String = src[i..k].chars().filter(|c| *c != '_').collect();
            let t = if float {
                PT::Num(text.parse().map_err(|_| err(i, "invalid number literal"))?)
            } else {
                match text.parse::<i64>() {
                    Ok(v) => PT::Int(v),
                    Err(_) => PT::Num(text.parse().map_err(|_| err(i, "invalid number literal"))?),
                }
            };
            toks.push(Tok { t, pos: base + start });
            i = k;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 {
            let mut k = i;
            while k < b.len() && (b[k] == b'_' || b[k].is_ascii_alphanumeric() || b[k] >= 0x80) {
                k += 1;
            }
            while !src.is_char_boundary(k) {
                k += 1;
            }
            toks.push(Tok { t: PT::Name(src[i..k].to_string()), pos: base + start });
            i = k;
            continue;
        }
        let rest = &src[i..];
        match OPS.iter().find(|o| rest.starts_with(**o)) {
            Some(o) => {
                match *o {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    _ => {}
                }
                toks.push(Tok { t: PT::Op(o), pos: base + start });
                i += o.len();
            }
            None => {
                let ch = rest.chars().next().unwrap();
                return Err(err(i, &format!("invalid character '{}'", ch)));
            }
        }
    }
    let end = base + src.len();
    if !matches!(toks.last().map(|t| &t.t), Some(PT::Newline) | None) {
        toks.push(Tok { t: PT::Newline, pos: end });
    }
    while indents.len() > 1 {
        indents.pop();
        toks.push(Tok { t: PT::Dedent, pos: end });
    }
    toks.push(Tok { t: PT::Eof, pos: end });
    Ok(toks)
}

/// Parse a Python program.
pub fn parse_program(src: &str) -> R<Program> {
    let toks = tokenize(src, 0)?;
    let mut p = P { toks, i: 0, saw_yield: false, tmp: 0, meta: None };
    let mut body = Vec::new();
    while !p.is(&PT::Eof) {
        if p.eat(&PT::Newline) {
            continue;
        }
        body.extend(p.statement()?);
    }
    Ok(Program { dialect: Dialect::Python, body })
}

fn parse_embedded(code: &str, base: usize) -> R<Expr> {
    let toks = tokenize(code.trim_end(), base)?;
    let mut p = P { toks, i: 0, saw_yield: false, tmp: 0, meta: None };
    let e = p.testlist()?;
    p.eat(&PT::Newline);
    if !p.is(&PT::Eof) {
        return p.err("invalid syntax in f-string expression");
    }
    Ok(e)
}

struct P {
    toks: Vec<Tok>,
    i: usize,
    /// A `yield` was parsed in the current function body.
    saw_yield: bool,
    /// Counter for temporary names (`match` subjects).
    tmp: usize,
    /// `metaclass=` of the class statement being parsed.
    meta: Option<Expr>,
}

/// A `match` statement pattern.
enum MP {
    Wild,
    Capture(String),
    Value(Expr),
    /// Sequence items and the position / name of a `*rest` item.
    Seq(Vec<MP>, Option<(usize, Option<String>)>),
    /// Mapping entries and `**rest`.
    Map(Vec<(Expr, MP)>, Option<String>),
    Class(Expr, Vec<MP>, Vec<(String, MP)>),
    Or(Vec<MP>),
    As(Box<MP>, String),
}

/// Keyword arguments of a parsed call (`f(a=1)` passes them through `__kwargs({...})`).
fn kwargs_of(args: &[Elem]) -> Vec<(String, Expr)> {
    let mut out = vec![];
    for a in args {
        if let Elem::Item(Expr::Call { callee, args: inner, .. }) = a {
            if matches!(&**callee, Expr::Ident(n, _) if n == "__kwargs") {
                if let Some(Elem::Item(Expr::Object(props))) = inner.first() {
                    for p in props {
                        if let Prop::KV(PropKey::Name(k), v) = p {
                            out.push((k.clone(), v.clone()));
                        }
                    }
                }
            }
        }
    }
    out
}

fn call(name: &str, args: Vec<Expr>, pos: u32) -> Expr {
    Expr::Call { callee: Box::new(Expr::Ident(name.to_string(), pos)), args: args.into_iter().map(Elem::Item).collect(), optional: false, pos }
}

fn and_all(conds: Vec<Expr>) -> Expr {
    let mut it = conds.into_iter();
    let first = it.next().unwrap_or(Expr::Bool(true));
    it.fold(first, |a, b| Expr::And(Box::new(a), Box::new(b)))
}

fn bind(name: &str, v: Expr, pos: u32) -> Expr {
    Expr::Seq(vec![Expr::Assign { op: None, target: Box::new(Pat::Id(name.to_string())), value: Box::new(v), pos }, Expr::Bool(true)])
}

/// The condition (with binding side effects) testing pattern `mp` against `s`.
fn pattern_cond(mp: &MP, s: &Expr, pos: u32) -> Expr {
    match mp {
        MP::Wild => Expr::Bool(true),
        MP::Capture(n) => bind(n, s.clone(), pos),
        MP::Value(e) => match e {
            Expr::Null | Expr::Bool(_) => Expr::Bin(BinOp::Is, Box::new(s.clone()), Box::new(e.clone()), pos),
            _ => Expr::Bin(BinOp::Eq, Box::new(s.clone()), Box::new(e.clone()), pos),
        },
        MP::Seq(items, star) => {
            let n = items.len() as i64;
            let mut conds = vec![call("%match_seq", vec![s.clone()], pos)];
            let len = call("len", vec![s.clone()], pos);
            conds.push(match star {
                Some(_) => Expr::Bin(BinOp::Ge, Box::new(len), Box::new(Expr::Int(n)), pos),
                None => Expr::Bin(BinOp::Eq, Box::new(len), Box::new(Expr::Int(n)), pos),
            });
            let split = star.as_ref().map(|(k, _)| *k).unwrap_or(items.len());
            for (i, p) in items.iter().enumerate() {
                let idx = if i < split { Expr::Int(i as i64) } else { Expr::Int(i as i64 - n) };
                conds.push(pattern_cond(p, &Expr::Index { obj: Box::new(s.clone()), index: Box::new(idx), optional: false, pos }, pos));
            }
            if let Some((k, Some(name))) = star {
                let after = n - *k as i64;
                let end = if after == 0 { None } else { Some(Box::new(Expr::Int(-after))) };
                let slice = Expr::Slice { obj: Box::new(s.clone()), start: Some(Box::new(Expr::Int(*k as i64))), end, step: None, pos };
                conds.push(bind(name, call("list", vec![slice], pos), pos));
            }
            and_all(conds)
        }
        MP::Map(entries, rest) => {
            let mut conds = vec![call("%match_map", vec![s.clone()], pos)];
            for (k, p) in entries {
                conds.push(Expr::Bin(BinOp::In, Box::new(k.clone()), Box::new(s.clone()), pos));
                conds.push(pattern_cond(p, &Expr::Index { obj: Box::new(s.clone()), index: Box::new(k.clone()), optional: false, pos }, pos));
            }
            if let Some(r) = rest {
                let keys = Expr::Array(entries.iter().map(|(k, _)| Elem::Item(k.clone())).collect());
                conds.push(bind(r, call("%match_rest", vec![s.clone(), keys], pos), pos));
            }
            and_all(conds)
        }
        MP::Class(cls, args, kw) => {
            let mut conds = vec![call("isinstance", vec![s.clone(), cls.clone()], pos)];
            for (i, p) in args.iter().enumerate() {
                let sub = call("%match_pos", vec![s.clone(), cls.clone(), Expr::Int(i as i64)], pos);
                conds.push(pattern_cond(p, &sub, pos));
            }
            for (name, p) in kw {
                conds.push(call("hasattr", vec![s.clone(), Expr::Str(name.clone())], pos));
                conds.push(pattern_cond(p, &Expr::Member { obj: Box::new(s.clone()), name: name.clone(), optional: false, pos }, pos));
            }
            and_all(conds)
        }
        MP::Or(ps) => {
            let mut it = ps.iter().map(|p| pattern_cond(p, s, pos));
            let first = it.next().unwrap_or(Expr::Bool(false));
            it.fold(first, |a, b| Expr::Or(Box::new(a), Box::new(b)))
        }
        MP::As(p, n) => Expr::And(Box::new(pattern_cond(p, s, pos)), Box::new(bind(n, s.clone(), pos))),
    }
}

fn describe(t: &PT) -> String {
    match t {
        PT::Name(n) => format!("'{}'", n),
        PT::Int(i) => format!("number {}", i),
        PT::Num(f) => format!("number {}", f),
        PT::Str(_) | PT::FStr(_) => "string".into(),
        PT::Op(o) => format!("'{}'", o),
        PT::Newline => "end of line".into(),
        PT::Indent => "indent".into(),
        PT::Dedent => "dedent".into(),
        PT::Eof => "end of input".into(),
    }
}

impl P {
    fn peek(&self) -> &PT {
        &self.toks[self.i].t
    }
    fn peek_at(&self, n: usize) -> &PT {
        &self.toks[(self.i + n).min(self.toks.len() - 1)].t
    }
    fn pos(&self) -> u32 {
        self.toks[self.i].pos as u32
    }
    fn next(&mut self) -> PT {
        let t = self.toks[self.i].t.clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn is(&self, t: &PT) -> bool {
        self.peek() == t
    }
    fn eat(&mut self, t: &PT) -> bool {
        if self.is(t) {
            self.next();
            true
        } else {
            false
        }
    }
    fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), PT::Op(x) if *x == o)
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
        matches!(self.peek(), PT::Name(x) if x == k)
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.next();
            true
        } else {
            false
        }
    }
    fn err<X>(&self, msg: impl Into<String>) -> R<X> {
        Err(SyntaxError { pos: self.toks[self.i].pos, msg: msg.into() })
    }
    fn expect_op(&mut self, o: &str) -> R<()> {
        if self.eat_op(o) {
            Ok(())
        } else {
            self.err(format!("expected '{}' but found {}", o, describe(self.peek())))
        }
    }
    fn name(&mut self) -> R<String> {
        match self.peek().clone() {
            PT::Name(n) => {
                self.next();
                Ok(n)
            }
            other => self.err(format!("expected name but found {}", describe(&other))),
        }
    }

    // ------------------------------------------------------------------ statements
    fn statement(&mut self) -> R<Vec<Stmt>> {
        if self.is_op("@") {
            return self.decorated();
        }
        if let PT::Name(k) = self.peek().clone() {
            match k.as_str() {
                "if" => return Ok(vec![self.if_stmt()?]),
                "while" => {
                    self.next();
                    let c = self.test()?;
                    let body = self.block()?;
                    let els = if self.eat_kw("else") { Some(Box::new(Stmt::Block(self.block()?))) } else { None };
                    return Ok(vec![Stmt::While(c, Box::new(Stmt::Block(body)), els)]);
                }
                "for" => {
                    self.next();
                    let target = self.target_list()?;
                    if !self.eat_kw("in") {
                        return self.err(format!("expected 'in' but found {}", describe(self.peek())));
                    }
                    let obj = self.testlist()?;
                    let body = self.block()?;
                    let els = if self.eat_kw("else") { Some(Box::new(Stmt::Block(self.block()?))) } else { None };
                    return Ok(vec![Stmt::ForIn { decl: None, pat: target, obj, body: Box::new(Stmt::Block(body)), values: true, else_body: els }]);
                }
                "def" => return Ok(vec![Stmt::Func(Arc::new(self.def_stmt()?))]),
                "try" => return Ok(vec![self.try_stmt()?]),
                "class" => {
                    let c = self.class_stmt(vec![])?;
                    return Ok(self.class_with_meta(c, vec![]));
                }
                "with" => return Ok(vec![self.with_stmt(false)?]),
                "async" if matches!(self.peek_at(1), PT::Name(n) if n == "def") => {
                    self.next();
                    let mut f = self.def_stmt()?;
                    f.is_async = true;
                    return Ok(vec![Stmt::Func(Arc::new(f))]);
                }
                "async" if matches!(self.peek_at(1), PT::Name(n) if n == "with") => {
                    self.next();
                    return Ok(vec![self.with_stmt(true)?]);
                }
                "async" if matches!(self.peek_at(1), PT::Name(n) if n == "for") => {
                    self.next();
                    return Ok(vec![self.async_for()?]);
                }
                "match" if self.is_match_stmt() => return Ok(vec![self.match_stmt()?]),
                _ => {}
            }
        }
        let mut out = vec![self.simple_stmt()?];
        while self.eat_op(";") {
            if self.is(&PT::Newline) || self.is(&PT::Eof) {
                break;
            }
            out.push(self.simple_stmt()?);
        }
        if !self.eat(&PT::Newline) && !self.is(&PT::Eof) && !self.is(&PT::Dedent) {
            return self.err(format!("invalid syntax: unexpected {}", describe(self.peek())));
        }
        Ok(out)
    }

    /// A class statement plus the runtime class creation hooks: `metaclass=` (or a parent's
    /// metaclass) and class decorators other than `@dataclass`.
    fn class_with_meta(&mut self, c: ClassDef, decorators: Vec<Expr>) -> Vec<Stmt> {
        let pos = self.pos();
        let name = c.name.clone().unwrap_or_default();
        let has_bases = c.extends.is_some();
        let meta = self.meta.take();
        let mut out = vec![Stmt::Class(Arc::new(c))];
        if meta.is_some() || has_bases {
            let m = meta.unwrap_or(Expr::Null);
            out.push(Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(name.clone())), value: Box::new(call("%metaclass", vec![m, Expr::Ident(name.clone(), pos)], pos)), pos }));
        }
        let mut value = Expr::Ident(name.clone(), pos);
        let mut any = false;
        for d in decorators.into_iter().rev() {
            let is_dc = match &d {
                Expr::Ident(n, _) => n == "dataclass",
                Expr::Member { name, .. } => name == "dataclass",
                Expr::Call { callee, .. } => matches!(&**callee, Expr::Ident(n, _) if n == "dataclass") || matches!(&**callee, Expr::Member { name, .. } if name == "dataclass"),
                _ => false,
            };
            if !is_dc {
                value = Expr::Call { callee: Box::new(d), args: vec![Elem::Item(value)], optional: false, pos };
                any = true;
            }
        }
        if any {
            out.push(Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(name)), value: Box::new(value), pos }));
        }
        out
    }

    /// `match` as a statement (a soft keyword): `match subject:` NEWLINE INDENT `case`.
    fn is_match_stmt(&self) -> bool {
        let mut n = 1;
        let mut depth = 0;
        loop {
            match self.peek_at(n) {
                PT::Op("(") | PT::Op("[") | PT::Op("{") => depth += 1,
                PT::Op(")") | PT::Op("]") | PT::Op("}") => depth -= 1,
                PT::Op(":") if depth == 0 => {
                    return n > 1 && matches!(self.peek_at(n + 1), PT::Newline) && matches!(self.peek_at(n + 2), PT::Indent) && matches!(self.peek_at(n + 3), PT::Name(c) if c == "case");
                }
                PT::Op("=") if depth == 0 => return false,
                PT::Newline | PT::Eof => return false,
                _ => {}
            }
            n += 1;
        }
    }

    /// `match subject:` with `case pattern [if guard]:` blocks, compiled to an if/elif chain.
    fn match_stmt(&mut self) -> R<Stmt> {
        let pos = self.pos();
        self.next();
        let subject = self.testlist_star()?;
        self.expect_op(":")?;
        if !self.eat(&PT::Newline) || !self.eat(&PT::Indent) {
            return self.err("expected an indented block of 'case' clauses");
        }
        self.tmp += 1;
        let tmp = format!("%subj{}", self.tmp);
        let s = Expr::Ident(tmp.clone(), pos);
        let mut cases = Vec::new();
        while !self.eat(&PT::Dedent) {
            if self.is(&PT::Eof) {
                break;
            }
            if self.eat(&PT::Newline) {
                continue;
            }
            if !self.eat_kw("case") {
                return self.err(format!("expected 'case' but found {}", describe(self.peek())));
            }
            let p = self.pattern_top()?;
            let guard = if self.eat_kw("if") { Some(self.test()?) } else { None };
            let body = self.block()?;
            let mut cond = pattern_cond(&p, &s, pos);
            if let Some(g) = guard {
                cond = Expr::And(Box::new(cond), Box::new(g));
            }
            cases.push((cond, body));
        }
        let mut chain: Option<Box<Stmt>> = None;
        for (cond, body) in cases.into_iter().rev() {
            chain = Some(Box::new(Stmt::If(cond, Box::new(Stmt::Block(body)), chain)));
        }
        let mut out = vec![Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(tmp)), value: Box::new(subject), pos })];
        if let Some(c) = chain {
            out.push(*c);
        }
        Ok(Stmt::Block(out))
    }

    fn pattern_top(&mut self) -> R<MP> {
        let first = self.or_pattern()?;
        if self.is_op(",") {
            let mut items = vec![first];
            while self.eat_op(",") {
                if self.is_op(":") || self.is_kw("if") {
                    break;
                }
                items.push(self.or_pattern()?);
            }
            return self.seq_pattern(items);
        }
        Ok(first)
    }

    /// A sequence pattern from items (a `*rest` item is a capture named `*name`).
    fn seq_pattern(&mut self, items: Vec<MP>) -> R<MP> {
        let mut star = None;
        let mut out = Vec::new();
        for it in items {
            match it {
                MP::Capture(n) if n.starts_with('*') => {
                    if star.is_some() {
                        return self.err("multiple starred names in sequence pattern");
                    }
                    let name = &n[1..];
                    star = Some((out.len(), if name == "_" { None } else { Some(name.to_string()) }));
                }
                other => out.push(other),
            }
        }
        Ok(MP::Seq(out, star))
    }

    fn or_pattern(&mut self) -> R<MP> {
        let mut alts = vec![self.closed_pattern()?];
        while self.eat_op("|") {
            alts.push(self.closed_pattern()?);
        }
        let p = if alts.len() == 1 { alts.pop().unwrap() } else { MP::Or(alts) };
        if self.eat_kw("as") {
            let n = self.name()?;
            return Ok(MP::As(Box::new(p), n));
        }
        Ok(p)
    }

    fn closed_pattern(&mut self) -> R<MP> {
        let pos = self.pos();
        match self.peek().clone() {
            PT::Op("*") => {
                self.next();
                let n = self.name()?;
                Ok(MP::Capture(format!("*{}", n)))
            }
            PT::Op("-") | PT::Int(_) | PT::Num(_) | PT::Str(_) => {
                let e = self.arith()?;
                Ok(MP::Value(e))
            }
            PT::Name(n) => {
                self.next();
                match n.as_str() {
                    "_" => return Ok(MP::Wild),
                    "None" => return Ok(MP::Value(Expr::Null)),
                    "True" => return Ok(MP::Value(Expr::Bool(true))),
                    "False" => return Ok(MP::Value(Expr::Bool(false))),
                    _ => {}
                }
                let mut e = Expr::Ident(n.clone(), pos);
                let mut dotted = false;
                while self.eat_op(".") {
                    let m = self.name()?;
                    e = Expr::Member { obj: Box::new(e), name: m, optional: false, pos };
                    dotted = true;
                }
                if self.eat_op("(") {
                    let mut args = Vec::new();
                    let mut kw = Vec::new();
                    while !self.is_op(")") {
                        if matches!(self.peek(), PT::Name(_)) && matches!(self.peek_at(1), PT::Op("=")) {
                            let k = self.name()?;
                            self.next();
                            kw.push((k, self.or_pattern()?));
                        } else {
                            args.push(self.or_pattern()?);
                        }
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                    self.expect_op(")")?;
                    return Ok(MP::Class(e, args, kw));
                }
                Ok(if dotted { MP::Value(e) } else { MP::Capture(n) })
            }
            PT::Op("(") => {
                self.next();
                if self.eat_op(")") {
                    return Ok(MP::Seq(vec![], None));
                }
                let first = self.or_pattern()?;
                if self.is_op(",") {
                    let mut items = vec![first];
                    while self.eat_op(",") {
                        if self.is_op(")") {
                            break;
                        }
                        items.push(self.or_pattern()?);
                    }
                    self.expect_op(")")?;
                    return self.seq_pattern(items);
                }
                self.expect_op(")")?;
                Ok(first)
            }
            PT::Op("[") => {
                self.next();
                let mut items = Vec::new();
                while !self.is_op("]") {
                    items.push(self.or_pattern()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("]")?;
                self.seq_pattern(items)
            }
            PT::Op("{") => {
                self.next();
                let mut entries = Vec::new();
                let mut rest = None;
                while !self.is_op("}") {
                    if self.eat_op("**") {
                        rest = Some(self.name()?);
                    } else {
                        let k = match self.closed_pattern()? {
                            MP::Value(e) => e,
                            _ => return self.err("mapping pattern keys must be literals or dotted names"),
                        };
                        self.expect_op(":")?;
                        entries.push((k, self.or_pattern()?));
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("}")?;
                Ok(MP::Map(entries, rest))
            }
            other => self.err(format!("invalid pattern: unexpected {}", describe(&other))),
        }
    }

    /// `async for x in aiterable: body`: awaits the elements of an async iterator.
    fn async_for(&mut self) -> R<Stmt> {
        let pos = self.pos();
        self.next();
        let target = self.target_list()?;
        if !self.eat_kw("in") {
            return self.err(format!("expected 'in' but found {}", describe(self.peek())));
        }
        let obj = self.testlist()?;
        let body = self.block()?;
        self.tmp += 1;
        let ai = format!("%ai{}", self.tmp);
        let ar = format!("%ar{}", self.tmp);
        let id = |n: &str| Expr::Ident(n.to_string(), pos);
        let member = |o: Expr, n: &str| Expr::Member { obj: Box::new(o), name: n.to_string(), optional: false, pos };
        let assign = |t: Pat, v: Expr| Stmt::Expr(Expr::Assign { op: None, target: Box::new(t), value: Box::new(v), pos });
        let next = Expr::Await(Box::new(Expr::Call { callee: Box::new(member(id(&ai), "next")), args: vec![], optional: false, pos }), pos);
        let mut loop_body = vec![assign(Pat::Id(ar.clone()), next), Stmt::If(member(id(&ar), "done"), Box::new(Stmt::Break(None)), None), assign(target, member(id(&ar), "value"))];
        loop_body.extend(body);
        Ok(Stmt::Block(vec![assign(Pat::Id(ai), call("%asyncIter", vec![obj], pos)), Stmt::While(Expr::Bool(true), Box::new(Stmt::Block(loop_body)), None)]))
    }

    /// `import a.b [as c], d` / `from m import x [as y], ...` / `from m import *`.
    fn import_stmt(&mut self) -> R<Stmt> {
        let pos = self.pos();
        let assign = |t: String, v: Expr| Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(t)), value: Box::new(v), pos });
        let mut out = Vec::new();
        if self.eat_kw("import") {
            loop {
                let mut name = self.name()?;
                while self.eat_op(".") {
                    name.push('.');
                    name.push_str(&self.name()?);
                }
                if self.eat_kw("as") {
                    let alias = self.name()?;
                    out.push(assign(alias, call("%pyimport", vec![Expr::Str(name), Expr::Bool(false)], pos)));
                } else {
                    let top = name.split('.').next().unwrap_or("").to_string();
                    out.push(assign(top, call("%pyimport", vec![Expr::Str(name), Expr::Bool(true)], pos)));
                }
                if !self.eat_op(",") {
                    break;
                }
            }
            return Ok(Stmt::Block(out));
        }
        self.next(); // from
        let mut module = String::new();
        while self.is_op(".") || self.is_op("...") {
            module.push_str(if self.eat_op("...") { "..." } else {
                self.next();
                "."
            });
        }
        if !self.is_kw("import") {
            module.push_str(&self.name()?);
            while self.eat_op(".") {
                module.push('.');
                module.push_str(&self.name()?);
            }
        }
        if !self.eat_kw("import") {
            return self.err(format!("expected 'import' but found {}", describe(self.peek())));
        }
        if module == "__future__" {
            while !self.is(&PT::Newline) && !self.is(&PT::Eof) && !self.is_op(";") {
                self.next();
            }
            return Ok(Stmt::Empty);
        }
        if self.eat_op("*") {
            return Ok(Stmt::Labeled(format!("%importstar:{}", module), Box::new(Stmt::Empty)));
        }
        let paren = self.eat_op("(");
        loop {
            if paren {
                while self.eat(&PT::Newline) {}
                if self.is_op(")") {
                    break;
                }
            }
            let name = self.name()?;
            let alias = if self.eat_kw("as") { self.name()? } else { name.clone() };
            out.push(assign(alias, call("%pyfrom", vec![Expr::Str(module.clone()), Expr::Str(name)], pos)));
            if !self.eat_op(",") {
                break;
            }
        }
        if paren {
            while self.eat(&PT::Newline) {}
            self.expect_op(")")?;
        }
        Ok(Stmt::Block(out))
    }

    /// `def name(params) [-> type]: body` (a generator when the body yields).
    fn def_stmt(&mut self) -> R<FuncDef> {
        self.next();
        let name = self.name()?;
        let (params, rest) = self.params(")")?;
        if self.eat_op("->") {
            self.test()?;
        }
        let saved = std::mem::replace(&mut self.saw_yield, false);
        let body = self.block();
        let generator = std::mem::replace(&mut self.saw_yield, saved);
        Ok(FuncDef { name: Some(name), params, rest, body: FuncBody::Block(body?), arrow: false, implicit_it: false, implicit_return: false, generator, is_async: false })
    }

    /// `@decorator` lines followed by a `def` or `class`.
    fn decorated(&mut self) -> R<Vec<Stmt>> {
        let mut decs = Vec::new();
        while self.eat_op("@") {
            decs.push(self.test()?);
            if !self.eat(&PT::Newline) {
                return self.err(format!("invalid syntax: unexpected {}", describe(self.peek())));
            }
            while self.eat(&PT::Newline) {}
        }
        if self.is_kw("class") {
            let c = self.class_stmt(decs.clone())?;
            return Ok(self.class_with_meta(c, decs));
        }
        let is_async = self.is_kw("async") && matches!(self.peek_at(1), PT::Name(n) if n == "def");
        if is_async {
            self.next();
        }
        if !self.is_kw("def") {
            return self.err(format!("invalid syntax: expected 'def' or 'class' after decorator but found {}", describe(self.peek())));
        }
        let pos = self.pos();
        let mut f = self.def_stmt()?;
        f.is_async = is_async;
        let name = f.name.clone().unwrap_or_default();
        let mut value = Expr::Ident(name.clone(), pos);
        for d in decs.into_iter().rev() {
            value = Expr::Call { callee: Box::new(d), args: vec![Elem::Item(value)], optional: false, pos };
        }
        Ok(vec![Stmt::Func(Arc::new(f)), Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(name)), value: Box::new(value), pos })])
    }

    /// `class Name(Base, ...): body` with methods, class attributes, `@staticmethod`,
    /// `@classmethod`, `@property` / `@x.setter` and `@dataclass`.
    fn class_stmt(&mut self, decorators: Vec<Expr>) -> R<ClassDef> {
        self.next();
        let name = self.name()?;
        let mut bases = Vec::new();
        if self.eat_op("(") {
            while !self.is_op(")") {
                if matches!(self.peek(), PT::Name(_)) && matches!(self.toks.get(self.i + 1).map(|t| &t.t), Some(PT::Op("="))) {
                    // metaclass=... keyword (other class keywords are ignored)
                    let k = self.name()?;
                    self.next();
                    let v = self.test()?;
                    if k == "metaclass" {
                        self.meta = Some(v);
                    }
                } else {
                    let b = self.test()?;
                    if !matches!(&b, Expr::Ident(n, _) if n == "object") {
                        bases.push(b);
                    }
                }
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op(")")?;
        }
        let saved = std::mem::replace(&mut self.saw_yield, false);
        let body = self.block();
        self.saw_yield = saved;
        let body = body?;
        let mut methods: Vec<ClassMember> = Vec::new();
        let mut fields: Vec<ClassField> = Vec::new();
        let mut data_fields: Vec<(String, Option<Expr>)> = Vec::new();
        let mut i = 0;
        while i < body.len() {
            match &body[i] {
                Stmt::Func(f) => {
                    let fname = f.name.clone().unwrap_or_default();
                    // decorator applications emitted by `decorated`
                    let mut kind = MemberKind::Method;
                    let mut is_static = false;
                    let mut mname = fname.clone();
                    while let Some(Stmt::Expr(Expr::Assign { op: None, target, value, .. })) = body.get(i + 1) {
                        if !matches!(&**target, Pat::Id(n) if *n == fname) {
                            break;
                        }
                        let mut v: &Expr = value;
                        let mut decs = Vec::new();
                        while let Expr::Call { callee, args, .. } = v {
                            decs.push((**callee).clone());
                            match args.first() {
                                Some(Elem::Item(a)) => v = a,
                                _ => break,
                            }
                        }
                        for d in decs {
                            match &d {
                                Expr::Ident(n, _) if n == "staticmethod" => is_static = true,
                                Expr::Ident(n, _) if n == "classmethod" => kind = MemberKind::ClassMethod,
                                Expr::Ident(n, _) if n == "property" || n.ends_with("cached_property") => kind = MemberKind::Getter,
                                Expr::Member { obj, name: m, .. } if m == "setter" || m == "getter" => {
                                    if let Expr::Ident(p, _) = &**obj {
                                        mname = p.clone();
                                    }
                                    kind = if m == "setter" { MemberKind::Setter } else { MemberKind::Getter };
                                }
                                Expr::Member { name: m, .. } if m == "cached_property" => kind = MemberKind::Getter,
                                _ => {}
                            }
                        }
                        i += 1;
                    }
                    methods.push(ClassMember { name: mname, key: None, func: f.clone(), kind, is_static });
                }
                Stmt::Expr(Expr::Assign { op: None, target, value, .. }) => match &**target {
                    Pat::Id(n) => fields.push(ClassField { name: n.clone(), init: Some((**value).clone()), is_static: true }),
                    _ => return self.err("unsupported class attribute assignment"),
                },
                Stmt::Labeled(l, inner) if l.starts_with("%annot:") => {
                    let n = l["%annot:".len()..].to_string();
                    let init = match &**inner {
                        Stmt::Expr(Expr::Assign { value, .. }) => Some((**value).clone()),
                        _ => None,
                    };
                    if let Some(e) = &init {
                        fields.push(ClassField { name: n.clone(), init: Some(e.clone()), is_static: true });
                    }
                    data_fields.push((n, init));
                }
                Stmt::Class(c) => fields.push(ClassField { name: c.name.clone().unwrap_or_default(), init: Some(Expr::Class(c.clone())), is_static: true }),
                Stmt::Empty | Stmt::Expr(Expr::Str(_)) | Stmt::Expr(Expr::Template(_)) => {}
                _ => return self.err("unsupported statement in class body"),
            }
            i += 1;
        }
        let dataclass = decorators.iter().any(|d| match d {
            Expr::Ident(n, _) => n == "dataclass",
            Expr::Member { name, .. } => name == "dataclass",
            Expr::Call { callee, .. } => matches!(&**callee, Expr::Ident(n, _) if n == "dataclass") || matches!(&**callee, Expr::Member { name, .. } if name == "dataclass"),
            _ => false,
        });
        let mut annotations = vec![];
        if dataclass {
            let opts: Vec<(String, Expr)> = decorators
                .iter()
                .find_map(|d| match d {
                    Expr::Call { callee, args, .. } if matches!(&**callee, Expr::Ident(n, _) if n == "dataclass") || matches!(&**callee, Expr::Member { name, .. } if name == "dataclass") => Some(kwargs_of(args)),
                    _ => None,
                })
                .unwrap_or_default();
            self.add_dataclass_methods(&name, &data_fields, &mut methods, &mut fields, &mut annotations, &opts)?;
        }
        let mut bases = bases.into_iter();
        let extends = bases.next();
        Ok(ClassDef { name: Some(name), extends, ctors: vec![], methods, fields, mixins: bases.collect(), style: ClassStyle::Python, annotations })
    }

    /// `@dataclass(frozen, order, eq, repr, init)`: `__init__` (with `field(default=,
    /// default_factory=, init=, repr=, compare=)` options and `__post_init__`), `__repr__`,
    /// `__eq__`, ordering methods, `__dataclass_fields__` and `__match_args__`.
    #[allow(clippy::too_many_arguments)]
    fn add_dataclass_methods(&mut self, name: &str, data: &[(String, Option<Expr>)], methods: &mut Vec<ClassMember>, fields: &mut Vec<ClassField>, annotations: &mut Vec<String>, opts: &[(String, Expr)]) -> R<()> {
        struct F {
            name: String,
            default: Option<Expr>,
            factory: Option<Expr>,
            init: bool,
            repr: bool,
            compare: bool,
        }
        let flag = |k: &str, def: bool| opts.iter().find(|(n, _)| n == k).map(|(_, v)| !matches!(v, Expr::Bool(false))).unwrap_or(def);
        let mut fs = Vec::new();
        for (n, d) in data {
            let mut f = F { name: n.clone(), default: d.clone(), factory: None, init: true, repr: true, compare: true };
            if let Some(Expr::Call { callee, args, .. }) = d {
                if matches!(&**callee, Expr::Ident(c, _) if c == "field") || matches!(&**callee, Expr::Member { name, .. } if name == "field") {
                    f.default = None;
                    for (k, v) in kwargs_of(args) {
                        match k.as_str() {
                            "default" => f.default = Some(v),
                            "default_factory" => f.factory = Some(v),
                            "init" => f.init = !matches!(v, Expr::Bool(false)),
                            "repr" => f.repr = !matches!(v, Expr::Bool(false)),
                            "compare" => f.compare = !matches!(v, Expr::Bool(false)),
                            _ => {}
                        }
                    }
                }
            }
            fs.push(f);
        }
        let pos = self.pos();
        let id = |n: &str| Expr::Ident(n.to_string(), pos);
        let self_attr = |n: &str| Expr::Member { obj: Box::new(id("self")), name: n.to_string(), optional: false, pos };
        if flag("init", true) && !methods.iter().any(|m| m.name == "__init__") {
            let mut params = vec![Param { pat: Pat::Id("self".into()), default: None, ty: None }];
            let mut body = Vec::new();
            for f in &fs {
                let value = if f.init {
                    params.push(Param { pat: Pat::Id(f.name.clone()), default: if f.factory.is_some() { Some(Expr::Undef) } else { f.default.clone() }, ty: None });
                    match &f.factory {
                        Some(fac) => Expr::Cond(Box::new(Expr::Bin(BinOp::Is, Box::new(id(&f.name)), Box::new(Expr::Undef), pos)), Box::new(Expr::Call { callee: Box::new(fac.clone()), args: vec![], optional: false, pos }), Box::new(id(&f.name))),
                        None => id(&f.name),
                    }
                } else {
                    match (&f.factory, &f.default) {
                        (Some(fac), _) => Expr::Call { callee: Box::new(fac.clone()), args: vec![], optional: false, pos },
                        (None, Some(d)) => d.clone(),
                        (None, None) => continue,
                    }
                };
                body.push(Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Target(self_attr(&f.name))), value: Box::new(value), pos }));
            }
            if methods.iter().any(|m| m.name == "__post_init__") {
                body.push(Stmt::Expr(Expr::Call { callee: Box::new(self_attr("__post_init__")), args: vec![], optional: false, pos }));
            }
            let f = FuncDef { name: Some("__init__".into()), params, rest: None, body: FuncBody::Block(body), arrow: false, implicit_it: false, implicit_return: false, generator: false, is_async: false };
            methods.push(ClassMember { name: "__init__".into(), key: None, func: Arc::new(f), kind: MemberKind::Method, is_static: false });
        }
        let repr = fs.iter().filter(|f| f.repr).map(|f| format!("'{0}=' + repr(self.{0})", f.name)).collect::<Vec<_>>().join(" + ', ' + ");
        let cmp: Vec<&F> = fs.iter().filter(|f| f.compare).collect();
        let mine = cmp.iter().map(|f| format!("self.{}", f.name)).collect::<Vec<_>>().join(", ");
        let theirs = cmp.iter().map(|f| format!("other.{}", f.name)).collect::<Vec<_>>().join(", ");
        let mut src = format!("class {}:\n    pass\n", name);
        if flag("repr", true) {
            src.push_str(&format!("    def __repr__(self):\n        return '{}(' + {} + ')'\n", name, if repr.is_empty() { "''".to_string() } else { repr }));
        }
        if flag("eq", true) {
            src.push_str(&format!("    def __eq__(self, other):\n        return type(other) is type(self) and [{}] == [{}]\n", mine, theirs));
        }
        if flag("order", false) {
            for (m, op) in [("__lt__", "<"), ("__le__", "<="), ("__gt__", ">"), ("__ge__", ">=")] {
                src.push_str(&format!("    def {}(self, other):\n        return [{}] {} [{}]\n", m, mine, op, theirs));
            }
        }
        let prog = parse_program(&src)?;
        if let Some(Stmt::Class(c)) = prog.body.into_iter().next() {
            for m in &c.methods {
                if !methods.iter().any(|x| x.name == m.name) {
                    methods.push(m.clone());
                }
            }
        }
        // remove `x: T = field(...)` class attributes (the instance values are set by __init__)
        fields.retain(|cf| !fs.iter().any(|f| f.name == cf.name && (f.factory.is_some() || matches!(&f.default, Some(Expr::Call { .. })) && f.default.is_none())));
        for f in &fs {
            if f.factory.is_some() {
                fields.retain(|cf| cf.name != f.name);
            } else if let Some(d) = &f.default {
                if let Some(cf) = fields.iter_mut().find(|cf| cf.name == f.name) {
                    cf.init = Some(d.clone());
                }
            }
        }
        let names = |pred: &dyn Fn(&F) -> bool| Expr::Array(fs.iter().filter(|f| pred(f)).map(|f| Elem::Item(Expr::Str(f.name.clone()))).collect());
        fields.push(ClassField { name: "__dataclass_fields__".into(), init: Some(names(&|_| true)), is_static: true });
        fields.push(ClassField { name: "__match_args__".into(), init: Some(names(&|f| f.init)), is_static: true });
        if flag("frozen", false) {
            annotations.push("frozen".into());
        }
        Ok(())
    }

    /// `with a as x, b as y: body`: `__enter__` / `__exit__` around the body (`async with`:
    /// awaited `__aenter__` / `__aexit__`).
    fn with_stmt(&mut self, is_async: bool) -> R<Stmt> {
        let (enter, exit) = if is_async { ("__aenter__", "__aexit__") } else { ("__enter__", "__exit__") };
        let pos = self.pos();
        self.next();
        let mut items = Vec::new();
        loop {
            let e = self.test()?;
            let target = if self.eat_kw("as") {
                let t = self.or_expr()?;
                Some(self.to_pat(t)?)
            } else {
                None
            };
            items.push((e, target));
            if !self.eat_op(",") {
                break;
            }
        }
        let mut body = self.block()?;
        for (k, (e, target)) in items.into_iter().enumerate().rev() {
            let m = format!("%with{}", k);
            let ok = format!("%withok{}", k);
            let ex = format!("%withexc{}", k);
            let id = |n: &str| Expr::Ident(n.to_string(), pos);
            let call = |obj: Expr, name: &str, args: Vec<Expr>| {
                let name = match name {
                    "__enter__" => enter,
                    "__exit__" => exit,
                    other => other,
                };
                let c = Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(obj), name: name.into(), optional: false, pos }), args: args.into_iter().map(Elem::Item).collect(), optional: false, pos };
                if is_async {
                    Expr::Await(Box::new(c), pos)
                } else {
                    c
                }
            };
            let assign = |t: Pat, v: Expr| Stmt::Expr(Expr::Assign { op: None, target: Box::new(t), value: Box::new(v), pos });
            let mut stmts = vec![assign(Pat::Id(m.clone()), e)];
            let entered = call(id(&m), "__enter__", vec![]);
            match target {
                Some(t) => stmts.push(assign(t, entered)),
                None => stmts.push(Stmt::Expr(entered)),
            }
            stmts.push(assign(Pat::Id(ok.clone()), Expr::Bool(true)));
            let type_of = Expr::Call { callee: Box::new(id("type")), args: vec![Elem::Item(id(&ex))], optional: false, pos };
            let handler = vec![
                assign(Pat::Id(ok.clone()), Expr::Bool(false)),
                Stmt::If(Expr::Unary(UnOp::Not, Box::new(call(id(&m), "__exit__", vec![type_of, id(&ex), Expr::Null])), pos), Box::new(Stmt::Throw(id(&ex), pos)), None),
            ];
            stmts.push(Stmt::Try {
                block: body,
                catches: vec![Catch { types: vec![], param: Some(Pat::Id(ex.clone())), body: handler }],
                finally: Some(vec![Stmt::If(id(&ok), Box::new(Stmt::Expr(call(id(&m), "__exit__", vec![Expr::Null, Expr::Null, Expr::Null]))), None)]),
            });
            body = stmts;
        }
        Ok(Stmt::Block(body))
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        self.expect_op(":")?;
        if self.eat(&PT::Newline) {
            if !self.eat(&PT::Indent) {
                return self.err("expected an indented block");
            }
            let mut body = Vec::new();
            while !self.eat(&PT::Dedent) {
                if self.is(&PT::Eof) {
                    break;
                }
                if self.eat(&PT::Newline) {
                    continue;
                }
                body.extend(self.statement()?);
            }
            return Ok(body);
        }
        // single-line block
        let mut body = vec![self.simple_stmt()?];
        while self.eat_op(";") {
            if self.is(&PT::Newline) {
                break;
            }
            body.push(self.simple_stmt()?);
        }
        if !self.eat(&PT::Newline) && !self.is(&PT::Eof) {
            return self.err(format!("invalid syntax: unexpected {}", describe(self.peek())));
        }
        Ok(body)
    }

    fn if_stmt(&mut self) -> R<Stmt> {
        self.next(); // if / elif
        let c = self.test()?;
        let body = self.block()?;
        let els = if self.is_kw("elif") {
            Some(Box::new(self.if_stmt()?))
        } else if self.eat_kw("else") {
            Some(Box::new(Stmt::Block(self.block()?)))
        } else {
            None
        };
        Ok(Stmt::If(c, Box::new(Stmt::Block(body)), els))
    }

    fn try_stmt(&mut self) -> R<Stmt> {
        self.next();
        let mut block = self.block()?;
        let mut catches = Vec::new();
        while self.eat_kw("except") {
            let mut types = Vec::new();
            let mut param = None;
            if !self.is_op(":") {
                let t = self.test()?;
                match t {
                    Expr::Ident(n, _) => types.push(n),
                    Expr::Array(items) => {
                        for it in items {
                            if let Elem::Item(Expr::Ident(n, _)) = it {
                                types.push(n);
                            }
                        }
                    }
                    Expr::Member { name, .. } => types.push(name),
                    _ => return self.err("invalid exception type"),
                }
                if self.eat_kw("as") {
                    param = Some(Pat::Id(self.name()?));
                }
            }
            let body = self.block()?;
            catches.push(Catch { types, param, body });
        }
        if self.eat_kw("else") {
            block.extend(self.block()?);
        }
        let finally = if self.eat_kw("finally") { Some(self.block()?) } else { None };
        if catches.is_empty() && finally.is_none() {
            return self.err("expected 'except' or 'finally' block");
        }
        Ok(Stmt::Try { block, catches, finally })
    }

    fn simple_stmt(&mut self) -> R<Stmt> {
        let pos = self.pos();
        if let PT::Name(k) = self.peek().clone() {
            match k.as_str() {
                "pass" => {
                    self.next();
                    return Ok(Stmt::Empty);
                }
                "break" => {
                    self.next();
                    return Ok(Stmt::Break(None));
                }
                "continue" => {
                    self.next();
                    return Ok(Stmt::Continue(None));
                }
                "return" => {
                    self.next();
                    if self.is(&PT::Newline) || self.is_op(";") || self.is(&PT::Eof) || self.is(&PT::Dedent) {
                        return Ok(Stmt::Return(None));
                    }
                    return Ok(Stmt::Return(Some(self.testlist()?)));
                }
                "raise" => {
                    self.next();
                    if self.is(&PT::Newline) || self.is(&PT::Eof) {
                        return Ok(Stmt::Throw(Expr::Ident("__reraise".into(), pos), pos));
                    }
                    let e = self.test()?;
                    if self.eat_kw("from") {
                        self.test()?;
                    }
                    return Ok(Stmt::Throw(e, pos));
                }
                "global" | "nonlocal" => {
                    self.next();
                    let mut names = vec![self.name()?];
                    while self.eat_op(",") {
                        names.push(self.name()?);
                    }
                    return Ok(Stmt::Global(if k == "global" { names } else { names.into_iter().map(|n| format!("%nonlocal:{}", n)).collect() }));
                }
                "import" | "from" => return self.import_stmt(),
                "assert" => {
                    self.next();
                    let e = self.test()?;
                    let msg = if self.eat_op(",") { Some(self.test()?) } else { None };
                    return Ok(Stmt::Assert(e, msg, pos));
                }
                "del" => {
                    self.next();
                    let t = self.test()?;
                    return Ok(Stmt::Expr(Expr::Unary(UnOp::Delete, Box::new(t), pos)));
                }
                _ => {}
            }
        }
        let first = self.testlist_star()?;
        // annotated assignment `x: int = 1`
        if self.is_op(":") {
            self.next();
            self.test()?;
            let label = match &first {
                Expr::Ident(n, _) => format!("%annot:{}", n),
                _ => "%annot".to_string(),
            };
            if !self.eat_op("=") {
                return Ok(Stmt::Labeled(label, Box::new(Stmt::Empty)));
            }
            let v = self.yield_or_testlist()?;
            return Ok(Stmt::Labeled(label, Box::new(Stmt::Expr(Expr::Assign { op: None, target: Box::new(self.to_pat(first)?), value: Box::new(v), pos }))));
        }
        let aug = match self.peek() {
            PT::Op("+=") => Some(BinOp::Add),
            PT::Op("-=") => Some(BinOp::Sub),
            PT::Op("*=") => Some(BinOp::Mul),
            PT::Op("/=") => Some(BinOp::Div),
            PT::Op("//=") => Some(BinOp::FloorDiv),
            PT::Op("%=") => Some(BinOp::Mod),
            PT::Op("**=") => Some(BinOp::Pow),
            PT::Op("&=") => Some(BinOp::BitAnd),
            PT::Op("|=") => Some(BinOp::BitOr),
            PT::Op("^=") => Some(BinOp::BitXor),
            PT::Op("<<=") => Some(BinOp::Shl),
            PT::Op(">>=") => Some(BinOp::Shr),
            _ => None,
        };
        if let Some(op) = aug {
            self.next();
            let v = self.yield_or_testlist()?;
            return Ok(Stmt::Expr(Expr::Assign { op: Some(AssignOp::Bin(op)), target: Box::new(self.to_pat(first)?), value: Box::new(v), pos }));
        }
        if self.is_op("=") {
            let mut targets = vec![first];
            let mut value;
            loop {
                self.next();
                value = if self.is_kw("yield") { self.yield_expr()? } else { self.testlist_star()? };
                if self.is_op("=") {
                    targets.push(value);
                    continue;
                }
                break;
            }
            // a = b = value: assign right to left
            let mut e = value;
            for t in targets.into_iter().rev() {
                e = Expr::Assign { op: None, target: Box::new(self.to_pat(t)?), value: Box::new(e), pos };
            }
            return Ok(Stmt::Expr(e));
        }
        Ok(Stmt::Expr(first))
    }

    fn to_pat(&self, e: Expr) -> R<Pat> {
        Ok(match e {
            Expr::Ident(n, _) => Pat::Id(n),
            Expr::Member { .. } | Expr::Index { .. } => Pat::Target(e),
            Expr::Array(items) => {
                let mut v = Vec::new();
                let mut rest = None;
                for it in items {
                    match it {
                        Elem::Item(e) => v.push(Some((self.to_pat(e)?, None))),
                        Elem::Spread(e) => rest = Some(Box::new(self.to_pat(e)?)),
                        Elem::Hole => v.push(None),
                    }
                }
                Pat::Arr(v, rest)
            }
            _ => return self.err("cannot assign to expression"),
        })
    }

    fn target_list(&mut self) -> R<Pat> {
        let first = self.star_or(Self::or_expr)?;
        if self.is_op(",") {
            let mut items = vec![first];
            while self.eat_op(",") {
                if self.is_kw("in") || self.is_op("=") {
                    break;
                }
                items.push(self.star_or(Self::or_expr)?);
            }
            return self.to_pat(Expr::Array(items.into_iter().map(Elem::Item).collect()));
        }
        self.to_pat(first)
    }

    fn star_or(&mut self, f: fn(&mut P) -> R<Expr>) -> R<Expr> {
        f(self)
    }

    fn params(&mut self, close: &str) -> R<(Vec<Param>, Option<Pat>)> {
        if close == ")" {
            self.expect_op("(")?;
        }
        let mut params = Vec::new();
        let mut rest = None;
        let mut kwonly = false;
        while !self.is_op(close) {
            if self.eat_op("**") {
                let n = self.name()?;
                if close == ")" && self.eat_op(":") {
                    self.test()?;
                }
                params.push(Param { pat: Pat::Id(n), default: Some(Expr::Object(vec![])), ty: Some("**".into()) });
            } else if self.eat_op("*") {
                kwonly = true;
                if let PT::Name(_) = self.peek() {
                    rest = Some(Pat::Id(self.name()?));
                    if close == ")" && self.eat_op(":") {
                        self.test()?;
                    }
                }
            } else if self.eat_op("/") {
                // positional-only marker
            } else {
                let n = self.name()?;
                if close == ")" && self.eat_op(":") {
                    self.test()?;
                }
                let default = if self.eat_op("=") { Some(self.test()?) } else { None };
                params.push(Param { pat: Pat::Id(n), default, ty: if kwonly { Some("kwonly".into()) } else { None } });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(close)?;
        Ok((params, rest))
    }

    // ------------------------------------------------------------------ expressions
    fn testlist(&mut self) -> R<Expr> {
        let first = self.test()?;
        if self.is_op(",") {
            let mut items = vec![Elem::Item(first)];
            while self.eat_op(",") {
                if self.ends_list() {
                    break;
                }
                items.push(Elem::Item(self.test()?));
            }
            return Ok(Expr::Array(items));
        }
        Ok(first)
    }

    fn ends_list(&self) -> bool {
        matches!(self.peek(), PT::Newline | PT::Eof | PT::Op("=") | PT::Op(")") | PT::Op("]") | PT::Op("}") | PT::Op(";") | PT::Op(":"))
    }

    fn testlist_star(&mut self) -> R<Expr> {
        let first = self.star_test()?;
        if self.is_op(",") {
            let mut items = vec![first];
            while self.eat_op(",") {
                if self.ends_list() {
                    break;
                }
                items.push(self.star_test()?);
            }
            return Ok(Expr::Array(items));
        }
        Ok(match first {
            Elem::Item(e) => e,
            Elem::Spread(e) => Expr::Array(vec![Elem::Spread(e)]),
            Elem::Hole => Expr::Null,
        })
    }

    fn star_test(&mut self) -> R<Elem> {
        if self.eat_op("*") {
            return Ok(Elem::Spread(self.or_expr()?));
        }
        Ok(Elem::Item(self.test()?))
    }

    fn yield_or_testlist(&mut self) -> R<Expr> {
        if self.is_kw("yield") {
            return self.yield_expr();
        }
        self.testlist()
    }

    /// `yield`, `yield value`, `yield a, b`, `yield from iterable`.
    fn yield_expr(&mut self) -> R<Expr> {
        let pos = self.pos();
        self.next();
        self.saw_yield = true;
        if self.eat_kw("from") {
            let e = self.test()?;
            return Ok(Expr::Yield { arg: Some(Box::new(e)), delegate: true, pos });
        }
        if matches!(self.peek(), PT::Newline | PT::Eof | PT::Dedent | PT::Op(")") | PT::Op(";") | PT::Op("]") | PT::Op("}")) {
            return Ok(Expr::Yield { arg: None, delegate: false, pos });
        }
        let e = self.testlist()?;
        Ok(Expr::Yield { arg: Some(Box::new(e)), delegate: false, pos })
    }

    fn test(&mut self) -> R<Expr> {
        if self.is_kw("yield") {
            return self.yield_expr();
        }
        if self.is_kw("lambda") {
            self.next();
            let (params, rest) = self.params(":")?;
            let body = self.test()?;
            return Ok(Expr::Func(Arc::new(FuncDef { name: None, params, rest, body: FuncBody::Expr(Box::new(body)), arrow: true, implicit_it: false, implicit_return: false, generator: false, is_async: false })));
        }
        if let (PT::Name(n), PT::Op(":=")) = (self.peek().clone(), self.peek_at(1).clone()) {
            // assignment expression `name := value`
            let pos = self.pos();
            self.next();
            self.next();
            let v = self.test()?;
            return Ok(Expr::Assign { op: None, target: Box::new(Pat::Id(n)), value: Box::new(v), pos });
        }
        let c = self.or_test()?;
        if self.is_kw("if") {
            // only a conditional expression when followed by `else`
            let save = self.i;
            self.next();
            let cond = self.or_test()?;
            if self.eat_kw("else") {
                let other = self.test()?;
                return Ok(Expr::Cond(Box::new(cond), Box::new(c), Box::new(other)));
            }
            self.i = save;
        }
        Ok(c)
    }

    fn or_test(&mut self) -> R<Expr> {
        let mut l = self.and_test()?;
        while self.eat_kw("or") {
            let r = self.and_test()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_test(&mut self) -> R<Expr> {
        let mut l = self.not_test()?;
        while self.eat_kw("and") {
            let r = self.not_test()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn not_test(&mut self) -> R<Expr> {
        let pos = self.pos();
        if self.eat_kw("not") {
            let e = self.not_test()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(e), pos));
        }
        self.comparison()
    }

    fn comp_op(&mut self) -> Option<BinOp> {
        let op = match self.peek() {
            PT::Op("<") => BinOp::Lt,
            PT::Op(">") => BinOp::Gt,
            PT::Op("==") => BinOp::Eq,
            PT::Op(">=") => BinOp::Ge,
            PT::Op("<=") => BinOp::Le,
            PT::Op("!=") => BinOp::Ne,
            PT::Name(n) if n == "in" => BinOp::In,
            PT::Name(n) if n == "not" && matches!(self.peek_at(1), PT::Name(x) if x == "in") => {
                self.next();
                BinOp::NotIn
            }
            PT::Name(n) if n == "is" => {
                if matches!(self.peek_at(1), PT::Name(x) if x == "not") {
                    self.next();
                    BinOp::IsNot
                } else {
                    BinOp::Is
                }
            }
            _ => return None,
        };
        self.next();
        Some(op)
    }

    fn comparison(&mut self) -> R<Expr> {
        let first = self.or_expr()?;
        let mut pos = self.pos();
        let mut parts: Vec<Expr> = Vec::new();
        let mut left = first.clone();
        let mut result: Option<Expr> = None;
        while let Some(op) = self.comp_op() {
            let right = self.or_expr()?;
            let cmp = Expr::Bin(op, Box::new(left), Box::new(right.clone()), pos);
            result = Some(match result {
                None => cmp,
                Some(prev) => Expr::And(Box::new(prev), Box::new(cmp)),
            });
            left = right;
            pos = self.pos();
            parts.push(Expr::Null);
        }
        Ok(result.unwrap_or(first))
    }

    fn or_expr(&mut self) -> R<Expr> {
        let mut l = self.xor_expr()?;
        while self.is_op("|") {
            let pos = self.pos();
            self.next();
            let r = self.xor_expr()?;
            l = Expr::Bin(BinOp::BitOr, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn xor_expr(&mut self) -> R<Expr> {
        let mut l = self.and_expr()?;
        while self.is_op("^") {
            let pos = self.pos();
            self.next();
            let r = self.and_expr()?;
            l = Expr::Bin(BinOp::BitXor, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> R<Expr> {
        let mut l = self.shift_expr()?;
        while self.is_op("&") {
            let pos = self.pos();
            self.next();
            let r = self.shift_expr()?;
            l = Expr::Bin(BinOp::BitAnd, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn shift_expr(&mut self) -> R<Expr> {
        let mut l = self.arith()?;
        loop {
            let op = match self.peek() {
                PT::Op("<<") => BinOp::Shl,
                PT::Op(">>") => BinOp::Shr,
                _ => break,
            };
            let pos = self.pos();
            self.next();
            let r = self.arith()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn arith(&mut self) -> R<Expr> {
        let mut l = self.term()?;
        loop {
            let op = match self.peek() {
                PT::Op("+") => BinOp::Add,
                PT::Op("-") => BinOp::Sub,
                _ => break,
            };
            let pos = self.pos();
            self.next();
            let r = self.term()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn term(&mut self) -> R<Expr> {
        let mut l = self.factor()?;
        loop {
            let op = match self.peek() {
                PT::Op("*") => BinOp::Mul,
                PT::Op("/") => BinOp::Div,
                PT::Op("//") => BinOp::FloorDiv,
                PT::Op("%") => BinOp::Mod,
                _ => break,
            };
            let pos = self.pos();
            self.next();
            let r = self.factor()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r), pos);
        }
        Ok(l)
    }

    fn factor(&mut self) -> R<Expr> {
        let pos = self.pos();
        let op = match self.peek() {
            PT::Op("-") => Some(UnOp::Neg),
            PT::Op("+") => Some(UnOp::Plus),
            PT::Op("~") => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let e = self.factor()?;
            return Ok(match (op, e) {
                (UnOp::Neg, Expr::Int(i)) => Expr::Int(-i),
                (UnOp::Neg, Expr::Num(f)) => Expr::Num(-f),
                (op, e) => Expr::Unary(op, Box::new(e), pos),
            });
        }
        self.power()
    }

    fn power(&mut self) -> R<Expr> {
        let base = if self.is_kw("await") && !matches!(self.peek_at(1), PT::Op("=") | PT::Op(".") | PT::Op(")") | PT::Op(",")) {
            let pos = self.pos();
            self.next();
            Expr::Await(Box::new(self.atom_expr()?), pos)
        } else {
            self.atom_expr()?
        };
        if self.is_op("**") {
            let pos = self.pos();
            self.next();
            let exp = self.factor()?;
            return Ok(Expr::Bin(BinOp::Pow, Box::new(base), Box::new(exp), pos));
        }
        Ok(base)
    }

    fn call_args(&mut self) -> R<Vec<Elem>> {
        let mut args = Vec::new();
        let mut kw: Vec<Prop> = Vec::new();
        while !self.is_op(")") {
            if self.eat_op("**") {
                kw.push(Prop::Spread(self.test()?));
            } else if self.eat_op("*") {
                args.push(Elem::Spread(self.test()?));
            } else if matches!(self.peek(), PT::Name(_)) && matches!(self.peek_at(1), PT::Op("=")) {
                let n = self.name()?;
                self.next();
                kw.push(Prop::KV(PropKey::Name(n), self.test()?));
            } else {
                let e = self.test()?;
                if self.is_kw("for") {
                    // generator expression argument
                    let clauses = self.comp_clauses()?;
                    args.push(Elem::Item(Expr::Comp { kind: CompKind::List, key: Box::new(e), value: None, clauses }));
                } else {
                    args.push(Elem::Item(e));
                }
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        if !kw.is_empty() {
            let pos = self.pos();
            args.push(Elem::Item(Expr::Call { callee: Box::new(Expr::Ident("__kwargs".into(), pos)), args: vec![Elem::Item(Expr::Object(kw))], optional: false, pos }));
        }
        Ok(args)
    }

    fn atom_expr(&mut self) -> R<Expr> {
        let mut e = self.atom()?;
        loop {
            let pos = self.pos();
            if self.eat_op("(") {
                let args = self.call_args()?;
                e = Expr::Call { callee: Box::new(e), args, optional: false, pos };
            } else if self.eat_op("[") {
                e = self.subscript(e, pos)?;
            } else if self.eat_op(".") {
                let name = self.name()?;
                e = Expr::Member { obj: Box::new(e), name, optional: false, pos };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn subscript(&mut self, obj: Expr, pos: u32) -> R<Expr> {
        let mut parts: [Option<Box<Expr>>; 3] = [None, None, None];
        let mut idx = 0;
        let mut is_slice = false;
        loop {
            if self.is_op(":") {
                is_slice = true;
                self.next();
                idx += 1;
                if idx > 2 {
                    return self.err("invalid slice");
                }
                continue;
            }
            if self.is_op("]") {
                break;
            }
            parts[idx] = Some(Box::new(self.test()?));
            if self.is_op(",") {
                // tuple index a[1, 2]
                let first = parts[idx].take().unwrap();
                let mut items = vec![Elem::Item(*first)];
                while self.eat_op(",") {
                    if self.is_op("]") {
                        break;
                    }
                    items.push(Elem::Item(self.test()?));
                }
                parts[idx] = Some(Box::new(Expr::Array(items)));
            }
        }
        self.expect_op("]")?;
        if is_slice {
            let [a, b, c] = parts;
            return Ok(Expr::Slice { obj: Box::new(obj), start: a, end: b, step: c, pos });
        }
        match parts[0].take() {
            Some(i) => Ok(Expr::Index { obj: Box::new(obj), index: i, optional: false, pos }),
            None => self.err("invalid subscript"),
        }
    }

    fn comp_clauses(&mut self) -> R<Vec<CompClause>> {
        let mut clauses = Vec::new();
        loop {
            if self.eat_kw("for") {
                let t = self.target_list()?;
                if !self.eat_kw("in") {
                    return self.err("expected 'in' in comprehension");
                }
                let it = self.or_test()?;
                clauses.push(CompClause::For(t, it));
            } else if self.eat_kw("if") {
                let c = self.or_test()?;
                clauses.push(CompClause::If(c));
            } else {
                break;
            }
        }
        Ok(clauses)
    }

    fn fstring(&mut self, parts: Vec<(bool, String, usize)>) -> R<Vec<TplPart>> {
        let mut out = Vec::new();
        for (code, text, off) in parts {
            if !code {
                out.push(TplPart::Lit(text));
                continue;
            }
            // split `expr!r:spec`
            let b = text.as_bytes();
            let mut depth = 0;
            let mut cut = text.len();
            let mut conv = None;
            let mut spec = None;
            let mut k = 0;
            while k < b.len() {
                match b[k] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth -= 1,
                    b'\'' | b'"' => {
                        let q = b[k];
                        k += 1;
                        while k < b.len() && b[k] != q {
                            k += 1;
                        }
                    }
                    b'!' if depth == 0 && b.get(k + 1) != Some(&b'=') => {
                        cut = cut.min(k);
                        conv = text[k + 1..].chars().next();
                        if let Some(c) = text[k..].find(':') {
                            spec = Some(text[k + c + 1..].to_string());
                        }
                        break;
                    }
                    b':' if depth == 0 => {
                        cut = cut.min(k);
                        spec = Some(text[k + 1..].to_string());
                        break;
                    }
                    _ => {}
                }
                k += 1;
            }
            // self-documenting `{expr=}`: the expression text, then its repr (or formatted value)
            let head = &text[..cut];
            let trimmed = head.trim_end();
            if trimmed.ends_with('=') && !trimmed.ends_with("==") && !trimmed.ends_with("!=") && !trimmed.ends_with("<=") && !trimmed.ends_with(">=") {
                out.push(TplPart::Lit(head.to_string()));
                let e = parse_embedded(&trimmed[..trimmed.len() - 1], off)?;
                let conv = if conv.is_none() && spec.is_none() { Some('r') } else { conv };
                out.push(TplPart::Expr(e, spec, conv));
                continue;
            }
            let e = parse_embedded(&text[..cut], off)?;
            out.push(TplPart::Expr(e, spec, conv));
        }
        Ok(out)
    }

    fn atom(&mut self) -> R<Expr> {
        let pos = self.pos();
        match self.peek().clone() {
            PT::Int(i) => {
                self.next();
                Ok(Expr::Int(i))
            }
            PT::Num(f) => {
                self.next();
                Ok(Expr::Num(f))
            }
            PT::Str(_) | PT::FStr(_) => {
                // adjacent literals concatenate
                let mut parts: Vec<TplPart> = Vec::new();
                let mut plain = true;
                while let PT::Str(_) | PT::FStr(_) = self.peek() {
                    match self.next() {
                        PT::Str(s) => parts.push(TplPart::Lit(s)),
                        PT::FStr(p) => {
                            plain = false;
                            parts.extend(self.fstring(p)?);
                        }
                        _ => unreachable!(),
                    }
                }
                if plain {
                    let s: String = parts.into_iter().map(|p| if let TplPart::Lit(s) = p { s } else { String::new() }).collect();
                    return Ok(Expr::Str(s));
                }
                Ok(Expr::Template(parts))
            }
            PT::Name(n) => {
                self.next();
                Ok(match n.as_str() {
                    "None" => Expr::Null,
                    "True" => Expr::Bool(true),
                    "False" => Expr::Bool(false),
                    "super" if self.is_op("(") && matches!(self.toks.get(self.i + 1).map(|t| &t.t), Some(PT::Op(")"))) => {
                        self.next();
                        self.next();
                        Expr::Super(pos)
                    }
                    "and" | "or" | "not" | "in" | "is" | "if" | "else" | "elif" | "for" | "while" | "def" | "return" | "lambda" | "pass" | "break" | "continue" => {
                        self.i -= 1;
                        return self.err(format!("invalid syntax: unexpected keyword '{}'", n));
                    }
                    _ => Expr::Ident(n, pos),
                })
            }
            PT::Op("(") => {
                self.next();
                if self.eat_op(")") {
                    return Ok(Expr::Array(vec![]));
                }
                let first = self.star_test()?;
                if self.is_kw("for") {
                    let Elem::Item(e) = first else { return self.err("invalid generator") };
                    let clauses = self.comp_clauses()?;
                    self.expect_op(")")?;
                    return Ok(Expr::Comp { kind: CompKind::List, key: Box::new(e), value: None, clauses });
                }
                if self.is_op(",") {
                    let mut items = vec![first];
                    while self.eat_op(",") {
                        if self.is_op(")") {
                            break;
                        }
                        items.push(self.star_test()?);
                    }
                    self.expect_op(")")?;
                    return Ok(Expr::Array(items));
                }
                self.expect_op(")")?;
                Ok(match first {
                    Elem::Item(e) => e,
                    other => Expr::Array(vec![other]),
                })
            }
            PT::Op("[") => {
                self.next();
                if self.eat_op("]") {
                    return Ok(Expr::Array(vec![]));
                }
                let first = self.star_test()?;
                if self.is_kw("for") {
                    let Elem::Item(e) = first else { return self.err("invalid comprehension") };
                    let clauses = self.comp_clauses()?;
                    self.expect_op("]")?;
                    return Ok(Expr::Comp { kind: CompKind::List, key: Box::new(e), value: None, clauses });
                }
                let mut items = vec![first];
                while self.eat_op(",") {
                    if self.is_op("]") {
                        break;
                    }
                    items.push(self.star_test()?);
                }
                self.expect_op("]")?;
                Ok(Expr::Array(items))
            }
            PT::Op("{") => {
                self.next();
                if self.eat_op("}") {
                    return Ok(Expr::Object(vec![]));
                }
                if self.eat_op("**") {
                    let mut props = vec![Prop::Spread(self.or_expr()?)];
                    while self.eat_op(",") {
                        if self.is_op("}") {
                            break;
                        }
                        props.push(self.dict_item()?);
                    }
                    self.expect_op("}")?;
                    return Ok(Expr::Object(props));
                }
                let k = self.test()?;
                if self.eat_op(":") {
                    let v = self.test()?;
                    if self.is_kw("for") {
                        let clauses = self.comp_clauses()?;
                        self.expect_op("}")?;
                        return Ok(Expr::Comp { kind: CompKind::Dict, key: Box::new(k), value: Some(Box::new(v)), clauses });
                    }
                    let mut props = vec![Prop::KV(PropKey::Computed(k), v)];
                    while self.eat_op(",") {
                        if self.is_op("}") {
                            break;
                        }
                        props.push(self.dict_item()?);
                    }
                    self.expect_op("}")?;
                    return Ok(Expr::Object(props));
                }
                // set literal / comprehension
                if self.is_kw("for") {
                    let clauses = self.comp_clauses()?;
                    self.expect_op("}")?;
                    return Ok(Expr::Comp { kind: CompKind::Set, key: Box::new(k), value: None, clauses });
                }
                let mut items = vec![Elem::Item(k)];
                while self.eat_op(",") {
                    if self.is_op("}") {
                        break;
                    }
                    items.push(Elem::Item(self.test()?));
                }
                self.expect_op("}")?;
                Ok(Expr::Call { callee: Box::new(Expr::Ident("set".into(), pos)), args: vec![Elem::Item(Expr::Array(items))], optional: false, pos })
            }
            PT::Op("...") => {
                self.next();
                Ok(Expr::Null)
            }
            other => self.err(format!("invalid syntax: unexpected {}", describe(&other))),
        }
    }

    fn dict_item(&mut self) -> R<Prop> {
        if self.eat_op("**") {
            return Ok(Prop::Spread(self.or_expr()?));
        }
        let k = self.test()?;
        self.expect_op(":")?;
        let v = self.test()?;
        Ok(Prop::KV(PropKey::Computed(k), v))
    }
}
