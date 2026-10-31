//! The `jsonpath` language (Jayway JsonPath compatible subset).
//!
//! Supported: `$`, `.key`, `['key']`, `["key"]`, `['a','b']`, `[n]`, `[-n]`, `[a,b]`, `[*]`,
//! `.*`, `..key` / `..*` / `..[n]` (recursive descent), slices `[start:end:step]`, filters
//! `[?(@.price < 10 && @.category == 'fiction')]` with `== != < <= > >= =~ in nin contains
//! size empty`, `&&`, `||`, `!`, parentheses, existence checks (`[?(@.isbn)]`) and `$` root
//! references, plus trailing functions `length()`, `size()`, `min()`, `max()`, `avg()`,
//! `sum()`, `keys()`, `first()`, `last()`.
//!
//! The result is a single value when the path is definite (no wildcards, recursion,
//! slices, filters or unions) and an array otherwise. A definite path that does not exist
//! raises `PathNotFoundException` unless `suppressExceptions=true`.
//!
//! Language options: `resultType`, `suppressExceptions`, `headerName`, `source`
//! (`header:name`, `property:name`, `variable:name`), `unpackArray`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::regex::Regex;
use crate::spi::{Expression, Language, Predicate};
use crate::value::Value;

/// Exception kind raised when a definite path does not exist.
pub const PATH_NOT_FOUND: &str = "PathNotFoundException";

#[derive(Debug, Clone)]
enum Sel {
    Name(String),
    Wildcard,
    Index(i64),
    Slice(Option<i64>, Option<i64>, Option<i64>),
    Filter(Box<FExpr>),
}

#[derive(Debug, Clone)]
struct Seg {
    descendant: bool,
    sels: Vec<Sel>,
}

#[derive(Debug, Clone)]
enum FOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Regex,
    In,
    Nin,
    Contains,
    Size,
    Empty,
    AnyOf,
    NoneOf,
    SubsetOf,
}

#[derive(Debug, Clone)]
enum Operand {
    Path(bool, Vec<Seg>, Option<String>),
    Lit(Value),
    Regex(Regex),
}

#[derive(Debug, Clone)]
enum FExpr {
    Or(Vec<FExpr>),
    And(Vec<FExpr>),
    Not(Box<FExpr>),
    Exists(Operand),
    Cmp(Operand, FOp, Operand),
}

/// A compiled JSONPath.
#[derive(Debug, Clone)]
pub struct JsonPath {
    path: String,
    segs: Vec<Seg>,
    func: Option<String>,
}

fn perr(path: &str, msg: &str, pos: usize) -> CamelError {
    CamelError::new(
        crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX,
        format!("Invalid JsonPath {}: {} at position {}", path, msg, pos),
    )
}

