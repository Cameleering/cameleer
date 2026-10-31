//! The `jq` language: a subset of the jq JSON query language.
//!
//! Supported: `.`, `..`, `.foo`, `."foo"`, `.foo?`, `.[n]`, `.[]`, `.[a:b]`, `.["k"]`,
//! pipes `|`, comma `,`, alternative `//`, `and`/`or`/`not`, comparisons, arithmetic
//! `+ - * / %`, literals, array construction `[...]`, object construction
//! `{a: .x, "b": .y, c, (.k): .v}`, `if ... then ... elif ... else ... end`, and the
//! functions `length keys keys_unsorted values has(k) map(f) map_values(f) select(f)
//! empty not add any all flatten range(n) range(a;b) floor sqrt tostring tonumber type
//! sort sort_by(f) group_by(f) unique unique_by(f) min max min_by(f) max_by(f) reverse
//! contains(x) startswith(s) endswith(s) ltrimstr(s) rtrimstr(s) split(s) join(s)
//! ascii_downcase ascii_upcase to_entries from_entries with_entries(f) first last
//! first(f) last(f) tojson fromjson test(re) recurse error(msg) isempty(f) abs`.
//!
//! The result is the single output, `null` for no output, or an array of all outputs.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Expression, Language, Predicate};
use crate::value::{compare_values, Value};

