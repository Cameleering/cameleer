//! JMS message selectors (the SQL-92 conditional expression subset of the JMS spec).
//!
//! Supported: literals (strings with `''` escapes, integers, decimals, exponents, `TRUE`,
//! `FALSE`), identifiers (header/property names), parentheses, arithmetic `+ - * /` and
//! unary minus, comparisons `= <> < > <= >=`, `AND` / `OR` / `NOT` with SQL three-valued
//! logic, `[NOT] LIKE 'pattern' [ESCAPE 'c']` (`%`, `_`), `[NOT] IN ('a', 'b')`,
//! `[NOT] BETWEEN x AND y` and `IS [NOT] NULL`. Keywords are case-insensitive.
//!
//! A message matches only when the selector evaluates to TRUE (unknown/NULL does not
//! match). Because STOMP carries every property as text, a string that looks like a number
//! is compared numerically against numeric literals (`price > 10` works on `price:12`).

use std::fmt;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::Value;

/// Exception kind for a selector syntax error.
pub const INVALID_SELECTOR: &str = "InvalidSelectorException";

#[derive(Clone, Debug, PartialEq)]
enum Lit {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Debug)]
enum Expr {
    Lit(Lit),
    Ident(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    Arith(char, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Like { e: Box<Expr>, pattern: Vec<LikeTok>, negated: bool },
    In { e: Box<Expr>, list: Vec<Lit>, negated: bool },
    Between { e: Box<Expr>, lo: Box<Expr>, hi: Box<Expr>, negated: bool },
    IsNull { e: Box<Expr>, negated: bool },
}

#[derive(Clone, Debug, PartialEq)]
enum LikeTok {
    Any,
    One,
    Char(char),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    Op(&'static str),
    Kw(&'static str),
}

const KEYWORDS: &[&str] = &["AND", "OR", "NOT", "LIKE", "ESCAPE", "IN", "BETWEEN", "IS", "NULL", "TRUE", "FALSE"];

fn syntax(msg: impl Into<String>, text: &str) -> CamelError {
    CamelError::new(INVALID_SELECTOR, format!("{} in selector: {}", msg.into(), text))
}

fn lex(text: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(syntax("Unterminated string literal", text)),
                    Some('\'') if chars.get(i + 1) == Some(&'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(ch) => {
                        s.push(*ch);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(s));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            if c == '0' && matches!(chars.get(i + 1), Some('x') | Some('X')) {
                i += 2;
                let hs = i;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let h: String = chars[hs..i].iter().collect();
                let v = i64::from_str_radix(&h, 16).map_err(|_| syntax("Invalid hex literal", text))?;
                out.push(Tok::Num(v as f64));
                continue;
            }
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let save = i;
                i += 1;
                if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                if i < chars.len() && chars[i].is_ascii_digit() {
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let n: String = chars[start..i].iter().collect();
            let v: f64 = n.parse().map_err(|_| syntax(format!("Invalid number '{}'", n), text))?;
            // Java long/float suffixes
            if i < chars.len() && matches!(chars[i], 'l' | 'L' | 'f' | 'F' | 'd' | 'D') {
                i += 1;
            }
            out.push(Tok::Num(v));
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$' || chars[i] == '.') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let upper = word.to_ascii_uppercase();
            match KEYWORDS.iter().find(|k| **k == upper) {
                Some(k) => out.push(Tok::Kw(k)),
                None => out.push(Tok::Ident(word)),
            }
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        let (op, width): (&'static str, usize) = match two.as_str() {
            "<>" => ("<>", 2),
            "<=" => ("<=", 2),
            ">=" => (">=", 2),
            "!=" => ("<>", 2),
            _ => (match c {
                '=' => "=",
                '<' => "<",
                '>' => ">",
                '+' => "+",
                '-' => "-",
                '*' => "*",
                '/' => "/",
                '(' => "(",
                ')' => ")",
                ',' => ",",
                _ => return Err(syntax(format!("Unexpected character '{}'", c), text)),
            }, 1),
        };
        i += width;
        out.push(Tok::Op(op));
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    text: &'a str,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Kw(k)) if *k == kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn err(&self, msg: &str) -> CamelError {
        syntax(format!("{} at token {}", msg, self.pos + 1), self.text)
    }

    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.eat_kw("OR") {
            let r = self.and()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.not()?;
        while self.eat_kw("AND") {
            let r = self.not()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.comparison()
    }
    fn comparison(&mut self) -> Result<Expr> {
        let l = self.additive()?;
        let op = match self.peek() {
            Some(Tok::Op("=")) => Some(CmpOp::Eq),
            Some(Tok::Op("<>")) => Some(CmpOp::Ne),
            Some(Tok::Op("<")) => Some(CmpOp::Lt),
            Some(Tok::Op(">")) => Some(CmpOp::Gt),
            Some(Tok::Op("<=")) => Some(CmpOp::Le),
            Some(Tok::Op(">=")) => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let r = self.additive()?;
            return Ok(Expr::Cmp(op, Box::new(l), Box::new(r)));
        }
        if self.eat_kw("IS") {
            let negated = self.eat_kw("NOT");
            if !self.eat_kw("NULL") {
                return Err(self.err("Expected NULL after IS"));
            }
            return Ok(Expr::IsNull { e: Box::new(l), negated });
        }
        let negated = self.eat_kw("NOT");
        if self.eat_kw("LIKE") {
            let Some(Tok::Str(p)) = self.next() else { return Err(self.err("Expected string pattern after LIKE")) };
            let escape = if self.eat_kw("ESCAPE") {
                match self.next() {
                    Some(Tok::Str(e)) if e.chars().count() == 1 => e.chars().next(),
                    _ => return Err(self.err("ESCAPE expects a single character string")),
                }
            } else {
                None
            };
            let mut pattern = Vec::new();
            let mut it = p.chars();
            while let Some(c) = it.next() {
                if Some(c) == escape {
                    match it.next() {
                        Some(n) => pattern.push(LikeTok::Char(n)),
                        None => return Err(self.err("Dangling escape character in LIKE pattern")),
                    }
                } else if c == '%' {
                    pattern.push(LikeTok::Any);
                } else if c == '_' {
                    pattern.push(LikeTok::One);
                } else {
                    pattern.push(LikeTok::Char(c));
                }
            }
            return Ok(Expr::Like { e: Box::new(l), pattern, negated });
        }
        if self.eat_kw("IN") {
            if !self.eat_op("(") {
                return Err(self.err("Expected ( after IN"));
            }
            let mut list = Vec::new();
            loop {
                match self.next() {
                    Some(Tok::Str(s)) => list.push(Lit::Str(s)),
                    Some(Tok::Num(n)) => list.push(Lit::Num(n)),
                    _ => return Err(self.err("IN expects a list of literals")),
                }
                if self.eat_op(",") {
                    continue;
                }
                if self.eat_op(")") {
                    break;
                }
                return Err(self.err("Expected , or ) in IN list"));
            }
            return Ok(Expr::In { e: Box::new(l), list, negated });
        }
        if self.eat_kw("BETWEEN") {
            let lo = self.additive()?;
            if !self.eat_kw("AND") {
                return Err(self.err("Expected AND in BETWEEN"));
            }
            let hi = self.additive()?;
            return Ok(Expr::Between { e: Box::new(l), lo: Box::new(lo), hi: Box::new(hi), negated });
        }
        if negated {
            return Err(self.err("Expected LIKE, IN or BETWEEN after NOT"));
        }
        Ok(l)
    }
    fn additive(&mut self) -> Result<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            let op = if self.eat_op("+") {
                '+'
            } else if self.eat_op("-") {
                '-'
            } else {
                break;
            };
            let r = self.multiplicative()?;
            l = Expr::Arith(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn multiplicative(&mut self) -> Result<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = if self.eat_op("*") {
                '*'
            } else if self.eat_op("/") {
                '/'
            } else {
                break;
            };
            let r = self.unary()?;
            l = Expr::Arith(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr> {
        if self.eat_op("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op("+") {
            return self.unary();
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::Op("(")) => {
                let e = self.or()?;
                if !self.eat_op(")") {
                    return Err(self.err("Expected )"));
                }
                Ok(e)
            }
            Some(Tok::Str(s)) => Ok(Expr::Lit(Lit::Str(s))),
            Some(Tok::Num(n)) => Ok(Expr::Lit(Lit::Num(n))),
            Some(Tok::Kw("TRUE")) => Ok(Expr::Lit(Lit::Bool(true))),
            Some(Tok::Kw("FALSE")) => Ok(Expr::Lit(Lit::Bool(false))),
            Some(Tok::Kw("NULL")) => Ok(Expr::Lit(Lit::Null)),
            Some(Tok::Ident(i)) => Ok(Expr::Ident(i)),
            Some(t) => Err(self.err(&format!("Unexpected token {:?}", t))),
            None => Err(self.err("Unexpected end of selector")),
        }
    }
}

/// A parsed JMS message selector.
#[derive(Clone)]
pub struct Selector {
    text: String,
    expr: Expr,
}

impl fmt::Debug for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Selector({})", self.text)
    }
}

fn to_lit(v: Option<Value>) -> Lit {
    match v {
        None | Some(Value::Null) => Lit::Null,
        Some(Value::Bool(b)) => Lit::Bool(b),
        Some(Value::Int(i)) => Lit::Num(i as f64),
        Some(Value::Float(f)) => Lit::Num(f),
        Some(Value::String(s)) => Lit::Str(s),
        Some(other) => Lit::Str(other.to_string()),
    }
}

/// Numeric view of a literal (numeric strings count, for STOMP's text-only properties).
fn num(l: &Lit) -> Option<f64> {
    match l {
        Lit::Num(n) => Some(*n),
        Lit::Str(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn tri_and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn tri_or(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

fn like(p: &[LikeTok], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(LikeTok::Any) => (0..=s.len()).any(|i| like(&p[1..], &s[i..])),
        Some(LikeTok::One) => !s.is_empty() && like(&p[1..], &s[1..]),
        Some(LikeTok::Char(c)) => !s.is_empty() && s[0] == *c && like(&p[1..], &s[1..]),
    }
}

fn equal(a: &Lit, b: &Lit) -> Option<bool> {
    match (a, b) {
        (Lit::Null, _) | (_, Lit::Null) => None,
        (Lit::Num(x), _) | (_, Lit::Num(x)) => {
            let other = if matches!(a, Lit::Num(_)) { b } else { a };
            num(other).map(|y| (*x - y).abs() < f64::EPSILON * x.abs().max(1.0)).or(Some(false))
        }
        (Lit::Str(x), Lit::Str(y)) => Some(x == y),
        (Lit::Bool(x), Lit::Bool(y)) => Some(x == y),
        (Lit::Bool(x), Lit::Str(s)) | (Lit::Str(s), Lit::Bool(x)) => Some(s.eq_ignore_ascii_case(if *x { "true" } else { "false" })),
    }
}

impl Selector {
    /// Parse a selector; fails with `InvalidSelectorException`.
    pub fn parse(text: &str) -> Result<Selector> {
        let toks = lex(text)?;
        if toks.is_empty() {
            return Err(syntax("Empty selector", text));
        }
        let mut p = Parser { toks, pos: 0, text };
        let expr = p.or()?;
        if p.pos < p.toks.len() {
            return Err(p.err("Unexpected trailing input"));
        }
        Ok(Selector { text: text.to_string(), expr })
    }

    /// The selector text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Evaluate against a property lookup; true only when the selector is TRUE.
    pub fn matches(&self, lookup: &dyn Fn(&str) -> Option<Value>) -> bool {
        matches!(self.eval(&self.expr, lookup), Lit::Bool(true))
    }

    fn truth(&self, e: &Expr, lookup: &dyn Fn(&str) -> Option<Value>) -> Option<bool> {
        match self.eval(e, lookup) {
            Lit::Bool(b) => Some(b),
            Lit::Str(s) if s.eq_ignore_ascii_case("true") => Some(true),
            Lit::Str(s) if s.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        }
    }

    fn eval(&self, e: &Expr, lookup: &dyn Fn(&str) -> Option<Value>) -> Lit {
        let b = |v: Option<bool>| v.map(Lit::Bool).unwrap_or(Lit::Null);
        match e {
            Expr::Lit(l) => l.clone(),
            Expr::Ident(name) => to_lit(lookup(name)),
            Expr::Not(x) => b(self.truth(x, lookup).map(|v| !v)),
            Expr::And(x, y) => b(tri_and(self.truth(x, lookup), self.truth(y, lookup))),
            Expr::Or(x, y) => b(tri_or(self.truth(x, lookup), self.truth(y, lookup))),
            Expr::Cmp(op, x, y) => {
                let (l, r) = (self.eval(x, lookup), self.eval(y, lookup));
                match op {
                    CmpOp::Eq => b(equal(&l, &r)),
                    CmpOp::Ne => b(equal(&l, &r).map(|v| !v)),
                    _ => {
                        if matches!(l, Lit::Null) || matches!(r, Lit::Null) {
                            return Lit::Null;
                        }
                        let ord = match (num(&l), num(&r)) {
                            (Some(x), Some(y)) => x.partial_cmp(&y),
                            _ => match (&l, &r) {
                                (Lit::Str(x), Lit::Str(y)) => Some(x.cmp(y)),
                                _ => None,
                            },
                        };
                        let Some(o) = ord else { return Lit::Null };
                        use std::cmp::Ordering::*;
                        Lit::Bool(match op {
                            CmpOp::Lt => o == Less,
                            CmpOp::Gt => o == Greater,
                            CmpOp::Le => o != Greater,
                            CmpOp::Ge => o != Less,
                            _ => unreachable!(),
                        })
                    }
                }
            }
            Expr::Arith(op, x, y) => match (num(&self.eval(x, lookup)), num(&self.eval(y, lookup))) {
                (Some(a), Some(c)) => Lit::Num(match op {
                    '+' => a + c,
                    '-' => a - c,
                    '*' => a * c,
                    _ => {
                        if c == 0.0 {
                            return Lit::Null;
                        }
                        a / c
                    }
                }),
                _ => Lit::Null,
            },
            Expr::Neg(x) => num(&self.eval(x, lookup)).map(|n| Lit::Num(-n)).unwrap_or(Lit::Null),
            Expr::Like { e, pattern, negated } => match self.eval(e, lookup) {
                Lit::Str(s) => {
                    let chars: Vec<char> = s.chars().collect();
                    Lit::Bool(like(pattern, &chars) != *negated)
                }
                Lit::Null => Lit::Null,
                other => {
                    let s = match other {
                        Lit::Num(n) => n.to_string(),
                        Lit::Bool(v) => v.to_string(),
                        _ => String::new(),
                    };
                    let chars: Vec<char> = s.chars().collect();
                    Lit::Bool(like(pattern, &chars) != *negated)
                }
            },
            Expr::In { e, list, negated } => {
                let v = self.eval(e, lookup);
                if matches!(v, Lit::Null) {
                    return Lit::Null;
                }
                let found = list.iter().any(|item| equal(&v, item) == Some(true));
                Lit::Bool(found != *negated)
            }
            Expr::Between { e, lo, hi, negated } => match (num(&self.eval(e, lookup)), num(&self.eval(lo, lookup)), num(&self.eval(hi, lookup))) {
                (Some(v), Some(l), Some(h)) => Lit::Bool((v >= l && v <= h) != *negated),
                _ => Lit::Null,
            },
            Expr::IsNull { e, negated } => Lit::Bool(matches!(self.eval(e, lookup), Lit::Null) != *negated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn m(sel: &str, props: &[(&str, Value)]) -> bool {
        let map: HashMap<String, Value> = props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        Selector::parse(sel).unwrap().matches(&|k| map.get(k).cloned())
    }

    #[test]
    fn comparisons_and_logic() {
        let p = [("color", Value::from("red")), ("price", Value::from("12.5")), ("qty", Value::Int(3)), ("vip", Value::from("true"))];
        assert!(m("color = 'red'", &p));
        assert!(!m("color = 'blue'", &p));
        assert!(m("color <> 'blue'", &p));
        assert!(m("price > 10", &p));
        assert!(m("price > 10 AND qty <= 3", &p));
        assert!(!m("price > 10 AND qty < 3", &p));
        assert!(m("price < 10 OR qty = 3", &p));
        assert!(m("NOT (color = 'blue')", &p));
        assert!(m("qty * 2 + 1 = 7", &p));
        assert!(m("-qty < 0", &p));
        assert!(m("vip = TRUE", &p));
        assert!(m("qty = 3.0", &p));
        assert!(m("price >= 1.25E1", &p));
        assert!(m("color = 'red' and qty in (1, 2, 3)", &p), "keywords are case-insensitive");
    }

    #[test]
    fn three_valued_logic_with_missing_properties() {
        let p = [("a", Value::Int(1))];
        assert!(!m("missing = 'x'", &p));
        assert!(!m("missing <> 'x'", &p), "unknown is not true");
        assert!(!m("NOT (missing = 'x')", &p));
        assert!(m("missing = 'x' OR a = 1", &p));
        assert!(!m("missing = 'x' AND a = 1", &p));
        assert!(m("missing IS NULL", &p));
        assert!(m("a IS NOT NULL", &p));
        assert!(!m("missing > 3", &p));
    }

    #[test]
    fn like_in_between() {
        let p = [("name", Value::from("order_42")), ("region", Value::from("EU")), ("n", Value::from("7"))];
        assert!(m("name LIKE 'order%'", &p));
        assert!(m("name LIKE 'order\\_4_' ESCAPE '\\'", &p));
        assert!(!m("name LIKE 'orderX%'", &p));
        assert!(m("name NOT LIKE '%x%'", &p));
        assert!(m("region IN ('EU', 'US')", &p));
        assert!(!m("region NOT IN ('EU', 'US')", &p));
        assert!(m("n BETWEEN 5 AND 10", &p));
        assert!(!m("n NOT BETWEEN 5 AND 10", &p));
        assert!(m("'it''s' = 'it''s'", &p));
        assert!(m("JMSType IS NULL", &p));
    }

    #[test]
    fn syntax_errors() {
        for bad in ["", "a =", "a = 'x", "a LIKE 5", "a IN 1", "(a = 1", "a = 1 b", "a NOT 1", "a BETWEEN 1 2", "a ESCAPE", "#"] {
            let e = Selector::parse(bad).unwrap_err();
            assert_eq!(e.kind, INVALID_SELECTOR, "{}", bad);
        }
    }
}