struct Parser<'a> {
    c: Vec<char>,
    i: usize,
    path: &'a str,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> CamelError {
        perr(self.path, msg, self.i)
    }
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.i += 1;
        }
    }
    fn starts(&self, s: &str) -> bool {
        let p: Vec<char> = s.chars().collect();
        self.i + p.len() <= self.c.len() && self.c[self.i..self.i + p.len()] == p[..]
    }

    fn ident(&mut self) -> String {
        let st = self.i;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '$' || c == '@' || (c as u32) > 127 {
                self.i += 1;
            } else {
                break;
            }
        }
        self.c[st..self.i].iter().collect()
    }

    /// Parse segments until end or a char that cannot continue a path. Returns (segments, function).
    fn segments(&mut self, in_filter: bool) -> Result<(Vec<Seg>, Option<String>)> {
        let mut segs = Vec::new();
        loop {
            if self.starts("..") {
                self.i += 2;
                if self.peek() == Some('[') {
                    let sels = self.bracket()?;
                    segs.push(Seg { descendant: true, sels });
                } else if self.peek() == Some('*') {
                    self.i += 1;
                    segs.push(Seg { descendant: true, sels: vec![Sel::Wildcard] });
                } else {
                    let n = self.ident();
                    if n.is_empty() {
                        return Err(self.err("Expected name after '..'"));
                    }
                    segs.push(Seg { descendant: true, sels: vec![Sel::Name(n)] });
                }
            } else if self.peek() == Some('.') {
                self.i += 1;
                if self.peek() == Some('*') {
                    self.i += 1;
                    segs.push(Seg { descendant: false, sels: vec![Sel::Wildcard] });
                } else if self.peek() == Some('[') {
                    let sels = self.bracket()?;
                    segs.push(Seg { descendant: false, sels });
                } else {
                    let n = self.ident();
                    if n.is_empty() {
                        return Err(self.err("Expected name after '.'"));
                    }
                    if self.starts("()") {
                        self.i += 2;
                        return Ok((segs, Some(n)));
                    }
                    segs.push(Seg { descendant: false, sels: vec![Sel::Name(n)] });
                }
            } else if self.peek() == Some('[') {
                let sels = self.bracket()?;
                segs.push(Seg { descendant: false, sels });
            } else {
                if !in_filter && self.peek().is_some() {
                    return Err(self.err(&format!("Unexpected character '{}'", self.peek().unwrap())));
                }
                return Ok((segs, None));
            }
        }
    }

    fn quoted(&mut self) -> Result<String> {
        let q = self.peek().unwrap();
        self.i += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == '\\' {
                if let Some(n) = self.peek() {
                    self.i += 1;
                    out.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
                continue;
            }
            if c == q {
                return Ok(out);
            }
            out.push(c);
        }
        Err(self.err("Unterminated string"))
    }

    fn int(&mut self) -> Option<i64> {
        let st = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        let t: String = self.c[st..self.i].iter().collect();
        if t.is_empty() || t == "-" {
            self.i = st;
            return None;
        }
        t.parse().ok()
    }

    fn bracket(&mut self) -> Result<Vec<Sel>> {
        self.i += 1; // [
        self.ws();
        let mut sels = Vec::new();
        if self.peek() == Some('?') {
            self.i += 1;
            self.ws();
            let paren = self.peek() == Some('(');
            if paren {
                self.i += 1;
            }
            let e = self.f_or()?;
            self.ws();
            if paren {
                if self.peek() != Some(')') {
                    return Err(self.err("Expected ')' to close filter"));
                }
                self.i += 1;
                self.ws();
            }
            if self.peek() != Some(']') {
                return Err(self.err("Expected ']' after filter"));
            }
            self.i += 1;
            return Ok(vec![Sel::Filter(Box::new(e))]);
        }
        loop {
            self.ws();
            match self.peek() {
                Some('*') => {
                    self.i += 1;
                    sels.push(Sel::Wildcard);
                }
                Some('\'') | Some('"') => {
                    let s = self.quoted()?;
                    sels.push(Sel::Name(s));
                }
                Some(c) if c.is_ascii_digit() || c == '-' || c == ':' => {
                    let a = self.int();
                    self.ws();
                    if self.peek() == Some(':') {
                        self.i += 1;
                        self.ws();
                        let b = self.int();
                        self.ws();
                        let mut step = None;
                        if self.peek() == Some(':') {
                            self.i += 1;
                            step = self.int();
                        }
                        sels.push(Sel::Slice(a, b, step));
                    } else {
                        sels.push(Sel::Index(a.ok_or_else(|| self.err("Expected index"))?));
                    }
                }
                _ => {
                    let n = self.ident();
                    if n.is_empty() {
                        return Err(self.err("Invalid bracket expression"));
                    }
                    sels.push(Sel::Name(n));
                }
            }
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                }
                Some(']') => {
                    self.i += 1;
                    return Ok(sels);
                }
                _ => return Err(self.err("Expected ',' or ']'")),
            }
        }
    }

    fn f_or(&mut self) -> Result<FExpr> {
        let mut items = vec![self.f_and()?];
        loop {
            self.ws();
            if self.starts("||") {
                self.i += 2;
                items.push(self.f_and()?);
            } else {
                break;
            }
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { FExpr::Or(items) })
    }

    fn f_and(&mut self) -> Result<FExpr> {
        let mut items = vec![self.f_unary()?];
        loop {
            self.ws();
            if self.starts("&&") {
                self.i += 2;
                items.push(self.f_unary()?);
            } else {
                break;
            }
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { FExpr::And(items) })
    }

    fn f_unary(&mut self) -> Result<FExpr> {
        self.ws();
        if self.peek() == Some('!') && !self.starts("!=") {
            self.i += 1;
            return Ok(FExpr::Not(Box::new(self.f_unary()?)));
        }
        if self.peek() == Some('(') {
            self.i += 1;
            let e = self.f_or()?;
            self.ws();
            if self.peek() != Some(')') {
                return Err(self.err("Expected ')'"));
            }
            self.i += 1;
            return Ok(e);
        }
        let left = self.operand()?;
        self.ws();
        let op = if self.starts("==") {
            self.i += 2;
            Some(FOp::Eq)
        } else if self.starts("!=") {
            self.i += 2;
            Some(FOp::Ne)
        } else if self.starts("<=") {
            self.i += 2;
            Some(FOp::Le)
        } else if self.starts(">=") {
            self.i += 2;
            Some(FOp::Ge)
        } else if self.starts("=~") {
            self.i += 2;
            Some(FOp::Regex)
        } else if self.starts("<") {
            self.i += 1;
            Some(FOp::Lt)
        } else if self.starts(">") {
            self.i += 1;
            Some(FOp::Gt)
        } else {
            let save = self.i;
            let w = self.ident();
            let op = match w.as_str() {
                "in" => Some(FOp::In),
                "nin" => Some(FOp::Nin),
                "contains" => Some(FOp::Contains),
                "size" => Some(FOp::Size),
                "empty" => Some(FOp::Empty),
                "anyof" => Some(FOp::AnyOf),
                "noneof" => Some(FOp::NoneOf),
                "subsetof" => Some(FOp::SubsetOf),
                _ => None,
            };
            if op.is_none() {
                self.i = save;
            }
            op
        };
        match op {
            None => Ok(FExpr::Exists(left)),
            Some(op) => {
                self.ws();
                let right = self.operand()?;
                Ok(FExpr::Cmp(left, op, right))
            }
        }
    }

    fn operand(&mut self) -> Result<Operand> {
        self.ws();
        match self.peek() {
            Some('@') | Some('$') => {
                let root = self.peek() == Some('$');
                self.i += 1;
                let (segs, f) = self.segments(true)?;
                Ok(Operand::Path(root, segs, f))
            }
            Some('\'') | Some('"') => Ok(Operand::Lit(Value::String(self.quoted()?))),
            Some('/') => {
                self.i += 1;
                let mut pat = String::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '\\' && self.peek() == Some('/') {
                        pat.push('/');
                        self.i += 1;
                        continue;
                    }
                    if c == '/' {
                        break;
                    }
                    pat.push(c);
                }
                let mut ci = false;
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphabetic() {
                        if c == 'i' {
                            ci = true;
                        }
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                Ok(Operand::Regex(Regex::with_flags(&pat, ci)?))
            }
            Some('[') => {
                // literal list
                let st = self.i;
                let mut depth = 0;
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '[' {
                        depth += 1;
                    } else if c == ']' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                let t: String = self.c[st..self.i].iter().collect();
                let v = crate::json::parse(&t.replace('\'', "\"")).map_err(|_| self.err("Invalid list literal"))?;
                Ok(Operand::Lit(v))
            }
            Some('{') => {
                let st = self.i;
                let mut depth = 0;
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                let t: String = self.c[st..self.i].iter().collect();
                Ok(Operand::Lit(crate::json::parse(&t.replace('\'', "\"")).map_err(|_| self.err("Invalid object literal"))?))
            }
            Some(c) if c.is_ascii_digit() || c == '-' => {
                let st = self.i;
                self.i += 1;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E') {
                    self.i += 1;
                }
                let t: String = self.c[st..self.i].iter().collect();
                if let Ok(n) = t.parse::<i64>() {
                    Ok(Operand::Lit(Value::Int(n)))
                } else {
                    Ok(Operand::Lit(Value::Float(t.parse::<f64>().map_err(|_| self.err("Invalid number"))?)))
                }
            }
            _ => {
                let w = self.ident();
                match w.as_str() {
                    "true" => Ok(Operand::Lit(Value::Bool(true))),
                    "false" => Ok(Operand::Lit(Value::Bool(false))),
                    "null" => Ok(Operand::Lit(Value::Null)),
                    _ => Err(self.err("Invalid filter operand")),
                }
            }
        }
    }
}