#[derive(Debug, Clone, PartialEq)]
enum T {
    Dot,
    DotDot,
    Field(String),
    Ident(String),
    Str(String),
    Num(f64),
    Op(&'static str),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semi,
    Pipe,
    Question,
}

fn jerr(q: &str, m: &str) -> CamelError {
    CamelError::new(crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid jq expression '{}': {}", q, m))
}

fn lex(q: &str) -> Result<Vec<T>> {
    let c: Vec<char> = q.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '#' {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        match ch {
            '.' => {
                if i + 1 < c.len() && c[i + 1] == '.' {
                    out.push(T::DotDot);
                    i += 2;
                } else if i + 1 < c.len() && (c[i + 1].is_alphabetic() || c[i + 1] == '_') {
                    let st = i + 1;
                    i += 1;
                    while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                        i += 1;
                    }
                    out.push(T::Field(c[st..i].iter().collect()));
                } else {
                    out.push(T::Dot);
                    i += 1;
                }
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' && i + 1 < c.len() {
                        i += 1;
                        s.push(match c[i] {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            o => o,
                        });
                    } else {
                        s.push(c[i]);
                    }
                    i += 1;
                }
                if i >= c.len() {
                    return Err(jerr(q, "unterminated string"));
                }
                i += 1;
                out.push(T::Str(s));
            }
            '0'..='9' => {
                let st = i;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || c[i] == 'e' || c[i] == 'E') {
                    i += 1;
                }
                let t: String = c[st..i].iter().collect();
                out.push(T::Num(t.parse().map_err(|_| jerr(q, "invalid number"))?));
            }
            '(' => {
                out.push(T::LParen);
                i += 1;
            }
            ')' => {
                out.push(T::RParen);
                i += 1;
            }
            '[' => {
                out.push(T::LBracket);
                i += 1;
            }
            ']' => {
                out.push(T::RBracket);
                i += 1;
            }
            '{' => {
                out.push(T::LBrace);
                i += 1;
            }
            '}' => {
                out.push(T::RBrace);
                i += 1;
            }
            ',' => {
                out.push(T::Comma);
                i += 1;
            }
            ':' => {
                out.push(T::Colon);
                i += 1;
            }
            ';' => {
                out.push(T::Semi);
                i += 1;
            }
            '?' => {
                out.push(T::Question);
                i += 1;
            }
            '|' => {
                out.push(T::Pipe);
                i += 1;
            }
            _ => {
                let two: String = c[i..(i + 2).min(c.len())].iter().collect();
                let op2 = ["==", "!=", "<=", ">=", "//"].iter().find(|o| **o == two);
                if let Some(o) = op2 {
                    out.push(T::Op(o));
                    i += 2;
                    continue;
                }
                let op1 = ["<", ">", "+", "-", "*", "/", "%"].iter().find(|o| o.starts_with(ch));
                if let Some(o) = op1 {
                    out.push(T::Op(o));
                    i += 1;
                    continue;
                }
                if ch.is_alphabetic() || ch == '_' || ch == '$' || ch == '@' {
                    let st = i;
                    i += 1;
                    while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                        i += 1;
                    }
                    out.push(T::Ident(c[st..i].iter().collect()));
                    continue;
                }
                return Err(jerr(q, &format!("unexpected character '{}'", ch)));
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum E {
    Identity,
    Recurse,
    Lit(Value),
    Field(Box<E>, String, bool),
    Index(Box<E>, Box<E>, bool),
    Slice(Box<E>, Option<Box<E>>, Option<Box<E>>),
    Iter(Box<E>, bool),
    Pipe(Box<E>, Box<E>),
    Comma(Box<E>, Box<E>),
    Alt(Box<E>, Box<E>),
    And(Box<E>, Box<E>),
    Or(Box<E>, Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Neg(Box<E>),
    Array(Option<Box<E>>),
    Object(Vec<(E, Option<E>)>),
    If(Vec<(E, E)>, Option<Box<E>>),
    Call(String, Vec<E>),
    Try(Box<E>),
}

struct P<'a> {
    t: Vec<T>,
    i: usize,
    q: &'a str,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn eat(&mut self, t: &T) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &T, what: &str) -> Result<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(jerr(self.q, &format!("expected {}", what)))
        }
    }
    fn is_ident(&self, w: &str) -> bool {
        matches!(self.peek(), Some(T::Ident(x)) if x == w)
    }

    fn pipe(&mut self, allow_comma: bool) -> Result<E> {
        let mut l = if allow_comma { self.comma()? } else { self.alt()? };
        while self.eat(&T::Pipe) {
            let r = if allow_comma { self.comma()? } else { self.alt()? };
            l = E::Pipe(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn comma(&mut self) -> Result<E> {
        let mut l = self.alt()?;
        while self.eat(&T::Comma) {
            let r = self.alt()?;
            l = E::Comma(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn alt(&mut self) -> Result<E> {
        let mut l = self.or()?;
        while self.peek() == Some(&T::Op("//")) {
            self.i += 1;
            let r = self.or()?;
            l = E::Alt(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn or(&mut self) -> Result<E> {
        let mut l = self.and()?;
        while self.is_ident("or") {
            self.i += 1;
            let r = self.and()?;
            l = E::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<E> {
        let mut l = self.cmp()?;
        while self.is_ident("and") {
            self.i += 1;
            let r = self.cmp()?;
            l = E::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<E> {
        let l = self.add()?;
        if let Some(T::Op(o)) = self.peek().cloned() {
            if matches!(o, "==" | "!=" | "<" | "<=" | ">" | ">=") {
                self.i += 1;
                let r = self.add()?;
                return Ok(E::Bin(o, Box::new(l), Box::new(r)));
            }
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<E> {
        let mut l = self.mul()?;
        while let Some(T::Op(o)) = self.peek().cloned() {
            if o == "+" || o == "-" {
                self.i += 1;
                let r = self.mul()?;
                l = E::Bin(o, Box::new(l), Box::new(r));
            } else {
                break;
            }
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<E> {
        let mut l = self.unary()?;
        while let Some(T::Op(o)) = self.peek().cloned() {
            if o == "*" || o == "/" || o == "%" {
                self.i += 1;
                let r = self.unary()?;
                l = E::Bin(o, Box::new(l), Box::new(r));
            } else {
                break;
            }
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<E> {
        if self.peek() == Some(&T::Op("-")) {
            self.i += 1;
            return Ok(E::Neg(Box::new(self.postfix()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<E> {
        let mut e = self.primary()?;
        loop {
            match self.peek().cloned() {
                Some(T::Field(f)) => {
                    self.i += 1;
                    let opt = self.eat(&T::Question);
                    e = E::Field(Box::new(e), f, opt);
                }
                Some(T::Dot) if matches!(self.t.get(self.i + 1), Some(T::Str(_)) | Some(T::LBracket)) => {
                    self.i += 1;
                    if let Some(T::Str(s)) = self.peek().cloned() {
                        self.i += 1;
                        let opt = self.eat(&T::Question);
                        e = E::Field(Box::new(e), s, opt);
                    }
                }
                Some(T::LBracket) => {
                    e = self.bracket(e)?;
                }
                Some(T::Question) => {
                    self.i += 1;
                    e = E::Try(Box::new(e));
                }
                _ => return Ok(e),
            }
        }
    }

    fn bracket(&mut self, base: E) -> Result<E> {
        self.i += 1; // [
        if self.eat(&T::RBracket) {
            let opt = self.eat(&T::Question);
            return Ok(E::Iter(Box::new(base), opt));
        }
        if self.eat(&T::Colon) {
            let hi = self.pipe(true)?;
            self.expect(&T::RBracket, "']'")?;
            return Ok(E::Slice(Box::new(base), None, Some(Box::new(hi))));
        }
        let idx = self.pipe(true)?;
        if self.eat(&T::Colon) {
            let hi = if self.peek() == Some(&T::RBracket) { None } else { Some(Box::new(self.pipe(true)?)) };
            self.expect(&T::RBracket, "']'")?;
            return Ok(E::Slice(Box::new(base), Some(Box::new(idx)), hi));
        }
        self.expect(&T::RBracket, "']'")?;
        let opt = self.eat(&T::Question);
        Ok(E::Index(Box::new(base), Box::new(idx), opt))
    }

    fn primary(&mut self) -> Result<E> {
        match self.peek().cloned() {
            Some(T::Dot) => {
                self.i += 1;
                if let Some(T::Str(s)) = self.peek().cloned() {
                    self.i += 1;
                    let opt = self.eat(&T::Question);
                    return Ok(E::Field(Box::new(E::Identity), s, opt));
                }
                Ok(E::Identity)
            }
            Some(T::DotDot) => {
                self.i += 1;
                Ok(E::Recurse)
            }
            Some(T::Field(f)) => {
                self.i += 1;
                let opt = self.eat(&T::Question);
                Ok(E::Field(Box::new(E::Identity), f, opt))
            }
            Some(T::Num(n)) => {
                self.i += 1;
                Ok(E::Lit(if n.fract() == 0.0 && n.abs() < 9e15 { Value::Int(n as i64) } else { Value::Float(n) }))
            }
            Some(T::Str(s)) => {
                self.i += 1;
                Ok(E::Lit(Value::String(s)))
            }
            Some(T::LParen) => {
                self.i += 1;
                let e = self.pipe(true)?;
                self.expect(&T::RParen, "')'")?;
                Ok(e)
            }
            Some(T::LBracket) => {
                self.i += 1;
                if self.eat(&T::RBracket) {
                    return Ok(E::Array(None));
                }
                let e = self.pipe(true)?;
                self.expect(&T::RBracket, "']'")?;
                Ok(E::Array(Some(Box::new(e))))
            }
            Some(T::LBrace) => {
                self.i += 1;
                let mut entries = Vec::new();
                if self.eat(&T::RBrace) {
                    return Ok(E::Object(entries));
                }
                loop {
                    let key = match self.peek().cloned() {
                        Some(T::Ident(k)) => {
                            self.i += 1;
                            E::Lit(Value::String(k))
                        }
                        Some(T::Str(k)) => {
                            self.i += 1;
                            E::Lit(Value::String(k))
                        }
                        Some(T::LParen) => {
                            self.i += 1;
                            let k = self.pipe(true)?;
                            self.expect(&T::RParen, "')'")?;
                            k
                        }
                        _ => return Err(jerr(self.q, "invalid object key")),
                    };
                    let val = if self.eat(&T::Colon) { Some(self.pipe(false)?) } else { None };
                    entries.push((key, val));
                    if self.eat(&T::Comma) {
                        continue;
                    }
                    self.expect(&T::RBrace, "'}'")?;
                    return Ok(E::Object(entries));
                }
            }
            Some(T::Ident(w)) => {
                self.i += 1;
                match w.as_str() {
                    "true" => Ok(E::Lit(Value::Bool(true))),
                    "false" => Ok(E::Lit(Value::Bool(false))),
                    "null" => Ok(E::Lit(Value::Null)),
                    "if" => {
                        let mut branches = Vec::new();
                        let c = self.pipe(true)?;
                        if !self.is_ident("then") {
                            return Err(jerr(self.q, "expected 'then'"));
                        }
                        self.i += 1;
                        let t = self.pipe(true)?;
                        branches.push((c, t));
                        let mut els = None;
                        loop {
                            if self.is_ident("elif") {
                                self.i += 1;
                                let c = self.pipe(true)?;
                                if !self.is_ident("then") {
                                    return Err(jerr(self.q, "expected 'then'"));
                                }
                                self.i += 1;
                                let t = self.pipe(true)?;
                                branches.push((c, t));
                            } else if self.is_ident("else") {
                                self.i += 1;
                                els = Some(Box::new(self.pipe(true)?));
                            } else if self.is_ident("end") {
                                self.i += 1;
                                break;
                            } else {
                                return Err(jerr(self.q, "expected 'elif', 'else' or 'end'"));
                            }
                        }
                        Ok(E::If(branches, els))
                    }
                    _ => {
                        let mut args = Vec::new();
                        if self.eat(&T::LParen) {
                            loop {
                                args.push(self.pipe(true)?);
                                if self.eat(&T::Semi) {
                                    continue;
                                }
                                self.expect(&T::RParen, "')'")?;
                                break;
                            }
                        }
                        if !known_fn(&w, args.len()) {
                            return Err(jerr(self.q, &format!("{}/{} is not defined", w, args.len())));
                        }
                        Ok(E::Call(w, args))
                    }
                }
            }
            _ => Err(jerr(self.q, "unexpected token")),
        }
    }
}

fn known_fn(name: &str, n: usize) -> bool {
    match n {
        0 => matches!(
            name,
            "length" | "utf8bytelength" | "keys" | "keys_unsorted" | "values" | "empty" | "not" | "add" | "any" | "all" | "flatten" | "floor"
                | "sqrt" | "abs" | "tostring" | "tonumber" | "type" | "sort" | "unique" | "min" | "max" | "reverse" | "ascii_downcase"
                | "ascii_upcase" | "to_entries" | "from_entries" | "first" | "last" | "tojson" | "fromjson" | "recurse" | "error"
                | "arrays" | "objects" | "strings" | "numbers" | "booleans" | "nulls" | "iterables" | "scalars" | "ceil" | "round"
                | "trim" | "ltrim" | "rtrim"
        ),
        1 => matches!(
            name,
            "has" | "map" | "map_values" | "select" | "range" | "sort_by" | "group_by" | "unique_by" | "min_by" | "max_by" | "contains"
                | "inside" | "startswith" | "endswith" | "ltrimstr" | "rtrimstr" | "split" | "join" | "with_entries" | "first" | "last"
                | "test" | "error" | "isempty" | "any" | "all" | "flatten" | "recurse" | "limit" | "index" | "add"
        ),
        2 => matches!(name, "range" | "limit" | "test" | "sub" | "gsub"),
        _ => false,
    }
}

fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

fn jq_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(false) => 1,
            Value::Bool(true) => 2,
            Value::Int(_) | Value::Float(_) => 3,
            Value::String(_) | Value::Bytes(_) => 4,
            Value::Array(_) => 5,
            Value::Object(_) => 6,
        }
    }
    let (ra, rb) = (rank(a), rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = jq_cmp(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let kx: Vec<&String> = x.keys().collect();
            let ky: Vec<&String> = y.keys().collect();
            match kx.cmp(&ky) {
                Ordering::Equal => {
                    for (k, v) in x {
                        let c = jq_cmp(v, &y[k]);
                        if c != Ordering::Equal {
                            return c;
                        }
                    }
                    Ordering::Equal
                }
                o => o,
            }
        }
        _ => compare_values(a, b).unwrap_or(Ordering::Equal),
    }
}

fn type_err(m: String) -> CamelError {
    CamelError::new("JqException", m)
}

fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

fn arith(op: &str, a: &Value, b: &Value) -> Result<Value> {
    Ok(match (op, a, b) {
        ("+", Value::Null, x) | ("+", x, Value::Null) => x.clone(),
        ("+", Value::Int(x), Value::Int(y)) => Value::Int(x + y),
        ("-", Value::Int(x), Value::Int(y)) => Value::Int(x - y),
        ("*", Value::Int(x), Value::Int(y)) => Value::Int(x * y),
        ("%", Value::Int(x), Value::Int(y)) => {
            if *y == 0 {
                return Err(type_err("cannot be divided because the divisor is zero".into()));
            }
            Value::Int(x % y)
        }
        (_, Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            match op {
                "+" => num(x + y),
                "-" => num(x - y),
                "*" => num(x * y),
                "/" => {
                    if y == 0.0 {
                        return Err(type_err("cannot be divided because the divisor is zero".into()));
                    }
                    num(x / y)
                }
                _ => num(x % y),
            }
        }
        ("+", Value::String(x), Value::String(y)) => Value::String(format!("{}{}", x, y)),
        ("+", Value::Array(x), Value::Array(y)) => Value::Array(x.iter().chain(y.iter()).cloned().collect()),
        ("+", Value::Object(x), Value::Object(y)) => {
            let mut m = x.clone();
            m.extend(y.clone());
            Value::Object(m)
        }
        ("-", Value::Array(x), Value::Array(y)) => Value::Array(x.iter().filter(|i| !y.contains(i)).cloned().collect()),
        ("/", Value::String(x), Value::String(y)) => Value::from(x.split(y.as_str()).map(|s| s.to_string()).collect::<Vec<String>>()),
        ("*", Value::Object(x), Value::Object(y)) => {
            let mut m = x.clone();
            for (k, v) in y {
                let merged = match (m.get(k), v) {
                    (Some(a @ Value::Object(_)), Value::Object(_)) => arith("*", a, v)?,
                    _ => v.clone(),
                };
                m.insert(k.clone(), merged);
            }
            Value::Object(m)
        }
        _ => return Err(type_err(format!("{} ({}) and {} ({}) cannot be combined with '{}'", a.type_name(), a, b.type_name(), b, op))),
    })
}

fn recurse_all(v: &Value, out: &mut Vec<Value>) {
    out.push(v.clone());
    match v {
        Value::Array(a) => a.iter().for_each(|x| recurse_all(x, out)),
        Value::Object(o) => o.values().for_each(|x| recurse_all(x, out)),
        _ => {}
    }
}

fn flatten(v: &[Value], depth: i64, out: &mut Vec<Value>) {
    for x in v {
        match x {
            Value::Array(a) if depth > 0 => flatten(a, depth - 1, out),
            other => out.push(other.clone()),
        }
    }
}

fn contains(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => y.iter().all(|(k, v)| x.get(k).map(|xv| contains(xv, v)).unwrap_or(false)),
        (Value::Array(x), Value::Array(y)) => y.iter().all(|yv| x.iter().any(|xv| contains(xv, yv))),
        (Value::String(x), Value::String(y)) => x.contains(y.as_str()),
        _ => a == b,
    }
}

fn one(e: &E, input: &Value) -> Result<Value> {
    eval(e, input)?.into_iter().next().ok_or_else(|| type_err("expression produced no output".into()))
}

fn eval(e: &E, input: &Value) -> Result<Vec<Value>> {
    Ok(match e {
        E::Identity => vec![input.clone()],
        E::Recurse => {
            let mut out = Vec::new();
            recurse_all(input, &mut out);
            out
        }
        E::Lit(v) => vec![v.clone()],
        E::Field(base, name, opt) => {
            let mut out = Vec::new();
            for b in eval(base, input)? {
                match &b {
                    Value::Object(o) => out.push(o.get(name).cloned().unwrap_or(Value::Null)),
                    Value::Null => out.push(Value::Null),
                    other => {
                        if !opt {
                            return Err(type_err(format!("Cannot index {} with \"{}\"", other.type_name(), name)));
                        }
                    }
                }
            }
            out
        }
        E::Index(base, idx, opt) => {
            let mut out = Vec::new();
            for b in eval(base, input)? {
                for i in eval(idx, input)? {
                    match (&b, &i) {
                        (Value::Array(a), Value::Int(_) | Value::Float(_)) => {
                            let n = i.as_i64().unwrap();
                            let k = if n < 0 { a.len() as i64 + n } else { n };
                            out.push(if k >= 0 { a.get(k as usize).cloned().unwrap_or(Value::Null) } else { Value::Null });
                        }
                        (Value::Object(o), Value::String(s)) => out.push(o.get(s).cloned().unwrap_or(Value::Null)),
                        (Value::Null, _) => out.push(Value::Null),
                        _ => {
                            if !opt {
                                return Err(type_err(format!("Cannot index {} with {}", b.type_name(), i.type_name())));
                            }
                        }
                    }
                }
            }
            out
        }
        E::Slice(base, lo, hi) => {
            let mut out = Vec::new();
            for b in eval(base, input)? {
                let len = b.len().unwrap_or(0) as i64;
                let norm = |x: i64| if x < 0 { (len + x).max(0) } else { x.min(len) };
                let l = match lo {
                    Some(e) => norm(one(e, input)?.as_i64().unwrap_or(0)),
                    None => 0,
                };
                let h = match hi {
                    Some(e) => norm(one(e, input)?.as_i64().unwrap_or(len)),
                    None => len,
                };
                let h = h.max(l);
                out.push(match &b {
                    Value::Array(a) => Value::Array(a[l as usize..h as usize].to_vec()),
                    Value::String(s) => Value::String(s.chars().skip(l as usize).take((h - l) as usize).collect()),
                    Value::Null => Value::Null,
                    other => return Err(type_err(format!("Cannot slice {}", other.type_name()))),
                });
            }
            out
        }
        E::Iter(base, opt) => {
            let mut out = Vec::new();
            for b in eval(base, input)? {
                match b {
                    Value::Array(a) => out.extend(a),
                    Value::Object(o) => out.extend(o.into_values()),
                    other => {
                        if !opt {
                            return Err(type_err(format!("Cannot iterate over {}", other.type_name())));
                        }
                    }
                }
            }
            out
        }
        E::Pipe(a, b) => {
            let mut out = Vec::new();
            for v in eval(a, input)? {
                out.extend(eval(b, &v)?);
            }
            out
        }
        E::Comma(a, b) => {
            let mut out = eval(a, input)?;
            out.extend(eval(b, input)?);
            out
        }
        E::Alt(a, b) => {
            let l: Vec<Value> = match eval(a, input) {
                Ok(v) => v.into_iter().filter(truthy).collect(),
                Err(_) => vec![],
            };
            if l.is_empty() {
                eval(b, input)?
            } else {
                l
            }
        }
        E::And(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, input)? {
                if !truthy(&x) {
                    out.push(Value::Bool(false));
                } else {
                    for y in eval(b, input)? {
                        out.push(Value::Bool(truthy(&y)));
                    }
                }
            }
            out
        }
        E::Or(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, input)? {
                if truthy(&x) {
                    out.push(Value::Bool(true));
                } else {
                    for y in eval(b, input)? {
                        out.push(Value::Bool(truthy(&y)));
                    }
                }
            }
            out
        }
        E::Bin(op, a, b) => {
            let mut out = Vec::new();
            let rs = eval(b, input)?;
            for l in eval(a, input)? {
                for r in &rs {
                    use std::cmp::Ordering::*;
                    out.push(match *op {
                        "==" => Value::Bool(jq_cmp(&l, r) == Equal),
                        "!=" => Value::Bool(jq_cmp(&l, r) != Equal),
                        "<" => Value::Bool(jq_cmp(&l, r) == Less),
                        "<=" => Value::Bool(jq_cmp(&l, r) != Greater),
                        ">" => Value::Bool(jq_cmp(&l, r) == Greater),
                        ">=" => Value::Bool(jq_cmp(&l, r) != Less),
                        o => arith(o, &l, r)?,
                    });
                }
            }
            out
        }
        E::Neg(a) => {
            let mut out = Vec::new();
            for v in eval(a, input)? {
                out.push(arith("-", &Value::Int(0), &v)?);
            }
            out
        }
        E::Array(inner) => vec![Value::Array(match inner {
            Some(e) => eval(e, input)?,
            None => vec![],
        })],
        E::Object(entries) => {
            let mut results: Vec<BTreeMap<String, Value>> = vec![BTreeMap::new()];
            for (k, v) in entries {
                let keys = eval(k, input)?;
                let vals = match v {
                    Some(v) => eval(v, input)?,
                    None => {
                        let name = keys.first().map(|k| k.to_string()).unwrap_or_default();
                        vec![input.get(&name).cloned().unwrap_or(Value::Null)]
                    }
                };
                let mut next = Vec::new();
                for r in &results {
                    for key in &keys {
                        let ks = match key {
                            Value::String(s) => s.clone(),
                            other => return Err(type_err(format!("Object keys must be strings, got {}", other.type_name()))),
                        };
                        for val in &vals {
                            let mut m = r.clone();
                            m.insert(ks.clone(), val.clone());
                            next.push(m);
                        }
                    }
                }
                results = next;
            }
            results.into_iter().map(Value::Object).collect()
        }
        E::If(branches, els) => {
            fn go(branches: &[(E, E)], els: &Option<Box<E>>, input: &Value) -> Result<Vec<Value>> {
                match branches.split_first() {
                    None => match els {
                        Some(e) => eval(e, input),
                        None => Ok(vec![input.clone()]),
                    },
                    Some(((c, t), rest)) => {
                        let mut out = Vec::new();
                        for cv in eval(c, input)? {
                            if truthy(&cv) {
                                out.extend(eval(t, input)?);
                            } else {
                                out.extend(go(rest, els, input)?);
                            }
                        }
                        Ok(out)
                    }
                }
            }
            go(branches, els, input)?
        }
        E::Try(inner) => eval(inner, input).unwrap_or_default(),
        E::Call(name, args) => call(name, args, input)?,
    })
}

fn sort_by(items: &[Value], f: &E) -> Result<Vec<(Value, Value)>> {
    let mut keyed = Vec::new();
    for it in items {
        keyed.push((Value::Array(eval(f, it)?), it.clone()));
    }
    keyed.sort_by(|a, b| jq_cmp(&a.0, &b.0));
    Ok(keyed)
}

fn arr<'v>(v: &'v Value, f: &str) -> Result<&'v Vec<Value>> {
    v.as_array().ok_or_else(|| type_err(format!("{} ({}) cannot be used with {}", v.type_name(), v, f)))
}

fn call(name: &str, args: &[E], input: &Value) -> Result<Vec<Value>> {
    let s = || input.as_str().map(|x| x.to_string()).ok_or_else(|| type_err(format!("{} ({}) is not a string", input.type_name(), input)));
    let one_arg = |i: usize| one(&args[i], input);
    Ok(match (name, args.len()) {
        ("empty", 0) => vec![],
        ("error", _) => {
            let m = if args.is_empty() { input.to_string() } else { one_arg(0)?.to_string() };
            return Err(type_err(m));
        }
        ("not", 0) => vec![Value::Bool(!truthy(input))],
        ("length", 0) => vec![match input {
            Value::Null => Value::Int(0),
            Value::Bool(_) => return Err(type_err("boolean has no length".into())),
            Value::Int(i) => Value::Int(i.abs()),
            Value::Float(f) => Value::Float(f.abs()),
            other => Value::Int(other.len().unwrap_or(0) as i64),
        }],
        ("utf8bytelength", 0) => vec![Value::Int(s()?.len() as i64)],
        ("keys", 0) | ("keys_unsorted", 0) => vec![match input {
            Value::Object(o) => Value::Array(o.keys().map(|k| Value::String(k.clone())).collect()),
            Value::Array(a) => Value::Array((0..a.len()).map(|i| Value::Int(i as i64)).collect()),
            other => return Err(type_err(format!("{} has no keys", other.type_name()))),
        }],
        ("values", 0) => {
            if input.is_null() {
                vec![]
            } else {
                vec![input.clone()]
            }
        }
        ("has", 1) => {
            let k = one_arg(0)?;
            vec![Value::Bool(match (input, &k) {
                (Value::Object(o), Value::String(s)) => o.contains_key(s),
                (Value::Array(a), n) => n.as_i64().map(|i| i >= 0 && (i as usize) < a.len()).unwrap_or(false),
                _ => return Err(type_err(format!("Cannot check whether {} has a {} key", input.type_name(), k.type_name()))),
            })]
        }
        ("map", 1) => {
            let items: Vec<Value> = match input {
                Value::Object(o) => o.values().cloned().collect(),
                other => arr(other, "map")?.clone(),
            };
            let mut out = Vec::new();
            for it in &items {
                out.extend(eval(&args[0], it)?);
            }
            vec![Value::Array(out)]
        }
        ("map_values", 1) => vec![match input {
            Value::Object(o) => {
                let mut m = BTreeMap::new();
                for (k, v) in o {
                    if let Some(x) = eval(&args[0], v)?.into_iter().next() {
                        m.insert(k.clone(), x);
                    }
                }
                Value::Object(m)
            }
            Value::Array(a) => {
                let mut out = Vec::new();
                for v in a {
                    if let Some(x) = eval(&args[0], v)?.into_iter().next() {
                        out.push(x);
                    }
                }
                Value::Array(out)
            }
            other => return Err(type_err(format!("Cannot map over {}", other.type_name()))),
        }],
        ("select", 1) => {
            let mut out = Vec::new();
            for c in eval(&args[0], input)? {
                if truthy(&c) {
                    out.push(input.clone());
                }
            }
            out
        }
        ("add", 0) => {
            let items = match input {
                Value::Object(o) => o.values().cloned().collect(),
                other => arr(other, "add")?.clone(),
            };
            let mut acc = Value::Null;
            for it in &items {
                acc = arith("+", &acc, it)?;
            }
            vec![acc]
        }
        ("add", 1) => {
            let mut acc = Value::Null;
            for it in eval(&args[0], input)? {
                acc = arith("+", &acc, &it)?;
            }
            vec![acc]
        }
        ("any", 0) => vec![Value::Bool(arr(input, "any")?.iter().any(truthy))],
        ("all", 0) => vec![Value::Bool(arr(input, "all")?.iter().all(truthy))],
        ("any", 1) | ("all", 1) => {
            let items = arr(input, name)?;
            let mut res = Vec::new();
            for it in items {
                res.push(eval(&args[0], it)?.iter().any(truthy));
            }
            vec![Value::Bool(if name == "any" { res.iter().any(|b| *b) } else { res.iter().all(|b| *b) })]
        }
        ("flatten", n) => {
            let depth = if n == 1 { one_arg(0)?.as_i64().unwrap_or(1) } else { i64::MAX };
            let mut out = Vec::new();
            flatten(arr(input, "flatten")?, depth, &mut out);
            vec![Value::Array(out)]
        }
        ("range", 1) => {
            let n = one_arg(0)?.as_i64().unwrap_or(0);
            (0..n.max(0)).map(Value::Int).collect()
        }
        ("range", 2) => {
            let a = one_arg(0)?.as_i64().unwrap_or(0);
            let b = one_arg(1)?.as_i64().unwrap_or(0);
            (a..b).map(Value::Int).collect()
        }
        ("floor", 0) | ("ceil", 0) | ("round", 0) | ("sqrt", 0) | ("abs", 0) => {
            let f = input.as_f64().filter(|_| matches!(input, Value::Int(_) | Value::Float(_))).ok_or_else(|| type_err(format!("{} ({}) number required", input.type_name(), input)))?;
            vec![match name {
                "floor" => num(f.floor()),
                "ceil" => num(f.ceil()),
                "round" => num(f.round()),
                "abs" => num(f.abs()),
                _ => Value::Float(f.sqrt()),
            }]
        }
        ("tostring", 0) => vec![match input {
            Value::String(_) => input.clone(),
            other => Value::String(other.to_json()),
        }],
        ("tonumber", 0) => vec![match input {
            Value::Int(_) | Value::Float(_) => input.clone(),
            other => {
                let st = other.to_string();
                st.trim().parse::<i64>().map(Value::Int).or_else(|_| st.trim().parse::<f64>().map(Value::Float)).map_err(|_| type_err(format!("Cannot parse '{}' as number", st)))?
            }
        }],
        ("type", 0) => vec![Value::from(match input {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) | Value::Float(_) => "number",
            Value::String(_) | Value::Bytes(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        })],
        ("arrays", 0) | ("objects", 0) | ("strings", 0) | ("numbers", 0) | ("booleans", 0) | ("nulls", 0) | ("iterables", 0) | ("scalars", 0) => {
            let keep = match name {
                "arrays" => matches!(input, Value::Array(_)),
                "objects" => matches!(input, Value::Object(_)),
                "strings" => matches!(input, Value::String(_)),
                "numbers" => matches!(input, Value::Int(_) | Value::Float(_)),
                "booleans" => matches!(input, Value::Bool(_)),
                "nulls" => input.is_null(),
                "iterables" => matches!(input, Value::Array(_) | Value::Object(_)),
                _ => !matches!(input, Value::Array(_) | Value::Object(_)),
            };
            if keep {
                vec![input.clone()]
            } else {
                vec![]
            }
        }
        ("sort", 0) => {
            let mut a = arr(input, "sort")?.clone();
            a.sort_by(jq_cmp);
            vec![Value::Array(a)]
        }
        ("sort_by", 1) => vec![Value::Array(sort_by(arr(input, "sort_by")?, &args[0])?.into_iter().map(|(_, v)| v).collect())],
        ("group_by", 1) => {
            let keyed = sort_by(arr(input, "group_by")?, &args[0])?;
            let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
            for (k, v) in keyed {
                match groups.last_mut() {
                    Some((gk, g)) if jq_cmp(gk, &k) == std::cmp::Ordering::Equal => g.push(v),
                    _ => groups.push((k, vec![v])),
                }
            }
            vec![Value::Array(groups.into_iter().map(|(_, g)| Value::Array(g)).collect())]
        }
        ("unique", 0) => {
            let mut a = arr(input, "unique")?.clone();
            a.sort_by(jq_cmp);
            a.dedup_by(|x, y| jq_cmp(x, y) == std::cmp::Ordering::Equal);
            vec![Value::Array(a)]
        }
        ("unique_by", 1) => {
            let mut keyed = sort_by(arr(input, "unique_by")?, &args[0])?;
            keyed.dedup_by(|x, y| jq_cmp(&x.0, &y.0) == std::cmp::Ordering::Equal);
            vec![Value::Array(keyed.into_iter().map(|(_, v)| v).collect())]
        }
        ("min", 0) | ("max", 0) => {
            let a = arr(input, name)?;
            let r = if name == "min" { a.iter().min_by(|x, y| jq_cmp(x, y)) } else { a.iter().max_by(|x, y| jq_cmp(x, y)) };
            vec![r.cloned().unwrap_or(Value::Null)]
        }
        ("min_by", 1) | ("max_by", 1) => {
            let keyed = sort_by(arr(input, name)?, &args[0])?;
            let r = if name == "min_by" { keyed.first() } else { keyed.last() };
            vec![r.map(|(_, v)| v.clone()).unwrap_or(Value::Null)]
        }
        ("reverse", 0) => vec![match input {
            Value::Array(a) => Value::Array(a.iter().rev().cloned().collect()),
            Value::String(st) => Value::String(st.chars().rev().collect()),
            Value::Null => Value::Array(vec![]),
            other => return Err(type_err(format!("Cannot reverse {}", other.type_name()))),
        }],
        ("contains", 1) => vec![Value::Bool(contains(input, &one_arg(0)?))],
        ("inside", 1) => vec![Value::Bool(contains(&one_arg(0)?, input))],
        ("startswith", 1) => vec![Value::Bool(s()?.starts_with(&one_arg(0)?.to_string()))],
        ("endswith", 1) => vec![Value::Bool(s()?.ends_with(&one_arg(0)?.to_string()))],
        ("ltrimstr", 1) => {
            let p = one_arg(0)?.to_string();
            vec![match input.as_str() {
                Some(st) => Value::String(st.strip_prefix(p.as_str()).unwrap_or(st).to_string()),
                None => input.clone(),
            }]
        }
        ("rtrimstr", 1) => {
            let p = one_arg(0)?.to_string();
            vec![match input.as_str() {
                Some(st) => Value::String(st.strip_suffix(p.as_str()).unwrap_or(st).to_string()),
                None => input.clone(),
            }]
        }
        ("trim", 0) => vec![Value::String(s()?.trim().to_string())],
        ("ltrim", 0) => vec![Value::String(s()?.trim_start().to_string())],
        ("rtrim", 0) => vec![Value::String(s()?.trim_end().to_string())],
        ("split", 1) => {
            let sep = one_arg(0)?.to_string();
            vec![Value::from(s()?.split(sep.as_str()).map(|x| x.to_string()).collect::<Vec<String>>())]
        }
        ("join", 1) => {
            let sep = one_arg(0)?.to_string();
            let parts: Vec<String> = arr(input, "join")?.iter().map(|v| if v.is_null() { String::new() } else { v.to_string() }).collect();
            vec![Value::String(parts.join(&sep))]
        }
        ("ascii_downcase", 0) => vec![Value::String(s()?.to_ascii_lowercase())],
        ("ascii_upcase", 0) => vec![Value::String(s()?.to_ascii_uppercase())],
        ("to_entries", 0) => vec![Value::Array(match input {
            Value::Object(o) => o.iter().map(|(k, v)| super::support::obj(vec![("key", Value::String(k.clone())), ("value", v.clone())])).collect(),
            other => return Err(type_err(format!("{} has no keys", other.type_name()))),
        })],
        ("from_entries", 0) => {
            let mut m = BTreeMap::new();
            for e in arr(input, "from_entries")? {
                let k = e.get("key").or_else(|| e.get("k")).or_else(|| e.get("name")).cloned().unwrap_or(Value::Null);
                let v = e.get("value").or_else(|| e.get("v")).cloned().unwrap_or(Value::Null);
                m.insert(k.to_string(), v);
            }
            vec![Value::Object(m)]
        }
        ("with_entries", 1) => {
            let entries = call("to_entries", &[], input)?.remove(0);
            let mapped = call("map", args, &entries)?.remove(0);
            call("from_entries", &[], &mapped)?
        }
        ("first", 0) => vec![arr(input, "first")?.first().cloned().unwrap_or(Value::Null)],
        ("last", 0) => vec![arr(input, "last")?.last().cloned().unwrap_or(Value::Null)],
        ("first", 1) => eval(&args[0], input)?.into_iter().take(1).collect(),
        ("last", 1) => eval(&args[0], input)?.into_iter().last().into_iter().collect(),
        ("limit", 2) => {
            let n = one_arg(0)?.as_i64().unwrap_or(0).max(0) as usize;
            eval(&args[1], input)?.into_iter().take(n).collect()
        }
        ("isempty", 1) => vec![Value::Bool(eval(&args[0], input)?.is_empty())],
        ("tojson", 0) => vec![Value::String(input.to_json())],
        ("fromjson", 0) => vec![crate::json::parse(&s()?)?],
        ("recurse", 0) => {
            let mut out = Vec::new();
            recurse_all(input, &mut out);
            out
        }
        ("recurse", 1) => {
            let mut out = Vec::new();
            let mut stack = vec![input.clone()];
            while let Some(v) = stack.pop() {
                out.push(v.clone());
                if out.len() > 100_000 {
                    break;
                }
                let next = eval(&args[0], &v).unwrap_or_default();
                for n in next.into_iter().rev() {
                    stack.push(n);
                }
            }
            out
        }
        ("test", n) => {
            let re = one_arg(0)?.to_string();
            let flags = if n == 2 { one_arg(1)?.to_string() } else { String::new() };
            let r = crate::regex::Regex::with_flags(&re, flags.contains('i'))?;
            vec![Value::Bool(r.is_match(&s()?))]
        }
        ("sub", 2) | ("gsub", 2) => {
            let re = crate::regex::Regex::new(&one_arg(0)?.to_string())?;
            let rep = one_arg(1)?.to_string();
            let st = s()?;
            vec![Value::String(if name == "sub" { re.replace_first(&st, &rep) } else { re.replace_all(&st, &rep) })]
        }
        ("index", 1) => {
            let needle = one_arg(0)?.to_string();
            vec![s()?.find(&needle).map(|k| Value::Int(k as i64)).unwrap_or(Value::Null)]
        }
        (other, n) => return Err(type_err(format!("{}/{} is not defined", other, n))),
    })
}

/// A compiled jq program.
#[derive(Debug, Clone)]
pub struct Jq {
    text: String,
    expr: E,
}

impl Jq {
    /// Compile a jq program.
    pub fn compile(q: &str) -> Result<Jq> {
        let toks = lex(q)?;
        if toks.is_empty() {
            return Ok(Jq { text: q.to_string(), expr: E::Identity });
        }
        let mut p = P { t: toks, i: 0, q };
        let expr = p.pipe(true)?;
        if p.i < p.t.len() {
            return Err(jerr(q, &format!("unexpected token {:?}", p.t[p.i])));
        }
        Ok(Jq { text: q.to_string(), expr })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// All outputs for an input.
    pub fn outputs(&self, input: &Value) -> Result<Vec<Value>> {
        let v = super::jsonpath::to_json_value(input)?;
        eval(&self.expr, &v)
    }

    /// Single output, null for none, array for several.
    pub fn evaluate(&self, input: &Value) -> Result<Value> {
        let mut outs = self.outputs(input)?;
        Ok(match outs.len() {
            0 => Value::Null,
            1 => outs.pop().unwrap(),
            _ => Value::Array(outs),
        })
    }
}

/// Run a jq program against a value.
pub fn query(input: &Value, q: &str) -> Result<Value> {
    Jq::compile(q)?.evaluate(input)
}

struct JqExpression {
    jq: Jq,
    options: BTreeMap<String, String>,
}

impl Expression for JqExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let input = super::jsonpath::source_value(ex, &self.options);
        let v = self.jq.evaluate(&input)?;
        match self.options.get("resultType").filter(|t| !t.is_empty()) {
            Some(t) => super::support::convert_to_type(&v, t),
            None => Ok(v),
        }
    }
}

impl Predicate for JqExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        let input = super::jsonpath::source_value(ex, &self.options);
        Ok(self.jq.outputs(&input)?.iter().any(truthy))
    }
}

/// The `jq` language.
#[derive(Default)]
pub struct JqLanguage;

impl Language for JqLanguage {
    fn name(&self) -> &str {
        "jq"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(JqExpression { jq: Jq::compile(text)?, options: options.clone() }))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(JqExpression { jq: Jq::compile(text)?, options: options.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let v = crate::json::parse(r#"{"a":[{"n":1},{"n":2},{"n":3}],"name":"x"}"#).unwrap();
        assert_eq!(query(&v, ".name").unwrap(), Value::from("x"));
        assert_eq!(query(&v, "[.a[] | select(.n > 1) | .n]").unwrap(), Value::from(vec![Value::Int(2), Value::Int(3)]));
        assert_eq!(query(&v, ".a | length").unwrap(), Value::Int(3));
        assert!(Jq::compile(".a | nosuchfn").is_err());
    }
}