impl JsonPath {
    /// Compile a JSONPath expression.
    pub fn compile(path: &str) -> Result<JsonPath> {
        let t = path.trim();
        let norm = if t.starts_with('$') {
            t.to_string()
        } else if t.starts_with('.') || t.starts_with('[') {
            format!("${}", t)
        } else {
            format!("$.{}", t)
        };
        let mut p = Parser { c: norm.chars().collect(), i: 1, path };
        let (segs, func) = p.segments(false)?;
        if p.i < p.c.len() {
            return Err(p.err("Unexpected trailing characters"));
        }
        Ok(JsonPath { path: path.to_string(), segs, func })
    }

    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// True if the path selects at most one value.
    pub fn is_definite(&self) -> bool {
        is_definite(&self.segs)
    }

    /// Evaluate against a JSON value. Strings/bytes are parsed as JSON first.
    pub fn evaluate(&self, input: &Value) -> Result<Value> {
        let root = to_json_value(input)?;
        let nodes = select(&self.segs, &root, &root);
        let definite = self.is_definite();
        let result = if definite {
            match nodes.into_iter().next() {
                Some(v) => v,
                None => {
                    return Err(CamelError::new(PATH_NOT_FOUND, format!("No results for path: {}", self.path)));
                }
            }
        } else {
            Value::Array(nodes)
        };
        match &self.func {
            None => Ok(result),
            Some(f) => apply_func(f, &result),
        }
    }
}

fn is_definite(segs: &[Seg]) -> bool {
    segs.iter().all(|s| !s.descendant && s.sels.len() == 1 && matches!(s.sels[0], Sel::Name(_) | Sel::Index(_)))
}

/// Parse strings/bytes as JSON; structured values are returned as is.
pub fn to_json_value(input: &Value) -> Result<Value> {
    match input {
        Value::String(_) | Value::Bytes(_) => {
            let s = input.as_str().ok_or_else(|| CamelError::new(crate::error::kinds::TYPE_CONVERSION, "Body is not valid UTF-8 JSON"))?;
            crate::json::parse(s)
        }
        other => Ok(other.clone()),
    }
}

fn apply_func(f: &str, v: &Value) -> Result<Value> {
    let nums = || -> Vec<f64> {
        match v {
            Value::Array(a) => a.iter().filter_map(|x| x.as_f64()).collect(),
            other => other.as_f64().into_iter().collect(),
        }
    };
    // Jayway returns doubles for the numeric aggregate functions
    let num = Value::Float;
    Ok(match f {
        "length" | "size" => Value::Int(v.len().unwrap_or(0) as i64),
        "min" => num(nums().into_iter().fold(f64::INFINITY, f64::min)),
        "max" => num(nums().into_iter().fold(f64::NEG_INFINITY, f64::max)),
        "sum" => num(nums().into_iter().sum()),
        "avg" => {
            let n = nums();
            if n.is_empty() {
                Value::Null
            } else {
                num(n.iter().sum::<f64>() / n.len() as f64)
            }
        }
        "keys" => Value::Array(v.as_object().map(|o| o.keys().map(|k| Value::String(k.clone())).collect()).unwrap_or_default()),
        "first" => v.as_array().and_then(|a| a.first().cloned()).unwrap_or(Value::Null),
        "last" => v.as_array().and_then(|a| a.last().cloned()).unwrap_or(Value::Null),
        "stddev" => {
            let n = nums();
            if n.is_empty() {
                Value::Null
            } else {
                let m = n.iter().sum::<f64>() / n.len() as f64;
                Value::Float((n.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n.len() as f64).sqrt())
            }
        }
        other => return Err(CamelError::illegal_argument(format!("Unknown JsonPath function: {}()", other))),
    })
}

fn children(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        _ => vec![],
    }
}

fn apply_sel(sel: &Sel, v: &Value, root: &Value, out: &mut Vec<Value>) {
    match sel {
        Sel::Name(n) => {
            if let Value::Object(o) = v {
                if let Some(x) = o.get(n) {
                    out.push(x.clone());
                }
            }
        }
        Sel::Wildcard => out.extend(children(v).into_iter().cloned()),
        Sel::Index(i) => {
            if let Value::Array(a) = v {
                let idx = if *i < 0 { a.len() as i64 + i } else { *i };
                if idx >= 0 && (idx as usize) < a.len() {
                    out.push(a[idx as usize].clone());
                }
            }
        }
        Sel::Slice(s, e, step) => {
            if let Value::Array(a) = v {
                let len = a.len() as i64;
                let norm = |x: i64| if x < 0 { (len + x).max(0) } else { x.min(len) };
                let st = s.map(norm).unwrap_or(0);
                let en = e.map(norm).unwrap_or(len);
                let step = step.unwrap_or(1).max(1);
                let mut k = st;
                while k < en {
                    out.push(a[k as usize].clone());
                    k += step;
                }
            }
        }
        Sel::Filter(f) => {
            let items: Vec<&Value> = match v {
                Value::Array(a) => a.iter().collect(),
                Value::Object(_) => vec![v],
                _ => vec![],
            };
            for it in items {
                if eval_filter(f, it, root) {
                    out.push(it.clone());
                }
            }
        }
    }
}

fn descendants<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(v);
    for c in children(v) {
        descendants(c, out);
    }
}

fn select(segs: &[Seg], start: &Value, root: &Value) -> Vec<Value> {
    let mut nodes = vec![start.clone()];
    for seg in segs {
        let mut next = Vec::new();
        for n in &nodes {
            if seg.descendant {
                let mut all = Vec::new();
                descendants(n, &mut all);
                for d in all {
                    for sel in &seg.sels {
                        // filters on descendants apply to arrays' elements only
                        if let Sel::Filter(_) = sel {
                            if let Value::Array(_) = d {
                                apply_sel(sel, d, root, &mut next);
                            }
                        } else {
                            apply_sel(sel, d, root, &mut next);
                        }
                    }
                }
            } else {
                for sel in &seg.sels {
                    apply_sel(sel, n, root, &mut next);
                }
            }
        }
        nodes = next;
    }
    nodes
}

fn operand_value(o: &Operand, cur: &Value, root: &Value) -> Option<Value> {
    match o {
        Operand::Lit(v) => Some(v.clone()),
        Operand::Regex(r) => Some(Value::String(r.as_str().to_string())),
        Operand::Path(is_root, segs, f) => {
            let base = if *is_root { root } else { cur };
            let nodes = select(segs, base, root);
            let v = if is_definite(segs) { nodes.into_iter().next()? } else { Value::Array(nodes) };
            match f {
                Some(f) => apply_func(f, &v).ok(),
                None => Some(v),
            }
        }
    }
}

fn cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64() == b.as_f64(),
        (Value::Int(_) | Value::Float(_), Value::String(s)) | (Value::String(s), Value::Int(_) | Value::Float(_)) => {
            // Jayway compares a number with a numeric string as numbers
            let n = if let Value::String(_) = a { b } else { a };
            s.parse::<f64>().ok() == n.as_f64()
        }
        _ => a == b,
    }
}

fn eval_filter(f: &FExpr, cur: &Value, root: &Value) -> bool {
    match f {
        FExpr::Or(items) => items.iter().any(|x| eval_filter(x, cur, root)),
        FExpr::And(items) => items.iter().all(|x| eval_filter(x, cur, root)),
        FExpr::Not(x) => !eval_filter(x, cur, root),
        FExpr::Exists(o) => match operand_value(o, cur, root) {
            None => false,
            Some(Value::Bool(b)) => b,
            Some(Value::Array(a)) if !matches!(o, Operand::Lit(_)) => match o {
                Operand::Path(_, segs, _) if !is_definite(segs) => !a.is_empty(),
                _ => true,
            },
            Some(_) => true,
        },
        FExpr::Cmp(l, op, r) => {
            let lv = operand_value(l, cur, root);
            if let (FOp::Regex, Operand::Regex(re)) = (op, r) {
                return match lv {
                    Some(Value::String(s)) => re.is_full_match(&s),
                    Some(Value::Int(_)) | Some(Value::Float(_)) => re.is_full_match(&lv.unwrap().to_string()),
                    _ => false,
                };
            }
            let rv = operand_value(r, cur, root);
            let (lv, rv) = match (lv, rv) {
                (Some(a), Some(b)) => (a, b),
                (None, Some(Value::Null)) | (Some(Value::Null), None) => return matches!(op, FOp::Eq),
                (None, _) | (_, None) => return false,
            };
            use std::cmp::Ordering::*;
            match op {
                FOp::Eq => eq(&lv, &rv),
                FOp::Ne => !eq(&lv, &rv),
                FOp::Lt => cmp(&lv, &rv) == Some(Less),
                FOp::Le => matches!(cmp(&lv, &rv), Some(Less | Equal)),
                FOp::Gt => cmp(&lv, &rv) == Some(Greater),
                FOp::Ge => matches!(cmp(&lv, &rv), Some(Greater | Equal)),
                FOp::Regex => match Regex::new(&rv.to_string()) {
                    Ok(re) => re.is_full_match(&lv.to_string()),
                    Err(_) => false,
                },
                FOp::In => rv.as_array().map(|a| a.iter().any(|x| eq(x, &lv))).unwrap_or(false),
                FOp::Nin => rv.as_array().map(|a| !a.iter().any(|x| eq(x, &lv))).unwrap_or(true),
                FOp::Contains => match &lv {
                    Value::Array(a) => a.iter().any(|x| eq(x, &rv)),
                    Value::String(s) => s.contains(&rv.to_string()),
                    _ => false,
                },
                FOp::Size => lv.len().map(|n| Some(n as i64) == rv.as_i64()).unwrap_or(false),
                FOp::Empty => {
                    let e = lv.len().map(|n| n == 0).unwrap_or(false);
                    rv.as_bool().map(|b| b == e).unwrap_or(false)
                }
                FOp::AnyOf => match (&lv, &rv) {
                    (Value::Array(a), Value::Array(b)) => a.iter().any(|x| b.iter().any(|y| eq(x, y))),
                    _ => false,
                },
                FOp::NoneOf => match (&lv, &rv) {
                    (Value::Array(a), Value::Array(b)) => !a.iter().any(|x| b.iter().any(|y| eq(x, y))),
                    _ => false,
                },
                FOp::SubsetOf => match (&lv, &rv) {
                    (Value::Array(a), Value::Array(b)) => a.iter().all(|x| b.iter().any(|y| eq(x, y))),
                    _ => false,
                },
            }
        }
    }
}

/// Evaluate a JSONPath against a value (used by the Simple `${jsonpath(...)}` function).
pub fn query(input: &Value, path: &str) -> Result<Value> {
    JsonPath::compile(path)?.evaluate(input)
}

/// Resolve the language input from options `headerName` / `source`.
pub(crate) fn source_value(ex: &Exchange, options: &BTreeMap<String, String>) -> Value {
    if let Some(h) = options.get("headerName").filter(|h| !h.is_empty()) {
        return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
    }
    if let Some(p) = options.get("propertyName").filter(|h| !h.is_empty()) {
        return ex.properties.get(p).cloned().unwrap_or(Value::Null);
    }
    if let Some(v) = options.get("variableName").filter(|h| !h.is_empty()) {
        return ex.properties.get(&format!("CamelVariable.{}", v)).cloned().unwrap_or(Value::Null);
    }
    if let Some(src) = options.get("source").filter(|h| !h.is_empty()) {
        if let Some(h) = src.strip_prefix("header:") {
            return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
        }
        if let Some(p) = src.strip_prefix("property:").or_else(|| src.strip_prefix("exchangeProperty:")) {
            return ex.properties.get(p).cloned().unwrap_or(Value::Null);
        }
        if let Some(v) = src.strip_prefix("variable:") {
            return ex.properties.get(&format!("CamelVariable.{}", v)).cloned().unwrap_or(Value::Null);
        }
        if src == "body" {
            return ex.message.body.clone();
        }
        return ex.message.headers.get(src).cloned().unwrap_or(Value::Null);
    }
    ex.message.body.clone()
}

fn opt_bool(options: &BTreeMap<String, String>, key: &str) -> bool {
    options.get(key).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

struct JsonPathExpression {
    path: JsonPath,
    options: BTreeMap<String, String>,
}

impl JsonPathExpression {
    fn eval(&self, ex: &Exchange) -> Result<Value> {
        let input = source_value(ex, &self.options);
        if input.is_null() {
            if opt_bool(&self.options, "suppressExceptions") || opt_bool(&self.options, "allowSimple") {
                return Ok(Value::Null);
            }
            return Err(CamelError::new(PATH_NOT_FOUND, format!("No input to evaluate JsonPath {} (body is null)", self.path.as_str())));
        }
        let mut v = match self.path.evaluate(&input) {
            Ok(v) => v,
            Err(e) if e.kind == PATH_NOT_FOUND && opt_bool(&self.options, "suppressExceptions") => Value::Null,
            Err(e) => return Err(e),
        };
        if opt_bool(&self.options, "unpackArray") {
            if let Value::Array(a) = &v {
                if a.len() == 1 {
                    v = a[0].clone();
                }
            }
        }
        if opt_bool(&self.options, "writeAsString") {
            if let Value::Object(_) | Value::Array(_) = v {
                v = Value::String(v.to_json());
            }
        }
        if let Some(t) = self.options.get("resultType").filter(|t| !t.is_empty()) {
            v = super::support::convert_to_type(&v, t)?;
        }
        Ok(v)
    }
}

impl Expression for JsonPathExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        self.eval(ex)
    }
}

impl Predicate for JsonPathExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        match self.eval(ex) {
            Ok(v) => Ok(super::support::value_predicate(&v)),
            Err(e) if e.kind == PATH_NOT_FOUND => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// The `jsonpath` language.
#[derive(Default)]
pub struct JsonPathLanguage;

impl Language for JsonPathLanguage {
    fn name(&self) -> &str {
        "jsonpath"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(JsonPathExpression { path: JsonPath::compile(text)?, options: options.clone() }))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(JsonPathExpression { path: JsonPath::compile(text)?, options: options.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let doc = crate::json::parse(r#"{"a":{"b":[1,2,3]},"c":"x"}"#).unwrap();
        assert_eq!(query(&doc, "$.a.b[1]").unwrap(), Value::Int(2));
        assert_eq!(query(&doc, "$.a.b[*]").unwrap(), Value::from(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(query(&doc, "$['c']").unwrap(), Value::from("x"));
        assert!(query(&doc, "$.nope").unwrap_err().kind == PATH_NOT_FOUND);
        assert_eq!(query(&doc, "$.a.b.length()").unwrap(), Value::Int(3));
        assert!(JsonPath::compile("$.a[").is_err());
    }
}
