//! The Simple language (Camel's `simple`): text templates with `${...}` functions and
//! a predicate syntax with binary/logical operators.
//!
//! Expressions are parsed once (at `create_expression`/`create_predicate` time) into an
//! AST; syntax errors are reported as `ExpressionIllegalSyntaxException` with the
//! location of the problem.
//!
//! Supported (summary):
//! - `${body}`, `${in.body}`, `${bodyAs(Type)}`, `${mandatoryBodyAs(Type)}`, `${bodyOneLine}`,
//!   `${bodyType}`, OGNL navigation `${body.address.city}`, `${body[0]}`, `${body?.x}`,
//!   methods `${body.length()}`, `${body.toUpperCase()}`, ...
//! - `${header.x}`, `${headers.x}`, `${header[x]}`, `${header:x}`, `${in.header.x}`, `${headers}`,
//!   `${headerAs(name,Type)}`, `${exchangeProperty.x}`, `${variable.x}`, `${variables}`
//! - `${exchangeId}`, `${id}`/`${messageId}`, `${messageTimestamp}`, `${routeId}`,
//!   `${fromRouteId}`, `${camelId}`, `${exception}`, `${exception.message}`,
//!   `${exception.kind}`/`${exception.class}`, `${exception.stacktrace}`
//! - `${date:now:pattern}`, `${date:now-1h:pattern}`, `${date:header.x:pattern}`, `${date:millis}`,
//!   `${date-with-timezone:now:GMT+2:pattern}`
//! - `${random(max)}`, `${random(min,max)}`, `${properties:key:default}`, `${env.NAME}`,
//!   `${sys.name}`, `${null}`, `${uuid}`, `${ref:name}`, `${bean:name.method}`
//! - Camel 4 functions: `collate(n)`, `skip(n)`, `join(sep,prefix,exp)`, `replace(from,to,exp)`,
//!   `substring(head,tail,exp)`, `substringBefore`, `substringAfter`, `substringBetween`, `trim`,
//!   `uppercase`, `lowercase`, `length`, `size`, `sum`, `max`, `min`, `average`, `abs`, `floor`,
//!   `ceil`, `round`, `iif(pred,a,b)`, `split`, `hash`, `base64Encode`, `base64Decode`,
//!   `concat`, `isEmpty`, `not`, `normalizeWhitespace`, `pretty`, `prettyBody`, `empty`,
//!   `list`, `map`, `jsonpath`, `jq`, `xpath`
//! - ternary `${header.x > 5 ? 'big' : 'small'}`, Elvis `${header.x ?: 'default'}`,
//!   unary `${header.n}++` / `${header.n}--`
//! - predicates: `== != > >= < <= =~ !=~ contains !contains ~~ !~~ regex !regex in !in
//!   is !is range !range startsWith endsWith` combined with `&&`/`||` (`and`/`or`)

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::regex::Regex;
use crate::spi::{Expression, Language, Predicate};
use crate::value::{compare_values, values_equal, Value};

use super::support::{self, value_predicate};

/// The `simple` language.
#[derive(Default)]
pub struct SimpleLanguage;

impl Language for SimpleLanguage {
    fn name(&self) -> &str {
        "simple"
    }
    fn create_expression(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(SimpleExpression::parse(text)?))
    }
    fn create_predicate(&self, text: &str, _o: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(SimplePredicate::parse(text)?))
    }
}

/// A parsed Simple expression (template).
pub struct SimpleExpression {
    text: String,
    expr: Expr,
}

impl SimpleExpression {
    /// Parse a Simple expression (template text with `${...}` functions).
    pub fn parse(text: &str) -> Result<Self> {
        let p = P::new(text);
        let expr = p.template(0, p.chars.len())?;
        Ok(SimpleExpression { text: text.to_string(), expr })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Expression for SimpleExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        self.expr.eval(ex)
    }
}

/// A parsed Simple predicate.
pub struct SimplePredicate {
    text: String,
    pred: Pred,
}

impl SimplePredicate {
    /// Parse a Simple predicate such as `${header.foo} == 'bar' && ${body} contains 'x'`.
    pub fn parse(text: &str) -> Result<Self> {
        let p = P::new(text);
        let pred = p.predicate(0, p.chars.len(), false)?;
        Ok(SimplePredicate { text: text.to_string(), pred })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Predicate for SimplePredicate {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        self.pred.eval(ex)
    }
}

/// Evaluate a Simple expression once against an exchange (parses every call).
pub fn evaluate(text: &str, ex: &Exchange) -> Result<Value> {
    SimpleExpression::parse(text)?.evaluate(ex)
}

/// Evaluate a Simple predicate once against an exchange (parses every call).
pub fn matches(text: &str, ex: &Exchange) -> Result<bool> {
    SimplePredicate::parse(text)?.matches(ex)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
enum Expr {
    Literal(Value),
    Template(Vec<Part>),
    Func(Box<Func>),
    /// Function whose text contains nested functions: evaluated to text, then parsed.
    Dynamic(Box<Expr>),
    Ternary(Box<Pred>, Box<Expr>, Box<Expr>),
    Elvis(Box<Expr>, Box<Expr>),
    Unary(Box<Expr>, i64),
}

#[derive(Debug, Clone)]
enum Nav {
    Key(String, bool),
    Index(String, bool),
    Method(String, Vec<Expr>, bool),
}

#[derive(Debug, Clone)]
enum Scope {
    Header,
    Property,
    Variable,
}

#[derive(Debug, Clone)]
enum DateSource {
    Now,
    Millis,
    Created,
    Header(String),
    Property(String),
    Variable(String),
}

#[derive(Debug, Clone)]
enum Func {
    Body(Vec<Nav>),
    BodyAs(String, bool, Vec<Nav>),
    BodyOneLine,
    BodyType,
    /// Scoped lookup: full name, alternative (base name, navigation).
    Scoped(Scope, String, Option<(String, Vec<Nav>)>),
    ScopedAs(Scope, Expr, String),
    Headers,
    Variables,
    ExchangeId,
    MessageId,
    MessageTimestamp,
    RouteId,
    FromRouteId,
    CamelId,
    Exception(Vec<Nav>),
    Date { source: DateSource, offset: i64, pattern: Option<String>, tz: i64 },
    Properties(String, Option<String>),
    PropertiesExist(String),
    Env(String),
    Sys(String),
    Null,
    Ref(String),
    Bean(String, Option<String>),
    Iif(Pred, Expr, Expr),
    Not(Pred),
    Call(String, Vec<Expr>, Vec<Nav>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    EqIc,
    Ne,
    NeIc,
    Gt,
    Ge,
    Lt,
    Le,
    Contains(bool),
    ContainsIc(bool),
    Regex(bool),
    In(bool),
    Is(bool),
    Range(bool),
    StartsWith(bool),
    EndsWith(bool),
}

#[derive(Debug, Clone)]
enum Pred {
    Or(Vec<Pred>),
    And(Vec<Pred>),
    Cmp(Expr, Op, Expr, Option<Regex>),
    Truthy(Expr),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct P<'a> {
    text: &'a str,
    chars: Vec<char>,
}

fn s(chars: &[char]) -> String {
    chars.iter().collect()
}

impl<'a> P<'a> {
    fn new(text: &'a str) -> Self {
        P { text, chars: text.chars().collect() }
    }

    fn err(&self, msg: impl std::fmt::Display, pos: usize) -> CamelError {
        CamelError::syntax(format!("{} at location {}\n{}\n{}*", msg, pos, self.text, " ".repeat(pos)))
    }

    /// Index of the `}` closing a function block whose content starts at `start`.
    fn find_close(&self, start: usize, end: usize) -> Option<usize> {
        let c = &self.chars;
        let mut depth = 1;
        let mut i = start;
        while i < end {
            match c[i] {
                '\'' | '"' => {
                    let q = c[i];
                    let mut j = i + 1;
                    while j < end && c[j] != q {
                        j += 1;
                    }
                    if j < end {
                        i = j;
                    }
                }
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Starts a function block at `i`? Returns the index where the content starts.
    fn block_start(&self, i: usize, end: usize) -> Option<usize> {
        let c = &self.chars;
        if i + 1 < end && c[i] == '$' && c[i + 1] == '{' {
            return Some(i + 2);
        }
        if i + 7 < end && c[i] == '$' && s(&c[i..i + 8]) == "$simple{" {
            return Some(i + 8);
        }
        None
    }

    /// Parse template text in `[start, end)`.
    fn template(&self, start: usize, end: usize) -> Result<Expr> {
        let c = &self.chars;
        let mut parts: Vec<Part> = Vec::new();
        let mut buf = String::new();
        let mut i = start;
        while i < end {
            let ch = c[i];
            if ch == '\\' && i + 1 < end {
                match c[i + 1] {
                    'n' => {
                        buf.push('\n');
                        i += 2;
                        continue;
                    }
                    't' => {
                        buf.push('\t');
                        i += 2;
                        continue;
                    }
                    'r' => {
                        buf.push('\r');
                        i += 2;
                        continue;
                    }
                    '$' if i + 2 < end && c[i + 2] == '{' => {
                        buf.push_str("${");
                        i += 3;
                        continue;
                    }
                    '}' => {
                        buf.push('}');
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            if ch == '$' && i + 2 < end && c[i + 1] == '$' && c[i + 2] == '{' {
                buf.push_str("${");
                i += 3;
                continue;
            }
            if let Some(cs) = self.block_start(i, end) {
                let close = self.find_close(cs, end).ok_or_else(|| self.err("expected symbol functionEnd but was eol", i))?;
                let mut e = self.block(cs, close)?;
                let mut next = close + 1;
                // unary ++ / --
                if next + 1 < end + 1 && next + 2 <= end {
                    let op = s(&c[next..next + 2]);
                    let after_ok = next + 2 == end || c[next + 2].is_whitespace();
                    if (op == "++" || op == "--") && after_ok {
                        e = Expr::Unary(Box::new(e), if op == "++" { 1 } else { -1 });
                        next += 2;
                    }
                }
                if !buf.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut buf)));
                }
                parts.push(Part::Expr(e));
                i = next;
                continue;
            }
            buf.push(ch);
            i += 1;
        }
        if !buf.is_empty() {
            parts.push(Part::Text(buf));
        }
        Ok(match parts.len() {
            0 => Expr::Literal(Value::String(String::new())),
            1 => match parts.pop().unwrap() {
                Part::Text(t) => Expr::Literal(Value::String(t)),
                Part::Expr(e) => e,
            },
            _ => Expr::Template(parts),
        })
    }

    /// Find a top-level occurrence of `pat` in `[start,end)` (outside quotes/brackets/blocks).
    fn find_top(&self, pat: &str, start: usize, end: usize) -> Option<usize> {
        let c = &self.chars;
        let p: Vec<char> = pat.chars().collect();
        let mut depth = 0i32;
        let mut i = start;
        while i < end {
            match c[i] {
                '\'' | '"' => {
                    let q = c[i];
                    let mut j = i + 1;
                    while j < end && c[j] != q {
                        j += 1;
                    }
                    i = j + 1;
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 && i + p.len() <= end && c[i..i + p.len()] == p[..] {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Parse the content of a `${...}` block in `[start, end)`.
    fn block(&self, start: usize, end: usize) -> Result<Expr> {
        let (start, end) = self.trim(start, end);
        if start >= end {
            return Err(self.err("Function is empty", start));
        }
        // ternary / elvis
        if let Some(q) = self.find_top(" ?: ", start, end) {
            let left = self.operand_range(start, q, true)?;
            let right = self.operand_range(q + 4, end, true)?;
            return Ok(Expr::Elvis(Box::new(left), Box::new(right)));
        }
        if let Some(q) = self.find_top(" ? ", start, end) {
            let colon = self.find_top(" : ", q + 3, end).ok_or_else(|| self.err("Ternary operator is missing ' : '", q))?;
            let cond = self.predicate(start, q, true)?;
            let a = self.operand_range(q + 3, colon, true)?;
            let b = self.operand_range(colon + 3, end, true)?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(a), Box::new(b)));
        }
        let text = s(&self.chars[start..end]);
        if let Some(f) = self.call_function(start, end)? {
            return Ok(Expr::Func(Box::new(f)));
        }
        if text.contains("${") {
            return Ok(Expr::Dynamic(Box::new(self.template(start, end)?)));
        }
        Ok(Expr::Func(Box::new(self.function(&text, start)?)))
    }

    fn trim(&self, mut start: usize, mut end: usize) -> (usize, usize) {
        while start < end && self.chars[start].is_whitespace() {
            start += 1;
        }
        while end > start && self.chars[end - 1].is_whitespace() {
            end -= 1;
        }
        (start, end)
    }

    /// An operand inside a block (inner mode): quoted literal, number, keyword, nested
    /// block or bare function name.
    fn operand_range(&self, start: usize, end: usize, inner: bool) -> Result<Expr> {
        let (start, end) = self.trim(start, end);
        if start >= end {
            return Err(self.err("Expected operand", start));
        }
        let c = &self.chars;
        if (c[start] == '\'' || c[start] == '"') && end - start >= 2 && c[end - 1] == c[start] {
            return self.template(start + 1, end - 1);
        }
        let text = s(&c[start..end]);
        if let Some(v) = literal_keyword(&text) {
            return Ok(Expr::Literal(v));
        }
        if text.contains("${") {
            return self.template(start, end);
        }
        if inner {
            if let Ok(Some(f)) = self.call_function(start, end) {
                return Ok(Expr::Func(Box::new(f)));
            }
            if let Ok(f) = self.function(&text, start) {
                return Ok(Expr::Func(Box::new(f)));
            }
        }
        Ok(Expr::Literal(Value::String(text)))
    }

    // --- predicates ---

    fn tokens(&self, start: usize, end: usize) -> Result<Vec<(usize, usize, bool)>> {
        // (start, end, quoted)
        let c = &self.chars;
        let mut toks = Vec::new();
        let mut i = start;
        while i < end {
            if c[i].is_whitespace() {
                i += 1;
                continue;
            }
            if c[i] == '\'' || c[i] == '"' {
                let q = c[i];
                let mut j = i + 1;
                while j < end && c[j] != q {
                    if c[j] == '\\' && j + 1 < end && c[j + 1] == q {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= end {
                    return Err(self.err("Unclosed quoted literal", i));
                }
                toks.push((i, j + 1, true));
                i = j + 1;
                continue;
            }
            let ts = i;
            while i < end && !c[i].is_whitespace() {
                if let Some(cs) = self.block_start(i, end) {
                    let close = self.find_close(cs, end).ok_or_else(|| self.err("expected symbol functionEnd but was eol", i))?;
                    i = close + 1;
                    continue;
                }
                i += 1;
            }
            toks.push((ts, i, false));
        }
        Ok(toks)
    }

    fn predicate(&self, start: usize, end: usize, inner: bool) -> Result<Pred> {
        let toks = self.tokens(start, end)?;
        if toks.is_empty() {
            return Err(self.err("Predicate is empty", start));
        }
        let mut pos = 0;
        let p = self.parse_or(&toks, &mut pos, inner)?;
        if pos < toks.len() {
            return Err(self.err(format!("Unexpected token '{}'", s(&self.chars[toks[pos].0..toks[pos].1])), toks[pos].0));
        }
        Ok(p)
    }

    fn tok_text(&self, t: &(usize, usize, bool)) -> String {
        s(&self.chars[t.0..t.1])
    }

    fn parse_or(&self, toks: &[(usize, usize, bool)], pos: &mut usize, inner: bool) -> Result<Pred> {
        let mut items = vec![self.parse_and(toks, pos, inner)?];
        while *pos < toks.len() && !toks[*pos].2 && matches!(self.tok_text(&toks[*pos]).as_str(), "||" | "or") {
            let at = toks[*pos].0;
            *pos += 1;
            if *pos >= toks.len() {
                return Err(self.err("Logical operator || has no right hand side", at));
            }
            items.push(self.parse_and(toks, pos, inner)?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Pred::Or(items) })
    }

    fn parse_and(&self, toks: &[(usize, usize, bool)], pos: &mut usize, inner: bool) -> Result<Pred> {
        let mut items = vec![self.parse_cmp(toks, pos, inner)?];
        while *pos < toks.len() && !toks[*pos].2 && matches!(self.tok_text(&toks[*pos]).as_str(), "&&" | "and") {
            let at = toks[*pos].0;
            *pos += 1;
            if *pos >= toks.len() {
                return Err(self.err("Logical operator && has no right hand side", at));
            }
            items.push(self.parse_cmp(toks, pos, inner)?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Pred::And(items) })
    }

    fn operator(&self, toks: &[(usize, usize, bool)], pos: usize) -> Option<(Op, usize)> {
        let t = |k: usize| -> Option<String> {
            toks.get(pos + k).filter(|t| !t.2).map(|t| self.tok_text(t))
        };
        let first = t(0)?;
        let second = t(1);
        let two = |w: &str| second.as_deref() == Some(w);
        Some(match first.as_str() {
            "==" => (Op::Eq, 1),
            "=~" => (Op::EqIc, 1),
            "!=" => (Op::Ne, 1),
            "!=~" => (Op::NeIc, 1),
            ">" => (Op::Gt, 1),
            ">=" => (Op::Ge, 1),
            "<" => (Op::Lt, 1),
            "<=" => (Op::Le, 1),
            "contains" => (Op::Contains(false), 1),
            "!contains" => (Op::Contains(true), 1),
            "~~" => (Op::ContainsIc(false), 1),
            "!~~" => (Op::ContainsIc(true), 1),
            "regex" => (Op::Regex(false), 1),
            "!regex" => (Op::Regex(true), 1),
            "in" => (Op::In(false), 1),
            "!in" => (Op::In(true), 1),
            "is" => (Op::Is(false), 1),
            "!is" => (Op::Is(true), 1),
            "range" => (Op::Range(false), 1),
            "!range" => (Op::Range(true), 1),
            "startsWith" => (Op::StartsWith(false), 1),
            "!startsWith" => (Op::StartsWith(true), 1),
            "endsWith" => (Op::EndsWith(false), 1),
            "!endsWith" => (Op::EndsWith(true), 1),
            "starts" if two("with") => (Op::StartsWith(false), 2),
            "ends" if two("with") => (Op::EndsWith(false), 2),
            "not" => match second.as_deref() {
                Some("contains") => (Op::Contains(true), 2),
                Some("regex") => (Op::Regex(true), 2),
                Some("in") => (Op::In(true), 2),
                Some("is") => (Op::Is(true), 2),
                Some("range") => (Op::Range(true), 2),
                _ => return None,
            },
            _ => return None,
        })
    }

    fn token_operand(&self, t: &(usize, usize, bool), inner: bool) -> Result<Expr> {
        if t.2 {
            // quoted: unescape \' inside
            let q = self.chars[t.0];
            let content = s(&self.chars[t.0 + 1..t.1 - 1]);
            if content.contains(&format!("\\{}", q)) {
                let un = content.replace(&format!("\\{}", q), &q.to_string());
                let sub = SimpleExpression::parse(&un)?;
                return Ok(sub.expr);
            }
            return self.template(t.0 + 1, t.1 - 1);
        }
        self.operand_range(t.0, t.1, inner)
    }

    fn parse_cmp(&self, toks: &[(usize, usize, bool)], pos: &mut usize, inner: bool) -> Result<Pred> {
        let lt = toks[*pos];
        if !lt.2 {
            let tt = self.tok_text(&lt);
            if matches!(tt.as_str(), "&&" | "||" | "and" | "or") || self.operator(toks, *pos).is_some() && tt != "in" && tt != "is" {
                return Err(self.err(format!("Unexpected operator '{}' without left hand side", tt), lt.0));
            }
        }
        let left = self.token_operand(&lt, inner)?;
        *pos += 1;
        if let Some((op, n)) = self.operator(toks, *pos) {
            let at = toks[*pos].0;
            *pos += n;
            if *pos >= toks.len() {
                return Err(self.err("Binary operator has no right hand side", at));
            }
            let rt = toks[*pos];
            let right = self.token_operand(&rt, inner)?;
            *pos += 1;
            let mut re = None;
            if let (Op::Regex(_), Expr::Literal(v)) = (op, &right) {
                re = Some(Regex::new(&v.to_string()).map_err(|e| self.err(format!("Invalid regex: {}", e.message.lines().next().unwrap_or("")), rt.0))?);
            }
            if let (Op::Is(_), false) = (op, rt.2) {
                // type names are plain text, never functions
                return Ok(Pred::Cmp(left, op, Expr::Literal(Value::String(self.tok_text(&rt))), None));
            }
            return Ok(Pred::Cmp(left, op, right, re));
        }
        Ok(Pred::Truthy(left))
    }

    // --- functions ---

    /// Parse call-form functions `name(args)` optionally followed by navigation.
    fn call_function(&self, start: usize, end: usize) -> Result<Option<Func>> {
        let c = &self.chars;
        let mut i = start;
        while i < end && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
            i += 1;
        }
        if i == start || i >= end || c[i] != '(' {
            return Ok(None);
        }
        let name = s(&c[start..i]);
        let known = CALLS.iter().any(|(n, _, _)| *n == name)
            || matches!(name.as_str(), "bodyAs" | "mandatoryBodyAs" | "headerAs" | "exchangePropertyAs" | "variableAs" | "iif" | "not");
        if !known {
            return Ok(None);
        }
        // matching paren
        let mut depth = 0;
        let mut j = i;
        let mut close = None;
        while j < end {
            match c[j] {
                '\'' | '"' => {
                    let q = c[j];
                    let mut k = j + 1;
                    while k < end && c[k] != q {
                        k += 1;
                    }
                    j = k;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let close = close.ok_or_else(|| self.err(format!("Function {} is missing closing parenthesis", name), i))?;
        let args = self.split_args(i + 1, close);
        let navs = self.navs(close + 1, end)?;
        let arg_expr = |k: usize| -> Result<Expr> {
            let (a, b) = args[k];
            self.arg(a, b)
        };
        let raw_arg = |k: usize| -> String {
            let (a, b) = args[k];
            let t = s(&c[a..b]);
            let t = t.trim();
            unquote(t).to_string()
        };
        let nargs = args.len();
        let f = match name.as_str() {
            "bodyAs" | "mandatoryBodyAs" => {
                if nargs != 1 {
                    return Err(self.err(format!("Valid syntax: ${{{}(type)}} was: {}", name, s(&c[start..end])), start));
                }
                Func::BodyAs(raw_arg(0), name == "mandatoryBodyAs", navs)
            }
            "headerAs" | "exchangePropertyAs" | "variableAs" => {
                if nargs != 2 {
                    return Err(self.err(format!("Valid syntax: ${{{}(key, type)}} was: {}", name, s(&c[start..end])), start));
                }
                let scope = match name.as_str() {
                    "headerAs" => Scope::Header,
                    "exchangePropertyAs" => Scope::Property,
                    _ => Scope::Variable,
                };
                Func::ScopedAs(scope, arg_expr(0)?, raw_arg(1))
            }
            "iif" => {
                if nargs != 3 {
                    return Err(self.err("Valid syntax: ${iif(predicate,trueExpression,falseExpression)}", start));
                }
                let (a, b) = args[0];
                Func::Iif(self.predicate(a, b, true)?, arg_expr(1)?, arg_expr(2)?)
            }
            "not" => {
                if nargs != 1 {
                    return Err(self.err("Valid syntax: ${not(predicate)}", start));
                }
                let (a, b) = args[0];
                Func::Not(self.predicate(a, b, true)?)
            }
            _ => {
                let (_, min, max) = CALLS.iter().find(|(n, _, _)| *n == name).unwrap();
                if nargs < *min || nargs > *max {
                    return Err(self.err(
                        format!("Function {} expects between {} and {} arguments but got {}", name, min, max, nargs),
                        start,
                    ));
                }
                let mut ev = Vec::new();
                for k in 0..nargs {
                    ev.push(arg_expr(k)?);
                }
                Func::Call(name, ev, navs)
            }
        };
        Ok(Some(f))
    }

    /// Split `(a, b, c)` content into argument ranges. Empty content gives no args.
    fn split_args(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let c = &self.chars;
        let (ts, te) = self.trim(start, end);
        if ts >= te {
            return vec![];
        }
        let mut out = Vec::new();
        let mut depth = 0;
        let mut i = start;
        let mut seg = start;
        while i < end {
            match c[i] {
                '\'' | '"' => {
                    let q = c[i];
                    let mut k = i + 1;
                    while k < end && c[k] != q {
                        k += 1;
                    }
                    i = k;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    out.push((seg, i));
                    seg = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        out.push((seg, end));
        out
    }

    /// A function argument: quoted literal (template) or template text.
    fn arg(&self, start: usize, end: usize) -> Result<Expr> {
        let (a, b) = self.trim(start, end);
        let c = &self.chars;
        if b > a + 1 && (c[a] == '\'' || c[a] == '"') && c[b - 1] == c[a] {
            return self.template(a + 1, b - 1);
        }
        self.template(a, b)
    }

    /// Parse OGNL-like navigation in `[start, end)`.
    fn navs(&self, start: usize, end: usize) -> Result<Vec<Nav>> {
        let c = &self.chars;
        let mut out = Vec::new();
        let mut i = start;
        while i < end {
            let mut safe = false;
            if c[i] == '?' && i + 1 < end && (c[i + 1] == '.' || c[i + 1] == '[') {
                safe = true;
                i += 1;
            }
            match c[i] {
                '.' => {
                    i += 1;
                    let ns = i;
                    while i < end && !matches!(c[i], '.' | '[' | '(' | '?') {
                        i += 1;
                    }
                    let name = s(&c[ns..i]);
                    if name.is_empty() {
                        return Err(self.err("Invalid OGNL expression: empty property name", ns));
                    }
                    if i < end && c[i] == '(' {
                        let mut depth = 0;
                        let mut j = i;
                        let mut close = None;
                        while j < end {
                            match c[j] {
                                '\'' | '"' => {
                                    let q = c[j];
                                    let mut k = j + 1;
                                    while k < end && c[k] != q {
                                        k += 1;
                                    }
                                    j = k;
                                }
                                '(' => depth += 1,
                                ')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        close = Some(j);
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            j += 1;
                        }
                        let close = close.ok_or_else(|| self.err("Invalid OGNL expression: missing ')'", i))?;
                        let mut args = Vec::new();
                        for (a, b) in self.split_args(i + 1, close) {
                            args.push(self.arg(a, b)?);
                        }
                        out.push(Nav::Method(name, args, safe));
                        i = close + 1;
                    } else {
                        out.push(Nav::Key(name, safe));
                    }
                }
                '[' => {
                    let close = (i + 1..end).find(|&k| c[k] == ']').ok_or_else(|| self.err("Invalid OGNL expression: missing ']'", i))?;
                    let key = s(&c[i + 1..close]);
                    out.push(Nav::Index(unquote(key.trim()).to_string(), safe));
                    i = close + 1;
                }
                _ => return Err(self.err(format!("Invalid OGNL expression: unexpected '{}'", c[i]), i)),
            }
        }
        Ok(out)
    }

    fn navs_str(&self, text: &str, pos: usize) -> Result<Vec<Nav>> {
        let sub = P::new(text);
        sub.navs(0, sub.chars.len()).map_err(|e| self.err(e.message.lines().next().unwrap_or("Invalid OGNL").to_string(), pos))
    }

    /// Scoped (header/property/variable) name with optional navigation.
    fn scoped(&self, scope: Scope, rest: &str, pos: usize) -> Result<Func> {
        if rest.is_empty() {
            return Err(self.err("Missing name", pos));
        }
        if let Some(r) = rest.strip_prefix('[') {
            let close = r.find(']').ok_or_else(|| self.err("Missing ']'", pos))?;
            let name = unquote(r[..close].trim()).to_string();
            let navs = self.navs_str(&r[close + 1..], pos)?;
            return Ok(Func::Scoped(scope, name.clone(), Some((name, navs))));
        }
        let split = rest.find(['.', '[', '?']);
        let alt = match split {
            Some(k) if k > 0 => {
                let navs = self.navs_str(&rest[k..], pos).ok();
                navs.map(|n| (rest[..k].to_string(), n))
            }
            _ => None,
        };
        Ok(Func::Scoped(scope, rest.to_string(), alt))
    }

    fn function(&self, f: &str, pos: usize) -> Result<Func> {
        let f = f.trim();
        let simple = match f {
            "body" | "in.body" | "message.body" => Some(Func::Body(vec![])),
            "bodyOneLine" => Some(Func::BodyOneLine),
            "bodyType" => Some(Func::BodyType),
            "headers" | "in.headers" => Some(Func::Headers),
            "variables" => Some(Func::Variables),
            "exchangeId" => Some(Func::ExchangeId),
            "id" | "messageId" | "in.id" | "in.messageId" => Some(Func::MessageId),
            "messageTimestamp" => Some(Func::MessageTimestamp),
            "routeId" => Some(Func::RouteId),
            "fromRouteId" => Some(Func::FromRouteId),
            "camelId" | "camelContext.name" => Some(Func::CamelId),
            "exception" => Some(Func::Exception(vec![])),
            "null" => Some(Func::Null),
            "uuid" => Some(Func::Call("uuid".into(), vec![], vec![])),
            "prettyBody" => Some(Func::Call("pretty".into(), vec![], vec![])),
            "messageHistory" => Some(Func::Null),
            _ => None,
        };
        if let Some(func) = simple {
            return Ok(func);
        }
        for prefix in ["body", "in.body", "message.body"] {
            if let Some(rest) = f.strip_prefix(prefix) {
                if rest.starts_with('.') || rest.starts_with('[') || rest.starts_with("?.") {
                    return Ok(Func::Body(self.navs_str(rest, pos)?));
                }
            }
        }
        for (prefix, scope) in [
            ("in.headers.", Scope::Header),
            ("in.header.", Scope::Header),
            ("in.headers:", Scope::Header),
            ("in.header:", Scope::Header),
            ("headers.", Scope::Header),
            ("header.", Scope::Header),
            ("headers:", Scope::Header),
            ("header:", Scope::Header),
            ("exchangeProperty.", Scope::Property),
            ("exchangeProperty:", Scope::Property),
            ("exchangeProperties.", Scope::Property),
            ("property.", Scope::Property),
            ("property:", Scope::Property),
            ("variables.", Scope::Variable),
            ("variable.", Scope::Variable),
            ("variable:", Scope::Variable),
        ] {
            if let Some(rest) = f.strip_prefix(prefix) {
                return self.scoped(scope, rest, pos);
            }
        }
        for (prefix, scope) in [
            ("in.headers", Scope::Header),
            ("in.header", Scope::Header),
            ("headers", Scope::Header),
            ("header", Scope::Header),
            ("exchangeProperty", Scope::Property),
            ("variables", Scope::Variable),
            ("variable", Scope::Variable),
        ] {
            if let Some(rest) = f.strip_prefix(prefix) {
                if rest.starts_with('[') {
                    return self.scoped(scope, rest, pos);
                }
            }
        }
        if let Some(rest) = f.strip_prefix("exception") {
            if rest.starts_with('.') || rest.starts_with('[') || rest.starts_with("?.") {
                return Ok(Func::Exception(self.navs_str(rest, pos)?));
            }
        }
        if let Some(rest) = f.strip_prefix("date-with-timezone:") {
            let (cmd, rest) = rest.split_once(':').ok_or_else(|| self.err("Valid syntax: ${date-with-timezone:command:timezone:pattern}", pos))?;
            // timezone may contain ':' as in GMT+02:00
            let (tz, pattern) = split_tz(rest);
            if pattern.is_empty() {
                return Err(self.err("Valid syntax: ${date-with-timezone:command:timezone:pattern}", pos));
            }
            return self.date(cmd, Some(pattern.to_string()), support::parse_timezone(tz), pos);
        }
        if let Some(rest) = f.strip_prefix("date:") {
            let (cmd, pattern) = match rest.split_once(':') {
                Some((c, p)) => (c, Some(p.to_string())),
                None => (rest, None),
            };
            return self.date(cmd, pattern, 0, pos);
        }
        if let Some(rest) = f.strip_prefix("properties-exist:") {
            return Ok(Func::PropertiesExist(rest.trim().to_string()));
        }
        if let Some(rest) = f.strip_prefix("properties:") {
            let rest = rest.trim();
            if rest.is_empty() {
                return Err(self.err("Valid syntax: ${properties:key[:default]}", pos));
            }
            return Ok(match rest.split_once(':') {
                Some((k, d)) => Func::Properties(k.to_string(), Some(d.to_string())),
                None => Func::Properties(rest.to_string(), None),
            });
        }
        for prefix in ["env.", "env:", "sysenv.", "sysenv:"] {
            if let Some(rest) = f.strip_prefix(prefix) {
                return Ok(Func::Env(rest.to_string()));
            }
        }
        for prefix in ["sys.", "sys:"] {
            if let Some(rest) = f.strip_prefix(prefix) {
                return Ok(Func::Sys(rest.to_string()));
            }
        }
        if let Some(rest) = f.strip_prefix("ref:") {
            return Ok(Func::Ref(rest.trim().to_string()));
        }
        if let Some(rest) = f.strip_prefix("bean:") {
            let rest = rest.trim();
            if let Some((n, m)) = rest.split_once("?method=") {
                return Ok(Func::Bean(n.to_string(), Some(m.to_string())));
            }
            if let Some((n, m)) = rest.split_once("::") {
                return Ok(Func::Bean(n.to_string(), Some(m.to_string())));
            }
            if let Some((n, m)) = rest.split_once('.') {
                return Ok(Func::Bean(n.to_string(), Some(m.to_string())));
            }
            return Ok(Func::Bean(rest.to_string(), None));
        }
        if f.starts_with("type:") {
            return Err(self.err(format!("The type: function is not supported in rust-camel: {}", f), pos));
        }
        Err(self.err(format!("Unknown function: {}", f), pos))
    }

    fn date(&self, cmd: &str, pattern: Option<String>, tz: i64, pos: usize) -> Result<Func> {
        // split off offsets: now-1h, header.x+2d
        let cmd = cmd.trim();
        let mut base = cmd;
        let mut offset = 0;
        for (k, ch) in cmd.char_indices() {
            if (ch == '+' || ch == '-') && cmd[k + 1..].starts_with(|c: char| c.is_ascii_digit()) {
                if let Some(o) = support::parse_offset(&cmd[k..]) {
                    base = &cmd[..k];
                    offset = o;
                    break;
                }
            }
        }
        let source = match base {
            "now" => DateSource::Now,
            "millis" => DateSource::Millis,
            "exchangeCreated" => DateSource::Created,
            "file" => DateSource::Header(crate::message::props::FILE_LAST_MODIFIED.to_string()),
            b => {
                if let Some(h) = b.strip_prefix("header.").or_else(|| b.strip_prefix("in.header.")).or_else(|| b.strip_prefix("headers.")) {
                    DateSource::Header(h.to_string())
                } else if let Some(p) = b.strip_prefix("exchangeProperty.") {
                    DateSource::Property(p.to_string())
                } else if let Some(v) = b.strip_prefix("variable.") {
                    DateSource::Variable(v.to_string())
                } else {
                    return Err(self.err(format!("Command not supported for dateExpression: {}", b), pos));
                }
            }
        };
        Ok(Func::Date { source, offset, pattern: pattern.filter(|p| !p.is_empty()), tz })
    }
}

/// Call-form functions with (name, min args, max args).
const CALLS: &[(&str, usize, usize)] = &[
    ("random", 1, 2),
    ("uuid", 0, 1),
    ("collate", 1, 1),
    ("skip", 1, 1),
    ("join", 0, 3),
    ("replace", 2, 3),
    ("substring", 1, 3),
    ("substringBefore", 1, 2),
    ("substringAfter", 1, 2),
    ("substringBetween", 2, 3),
    ("trim", 0, 1),
    ("uppercase", 0, 1),
    ("lowercase", 0, 1),
    ("length", 0, 1),
    ("size", 0, 1),
    ("sum", 0, 64),
    ("max", 0, 64),
    ("min", 0, 64),
    ("average", 0, 64),
    ("abs", 0, 1),
    ("floor", 0, 1),
    ("ceil", 0, 1),
    ("round", 0, 1),
    ("split", 0, 2),
    ("hash", 0, 2),
    ("base64Encode", 0, 1),
    ("base64Decode", 0, 1),
    ("concat", 2, 3),
    ("isEmpty", 0, 1),
    ("isAlpha", 0, 1),
    ("isNumeric", 0, 1),
    ("isAlphaNumeric", 0, 1),
    ("normalizeWhitespace", 0, 1),
    ("pretty", 0, 1),
    ("empty", 1, 1),
    ("list", 0, 64),
    ("map", 0, 64),
    ("jsonpath", 1, 2),
    ("jq", 1, 2),
    ("xpath", 1, 2),
    ("messageHistory", 0, 1),
    ("increment", 0, 1),
    ("decrement", 0, 1),
];

fn unquote(t: &str) -> &str {
    let b = t.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

fn split_tz(rest: &str) -> (&str, &str) {
    // GMT+02:00:pattern / +02:00:pattern
    let bytes = rest.as_bytes();
    if let Some(k) = rest.find(':') {
        let tz = &rest[..k];
        let after = &rest[k + 1..];
        let has_sign_hours = tz.ends_with(|c: char| c.is_ascii_digit()) && (tz.contains('+') || tz.contains('-'));
        if has_sign_hours && after.len() >= 3 && after.as_bytes()[0].is_ascii_digit() && after.as_bytes()[1].is_ascii_digit() && after.as_bytes()[2] == b':' {
            let _ = bytes;
            return (&rest[..k + 3], &rest[k + 4..]);
        }
        return (tz, after);
    }
    (rest, "")
}

fn literal_keyword(t: &str) -> Option<Value> {
    match t {
        "null" => Some(Value::Null),
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => {
            if t.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+') {
                let n = t.strip_suffix(['L', 'l']).unwrap_or(t);
                if let Ok(i) = n.parse::<i64>() {
                    return Some(Value::Int(i));
                }
                let f = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t);
                if f.contains(|c: char| c.is_ascii_digit()) && !f.contains("..") {
                    if let Ok(x) = f.parse::<f64>() {
                        return Some(Value::Float(x));
                    }
                }
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

impl Expr {
    fn eval(&self, ex: &Exchange) -> Result<Value> {
        match self {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        Part::Text(t) => out.push_str(t),
                        Part::Expr(e) => out.push_str(&e.eval(ex)?.to_string()),
                    }
                }
                Ok(Value::String(out))
            }
            Expr::Func(f) => f.eval(ex),
            Expr::Dynamic(inner) => {
                let text = inner.eval(ex)?.to_string();
                let p = P::new(&text);
                let e = p.block(0, p.chars.len())?;
                e.eval(ex)
            }
            Expr::Ternary(c, a, b) => {
                if c.eval(ex)? {
                    a.eval(ex)
                } else {
                    b.eval(ex)
                }
            }
            Expr::Elvis(a, b) => {
                let v = a.eval(ex)?;
                if value_predicate(&v) && !matches!(&v, Value::String(s) if s.is_empty()) {
                    Ok(v)
                } else {
                    b.eval(ex)
                }
            }
            Expr::Unary(e, d) => {
                let v = e.eval(ex)?;
                match v {
                    Value::Int(i) => Ok(Value::Int(i + d)),
                    Value::Float(f) => Ok(Value::Float(f + *d as f64)),
                    other => match other.as_i64() {
                        Some(i) => Ok(Value::Int(i + d)),
                        None => match other.as_f64() {
                            Some(f) => Ok(Value::Float(f + *d as f64)),
                            None => Err(CamelError::expression(format!("Cannot apply unary operator to non-numeric value: {}", other))),
                        },
                    },
                }
            }
        }
    }

    fn eval_string(&self, ex: &Exchange) -> Result<String> {
        Ok(self.eval(ex)?.to_string())
    }
}

fn op_matches(op: Op, l: &Value, r: &Value, re: Option<&Regex>) -> Result<bool> {
    use std::cmp::Ordering::*;
    let lstr = || l.to_string();
    Ok(match op {
        Op::Eq => values_equal(l, r),
        Op::Ne => !values_equal(l, r),
        Op::EqIc => {
            if l.is_null() || r.is_null() {
                l.is_null() && r.is_null()
            } else {
                lstr().eq_ignore_ascii_case(&r.to_string()) || values_equal(l, r)
            }
        }
        Op::NeIc => {
            !(if l.is_null() || r.is_null() { l.is_null() && r.is_null() } else { lstr().eq_ignore_ascii_case(&r.to_string()) || values_equal(l, r) })
        }
        Op::Gt => compare_values(l, r) == Some(Greater),
        Op::Ge => matches!(compare_values(l, r), Some(Greater | Equal)),
        Op::Lt => compare_values(l, r) == Some(Less),
        Op::Le => matches!(compare_values(l, r), Some(Less | Equal)),
        Op::Contains(neg) => {
            let m = match l {
                Value::Null => false,
                Value::Array(a) => a.iter().any(|i| values_equal(i, r)),
                Value::Object(o) => o.contains_key(&r.to_string()),
                other => other.to_string().contains(&r.to_string()),
            };
            m != neg
        }
        Op::ContainsIc(neg) => {
            let m = match l {
                Value::Null => false,
                Value::Array(a) => a.iter().any(|i| i.to_string().eq_ignore_ascii_case(&r.to_string())),
                other => other.to_string().to_lowercase().contains(&r.to_string().to_lowercase()),
            };
            m != neg
        }
        Op::Regex(neg) => {
            let m = if l.is_null() {
                false
            } else {
                match re {
                    Some(re) => re.is_full_match(&lstr()),
                    None => Regex::new(&r.to_string())?.is_full_match(&lstr()),
                }
            };
            m != neg
        }
        Op::In(neg) => {
            let items: Vec<Value> = match r {
                Value::Array(a) => a.clone(),
                other => other.to_string().split(',').map(|p| Value::String(p.trim().to_string())).collect(),
            };
            let m = items.iter().any(|i| values_equal(l, i));
            m != neg
        }
        Op::Is(neg) => support::is_instance_of(l, &r.to_string()) != neg,
        Op::Range(neg) => {
            let rs = r.to_string();
            let (a, b) = rs.split_once("..").ok_or_else(|| CamelError::syntax(format!("Range must be in the format 'from..to': {}", rs)))?;
            let lo = Value::String(a.trim().to_string());
            let hi = Value::String(b.trim().to_string());
            let m = !l.is_null()
                && matches!(compare_values(l, &lo), Some(Greater | Equal))
                && matches!(compare_values(l, &hi), Some(Less | Equal));
            m != neg
        }
        Op::StartsWith(neg) => (!l.is_null() && lstr().starts_with(&r.to_string())) != neg,
        Op::EndsWith(neg) => (!l.is_null() && lstr().ends_with(&r.to_string())) != neg,
    })
}

impl Pred {
    fn eval(&self, ex: &Exchange) -> Result<bool> {
        match self {
            Pred::Or(items) => {
                for p in items {
                    if p.eval(ex)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Pred::And(items) => {
                for p in items {
                    if !p.eval(ex)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pred::Truthy(e) => Ok(value_predicate(&e.eval(ex)?)),
            Pred::Cmp(l, op, r, re) => {
                let lv = l.eval(ex)?;
                let rv = r.eval(ex)?;
                // numeric literal vs numeric-looking string compare numerically
                op_matches(*op, &lv, &rv, re.as_ref())
            }
        }
    }
}

fn exception_of(ex: &Exchange) -> Option<CamelError> {
    if let Some(e) = &ex.exception {
        return Some(e.clone());
    }
    match ex.properties.get(crate::message::props::EXCEPTION_CAUGHT)? {
        Value::Null => None,
        Value::Object(o) => {
            let kind = o.get("kind").or_else(|| o.get("class")).map(|v| v.to_string()).unwrap_or_else(|| kinds::EXCEPTION.to_string());
            let msg = o.get("message").map(|v| v.to_string()).unwrap_or_default();
            Some(CamelError::new(kind, msg))
        }
        other => Some(CamelError::new(kinds::EXCEPTION, other.to_string())),
    }
}

fn error_value(e: &CamelError) -> Value {
    let mut m = BTreeMap::new();
    m.insert("kind".to_string(), Value::String(e.kind.clone()));
    m.insert("class".to_string(), Value::String(e.kind.clone()));
    m.insert("message".to_string(), Value::String(e.message.clone()));
    m.insert("stacktrace".to_string(), Value::String(stacktrace(e)));
    m.insert("cause".to_string(), e.cause.as_deref().map(error_value).unwrap_or(Value::Null));
    Value::Object(m)
}

fn stacktrace(e: &CamelError) -> String {
    let mut out = String::new();
    for (i, c) in e.chain().enumerate() {
        if i > 0 {
            out.push_str("\nCaused by: ");
        }
        out.push_str(&format!("{}: {}", c.kind, c.message));
    }
    out
}

fn scoped_get(ex: &Exchange, scope: &Scope, name: &str) -> Option<Value> {
    match scope {
        Scope::Header => ex.message.headers.get(name).cloned(),
        Scope::Property => ex.properties.get(name).cloned(),
        Scope::Variable => ex.properties.get(&format!("CamelVariable.{}", name)).cloned(),
    }
}

/// Parse a string body as JSON when navigating into it.
fn as_structured(v: Value) -> Value {
    if let Some(s) = v.as_str() {
        let t = s.trim_start();
        if t.starts_with('{') || t.starts_with('[') {
            if let Ok(j) = crate::json::parse(s) {
                return j;
            }
        }
    }
    v
}

fn navigate(mut v: Value, navs: &[Nav], ex: &Exchange) -> Result<Value> {
    for (i, n) in navs.iter().enumerate() {
        let safe = match n {
            Nav::Key(_, s) | Nav::Index(_, s) | Nav::Method(_, _, s) => *s,
        };
        if v.is_null() {
            if safe || i == 0 {
                return Ok(Value::Null);
            }
            let what = match n {
                Nav::Key(k, _) | Nav::Index(k, _) | Nav::Method(k, _, _) => k.clone(),
            };
            return Err(CamelError::new(
                kinds::NULL_POINTER,
                format!("Cannot invoke '{}' on null object in OGNL expression (use ?. for null safe navigation)", what),
            ));
        }
        v = match n {
            Nav::Key(k, _) => {
                let sv = if matches!(v, Value::String(_) | Value::Bytes(_)) { as_structured(v) } else { v };
                match &sv {
                    Value::Object(o) => o.get(k).cloned().unwrap_or(Value::Null),
                    Value::Array(a) => match k.as_str() {
                        "length" | "size" => Value::Int(a.len() as i64),
                        _ => k.parse::<usize>().ok().and_then(|i| a.get(i).cloned()).unwrap_or(Value::Null),
                    },
                    Value::String(st) if k == "length" => Value::Int(st.chars().count() as i64),
                    other => {
                        return Err(CamelError::new(
                            "MethodNotFoundException",
                            format!("Method with name: {} not found on bean of type: {}", k, other.type_name()),
                        ))
                    }
                }
            }
            Nav::Index(k, _) => {
                let sv = if matches!(v, Value::String(_) | Value::Bytes(_)) { as_structured(v) } else { v };
                match &sv {
                    Value::Array(a) => {
                        let idx = if k == "last" {
                            a.len() as i64 - 1
                        } else if let Some(off) = k.strip_prefix("last-") {
                            a.len() as i64 - 1 - off.trim().parse::<i64>().unwrap_or(0)
                        } else {
                            match k.parse::<i64>() {
                                Ok(i) if i < 0 => a.len() as i64 + i,
                                Ok(i) => i,
                                Err(_) => -1,
                            }
                        };
                        if idx >= 0 { a.get(idx as usize).cloned().unwrap_or(Value::Null) } else { Value::Null }
                    }
                    Value::Object(o) => o.get(k).cloned().unwrap_or(Value::Null),
                    Value::String(st) => k.parse::<usize>().ok().and_then(|i| st.chars().nth(i)).map(|c| Value::String(c.to_string())).unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }
            Nav::Method(m, args, _) => {
                let mut a = Vec::new();
                for e in args {
                    a.push(e.eval(ex)?);
                }
                call_method(&v, m, &a)?
            }
        };
    }
    Ok(v)
}

fn call_method(v: &Value, m: &str, a: &[Value]) -> Result<Value> {
    let st = || v.to_string();
    let arg = |i: usize| a.get(i).cloned().unwrap_or(Value::Null);
    let argi = |i: usize| a.get(i).and_then(|x| x.as_i64()).unwrap_or(0);
    Ok(match m {
        "length" | "size" | "getLength" | "getSize" => Value::Int(v.len().unwrap_or(1) as i64),
        "toUpperCase" => Value::String(st().to_uppercase()),
        "toLowerCase" => Value::String(st().to_lowercase()),
        "trim" | "strip" => Value::String(st().trim().to_string()),
        "isEmpty" => Value::Bool(v.is_empty()),
        "toString" => Value::String(st()),
        "substring" => {
            let chars: Vec<char> = st().chars().collect();
            let b = argi(0).clamp(0, chars.len() as i64) as usize;
            let e = if a.len() > 1 { argi(1).clamp(b as i64, chars.len() as i64) as usize } else { chars.len() };
            Value::String(chars[b..e].iter().collect())
        }
        "startsWith" => Value::Bool(st().starts_with(&arg(0).to_string())),
        "endsWith" => Value::Bool(st().ends_with(&arg(0).to_string())),
        "contains" => Value::Bool(match v {
            Value::Array(items) => items.iter().any(|i| values_equal(i, &arg(0))),
            Value::Object(o) => o.contains_key(&arg(0).to_string()),
            _ => st().contains(&arg(0).to_string()),
        }),
        "containsKey" => Value::Bool(v.as_object().map(|o| o.contains_key(&arg(0).to_string())).unwrap_or(false)),
        "get" => match v {
            Value::Array(items) => items.get(argi(0) as usize).cloned().unwrap_or(Value::Null),
            _ => v.get(&arg(0).to_string()).cloned().unwrap_or(Value::Null),
        },
        "indexOf" => Value::Int(st().find(&arg(0).to_string()).map(|b| st()[..b].chars().count() as i64).unwrap_or(-1)),
        "charAt" => st().chars().nth(argi(0) as usize).map(|c| Value::String(c.to_string())).unwrap_or(Value::Null),
        "replace" => Value::String(st().replace(&arg(0).to_string(), &arg(1).to_string())),
        "replaceAll" => Value::String(Regex::new(&arg(0).to_string())?.replace_all(&st(), &arg(1).to_string())),
        "replaceFirst" => Value::String(Regex::new(&arg(0).to_string())?.replace_first(&st(), &arg(1).to_string())),
        "split" => Value::from(Regex::new(&arg(0).to_string())?.split(&st())),
        "matches" => Value::Bool(Regex::new(&arg(0).to_string())?.is_full_match(&st())),
        "equals" => Value::Bool(values_equal(v, &arg(0))),
        "equalsIgnoreCase" => Value::Bool(st().eq_ignore_ascii_case(&arg(0).to_string())),
        "keySet" | "keys" => Value::Array(v.as_object().map(|o| o.keys().map(|k| Value::String(k.clone())).collect()).unwrap_or_default()),
        "values" => Value::Array(v.as_object().map(|o| o.values().cloned().collect()).unwrap_or_default()),
        "intValue" | "longValue" => Value::Int(v.as_i64().unwrap_or(0)),
        "doubleValue" | "floatValue" => Value::Float(v.as_f64().unwrap_or(0.0)),
        "getClass" => Value::String(v.type_name().to_string()),
        _ => {
            return Err(CamelError::new(
                "MethodNotFoundException",
                format!("Method with name: {} not found on bean: {} of type: {}", m, v, v.type_name()),
            ))
        }
    })
}

fn body_one_line(v: &Value) -> String {
    v.to_string().replace(['\n', '\r'], "")
}

fn num_value(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

fn numbers(vals: &[Value]) -> Result<Vec<f64>> {
    let mut out = Vec::new();
    for v in vals {
        match v {
            Value::Array(a) => out.extend(numbers(a)?),
            Value::Null => {}
            other => out.push(other.as_f64().ok_or_else(|| CamelError::new(kinds::NUMBER_FORMAT, format!("Not a number: {}", other)))?),
        }
    }
    Ok(out)
}

impl Func {
    fn eval(&self, ex: &Exchange) -> Result<Value> {
        match self {
            Func::Body(navs) => navigate(ex.message.body.clone(), navs, ex),
            Func::BodyAs(t, mandatory, navs) => {
                let v = support::convert_to_type(&ex.message.body, t)?;
                if *mandatory && v.is_null() {
                    return Err(CamelError::new("InvalidPayloadException", format!("No body available of type: {} on: Message", t)));
                }
                navigate(v, navs, ex)
            }
            Func::BodyOneLine => Ok(Value::String(body_one_line(&ex.message.body))),
            Func::BodyType => Ok(Value::String(ex.message.body.type_name().to_string())),
            Func::Scoped(scope, name, alt) => {
                if let Some(v) = scoped_get(ex, scope, name) {
                    return Ok(v);
                }
                if let Some((base, navs)) = alt {
                    if let Some(v) = scoped_get(ex, scope, base) {
                        return navigate(v, navs, ex);
                    }
                }
                Ok(Value::Null)
            }
            Func::ScopedAs(scope, name, t) => {
                let n = name.eval_string(ex)?;
                support::convert_to_type(&scoped_get(ex, scope, &n).unwrap_or(Value::Null), t)
            }
            Func::Headers => Ok(ex.message.headers.to_value()),
            Func::Variables => Ok(Value::Object(
                ex.properties.iter().filter_map(|(k, v)| k.strip_prefix("CamelVariable.").map(|n| (n.to_string(), v.clone()))).collect(),
            )),
            Func::ExchangeId => Ok(Value::String(ex.exchange_id.clone())),
            Func::MessageId => Ok(Value::String(ex.message.message_id.clone())),
            Func::MessageTimestamp => Ok(Value::Int(ex.created_millis())),
            Func::RouteId | Func::FromRouteId => Ok(ex.from_route_id.clone().map(Value::String).unwrap_or(Value::Null)),
            Func::CamelId => Ok(ex.context.as_ref().map(|c| Value::String(c.name().to_string())).unwrap_or(Value::Null)),
            Func::Exception(navs) => match exception_of(ex) {
                None => Ok(Value::Null),
                Some(e) => {
                    if navs.is_empty() {
                        return Ok(Value::String(e.to_string()));
                    }
                    // `exception.class.name` / `exception.class.simpleName` -> kind
                    if let Some(Nav::Key(k, _)) = navs.first() {
                        if k == "class" {
                            return Ok(Value::String(e.kind.clone()));
                        }
                        if k == "stackTrace" {
                            return Ok(Value::String(stacktrace(&e)));
                        }
                        if k == "localizedMessage" {
                            return Ok(Value::String(e.message.clone()));
                        }
                    }
                    if let Some(Nav::Method(m, _, _)) = navs.first() {
                        match m.as_str() {
                            "getMessage" | "getLocalizedMessage" => return Ok(Value::String(e.message.clone())),
                            "getClass" => return Ok(Value::String(e.kind.clone())),
                            _ => {}
                        }
                    }
                    navigate(error_value(&e), navs, ex)
                }
            },
            Func::Date { source, offset, pattern, tz } => {
                let base = match source {
                    DateSource::Now => Some(support_now()),
                    DateSource::Millis => {
                        if pattern.is_none() {
                            return Ok(Value::Int(support_now() + offset));
                        }
                        Some(support_now())
                    }
                    DateSource::Created => Some(ex.created_millis()),
                    DateSource::Header(h) => match ex.message.headers.get(h) {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(
                            support::value_to_millis(v)
                                .ok_or_else(|| CamelError::expression(format!("Cannot find Date/long object at command: header.{}", h)))?,
                        ),
                    },
                    DateSource::Property(p) => match ex.properties.get(p) {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(
                            support::value_to_millis(v)
                                .ok_or_else(|| CamelError::expression(format!("Cannot find Date/long object at command: exchangeProperty.{}", p)))?,
                        ),
                    },
                    DateSource::Variable(p) => match ex.properties.get(&format!("CamelVariable.{}", p)) {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(
                            support::value_to_millis(v)
                                .ok_or_else(|| CamelError::expression(format!("Cannot find Date/long object at command: variable.{}", p)))?,
                        ),
                    },
                };
                let millis = match base {
                    None => return Ok(Value::Null),
                    Some(m) => m + offset,
                };
                Ok(match pattern {
                    Some(p) => Value::String(support::format_date(millis, p, *tz)),
                    None => Value::Int(millis),
                })
            }
            Func::Properties(key, default) => {
                let ctx = ex.context()?;
                match ctx.properties().get(key) {
                    Some(v) => Ok(Value::String(ctx.resolve_placeholders(&v)?)),
                    None => match default {
                        Some(d) => Ok(Value::String(d.clone())),
                        None => Err(CamelError::illegal_argument(format!("Property with key [{}] not found in properties from text: {}", key, key))),
                    },
                }
            }
            Func::PropertiesExist(key) => {
                let (neg, k) = match key.strip_prefix('!') {
                    Some(k) => (true, k),
                    None => (false, key.as_str()),
                };
                Ok(Value::Bool(ex.context()?.properties().get(k).is_some() != neg))
            }
            Func::Env(n) => Ok(std::env::var(n)
                .ok()
                .or_else(|| std::env::var(n.to_ascii_uppercase().replace(['.', '-'], "_")).ok())
                .map(Value::String)
                .unwrap_or(Value::Null)),
            Func::Sys(n) => Ok(match n.as_str() {
                "os.name" => Value::from(std::env::consts::OS),
                "os.arch" => Value::from(std::env::consts::ARCH),
                "line.separator" => Value::from("\n"),
                "file.separator" => Value::from(std::path::MAIN_SEPARATOR.to_string()),
                "path.separator" => Value::from(if cfg!(windows) { ";" } else { ":" }),
                "user.dir" => std::env::current_dir().map(|p| Value::String(p.display().to_string())).unwrap_or(Value::Null),
                "user.home" => std::env::var("HOME").map(Value::String).unwrap_or(Value::Null),
                "user.name" => std::env::var("USER").map(Value::String).unwrap_or(Value::Null),
                "java.io.tmpdir" => Value::String(std::env::temp_dir().display().to_string()),
                other => std::env::var(other).map(Value::String).unwrap_or(Value::Null),
            }),
            Func::Null => Ok(Value::Null),
            Func::Ref(n) => support::lookup_ref_value(ex.context()?, n, ex),
            Func::Bean(n, m) => support::invoke_bean(ex.context()?, n, m.as_deref(), ex),
            Func::Iif(p, a, b) => {
                if p.eval(ex)? {
                    a.eval(ex)
                } else {
                    b.eval(ex)
                }
            }
            Func::Not(p) => Ok(Value::Bool(!p.eval(ex)?)),
            Func::Call(name, args, navs) => {
                let v = call(name, args, ex)?;
                navigate(v, navs, ex)
            }
        }
    }
}

fn support_now() -> i64 {
    crate::util_time::now_millis()
}

/// Evaluate a call-form function.
fn call(name: &str, args: &[Expr], ex: &Exchange) -> Result<Value> {
    let body = || ex.message.body.clone();
    let ev = |i: usize| -> Result<Value> { args[i].eval(ex) };
    let evs = |i: usize| -> Result<String> { args[i].eval_string(ex) };
    // value of the optional trailing "exp" argument (defaults to the body)
    let exp_or_body = |i: usize| -> Result<Value> {
        if args.len() > i {
            ev(i)
        } else {
            Ok(body())
        }
    };
    let int_arg = |i: usize| -> Result<i64> {
        let v = ev(i)?;
        v.as_i64().ok_or_else(|| CamelError::new(kinds::NUMBER_FORMAT, format!("Function {} expects a number but got: {}", name, v)))
    };
    Ok(match name {
        "random" => {
            let (min, max) = if args.len() == 2 { (int_arg(0)?, int_arg(1)?) } else { (0, int_arg(0)?) };
            Value::Int(support::random_range(min, max))
        }
        "uuid" => {
            let kind = if args.is_empty() { "default".to_string() } else { evs(0)? };
            match kind.as_str() {
                "classic" => Value::String(crate::message::generate_id()),
                "short" | "simple" => Value::String(support::to_hex(&support::random_u64().to_be_bytes())),
                _ => Value::String(support::uuid_v4()),
            }
        }
        "collate" => {
            let n = int_arg(0)?.max(1) as usize;
            let items = support::iterate(&body());
            Value::Array(items.chunks(n).map(|c| Value::Array(c.to_vec())).collect())
        }
        "skip" => {
            let n = int_arg(0)?.max(0) as usize;
            Value::Array(support::iterate(&body()).into_iter().skip(n).collect())
        }
        "join" => {
            let sep = if !args.is_empty() { evs(0)? } else { ",".to_string() };
            let prefix = if args.len() > 1 { evs(1)? } else { String::new() };
            let v = exp_or_body(2)?;
            let items = support::iterate(&v);
            Value::String(items.iter().map(|i| format!("{}{}", prefix, i)).collect::<Vec<_>>().join(&sep))
        }
        "replace" => {
            let from = decode_entities(&evs(0)?);
            let to = decode_entities(&evs(1)?);
            let v = exp_or_body(2)?;
            if v.is_null() {
                Value::Null
            } else {
                Value::String(v.to_string().replace(&from, &to))
            }
        }
        "substring" => {
            // substring(head) / substring(head, tail) / substring(head, tail, exp)
            let head = int_arg(0)?;
            let tail = if args.len() > 1 { int_arg(1)? } else { 0 };
            let v = exp_or_body(2)?;
            if v.is_null() {
                return Ok(Value::Null);
            }
            let chars: Vec<char> = v.to_string().chars().collect();
            let (mut b, mut e) = (0usize, chars.len());
            if args.len() == 1 {
                if head >= 0 {
                    b = (head as usize).min(chars.len());
                } else {
                    e = chars.len().saturating_sub(head.unsigned_abs() as usize);
                }
            } else {
                b = (head.unsigned_abs() as usize).min(chars.len());
                e = chars.len().saturating_sub(tail.unsigned_abs() as usize);
            }
            if b >= e {
                Value::String(String::new())
            } else {
                Value::String(chars[b..e].iter().collect())
            }
        }
        "substringBefore" | "substringAfter" => {
            let (v, needle) = if args.len() == 2 { (ev(0)?, evs(1)?) } else { (body(), evs(0)?) };
            if v.is_null() {
                return Ok(Value::Null);
            }
            let st = v.to_string();
            match st.find(&needle) {
                None => Value::Null,
                Some(k) => {
                    if name == "substringBefore" {
                        Value::String(st[..k].to_string())
                    } else {
                        Value::String(st[k + needle.len()..].to_string())
                    }
                }
            }
        }
        "substringBetween" => {
            let (v, after, before) = if args.len() == 3 { (ev(0)?, evs(1)?, evs(2)?) } else { (body(), evs(0)?, evs(1)?) };
            let st = v.to_string();
            match st.find(&after) {
                None => Value::Null,
                Some(k) => {
                    let rest = &st[k + after.len()..];
                    match rest.find(&before) {
                        None => Value::Null,
                        Some(e) => Value::String(rest[..e].to_string()),
                    }
                }
            }
        }
        "trim" => match exp_or_body(0)? {
            Value::Null => Value::Null,
            v => Value::String(v.to_string().trim().to_string()),
        },
        "uppercase" => match exp_or_body(0)? {
            Value::Null => Value::Null,
            v => Value::String(v.to_string().to_uppercase()),
        },
        "lowercase" => match exp_or_body(0)? {
            Value::Null => Value::Null,
            v => Value::String(v.to_string().to_lowercase()),
        },
        "normalizeWhitespace" => match exp_or_body(0)? {
            Value::Null => Value::Null,
            v => Value::String(v.to_string().split_whitespace().collect::<Vec<_>>().join(" ")),
        },
        "length" => {
            let v = exp_or_body(0)?;
            Value::Int(match &v {
                Value::Null => 0,
                Value::Bytes(b) => b.len() as i64,
                Value::Array(_) | Value::Object(_) => v.to_json().chars().count() as i64,
                other => other.to_string().chars().count() as i64,
            })
        }
        "size" => {
            let v = exp_or_body(0)?;
            Value::Int(match &v {
                Value::Null => 0,
                Value::Array(a) => a.len() as i64,
                Value::Object(o) => o.len() as i64,
                _ => 1,
            })
        }
        "sum" | "max" | "min" | "average" => {
            let vals: Vec<Value> = if args.is_empty() {
                vec![body()]
            } else {
                args.iter().map(|a| a.eval(ex)).collect::<Result<Vec<_>>>()?
            };
            let vals: Vec<Value> = vals.into_iter().map(|v| if matches!(v, Value::String(_)) { as_structured(v) } else { v }).collect();
            let nums = numbers(&vals)?;
            if nums.is_empty() {
                return Ok(Value::Null);
            }
            match name {
                "sum" => num_value(nums.iter().sum()),
                "max" => num_value(nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                "min" => num_value(nums.iter().cloned().fold(f64::INFINITY, f64::min)),
                _ => num_value(nums.iter().sum::<f64>() / nums.len() as f64),
            }
        }
        "abs" | "floor" | "ceil" | "round" => {
            let v = exp_or_body(0)?;
            if v.is_null() {
                return Ok(Value::Null);
            }
            if let (Value::Int(i), "abs") = (&v, name) {
                return Ok(Value::Int(i.abs()));
            }
            let f = v.as_f64().ok_or_else(|| CamelError::new(kinds::NUMBER_FORMAT, format!("Not a number: {}", v)))?;
            match name {
                "abs" => {
                    if matches!(v, Value::Float(_)) {
                        Value::Float(f.abs())
                    } else {
                        num_value(f.abs())
                    }
                }
                "floor" => Value::Int(f.floor() as i64),
                "ceil" => Value::Int(f.ceil() as i64),
                _ => Value::Int(f.round() as i64),
            }
        }
        "split" => {
            let (v, sep) = match args.len() {
                0 => (body(), ",".to_string()),
                1 => (body(), evs(0)?),
                _ => (ev(0)?, evs(1)?),
            };
            if v.is_null() {
                return Ok(Value::Null);
            }
            Value::Array(v.to_string().split(sep.as_str()).map(|p| Value::String(p.to_string())).collect())
        }
        "hash" => {
            let v = exp_or_body(0)?;
            let algo = if args.len() > 1 { evs(1)? } else { "SHA-256".to_string() };
            if v.is_null() {
                return Ok(Value::Null);
            }
            Value::String(support::hash_hex(&v.to_bytes(), &algo)?)
        }
        "base64Encode" => {
            let v = exp_or_body(0)?;
            Value::String(crate::dataformat::base64::encode(&v.to_bytes(), false))
        }
        "base64Decode" => {
            let v = exp_or_body(0)?;
            let bytes = crate::dataformat::base64::decode(&v.to_string())?;
            match String::from_utf8(bytes) {
                Ok(s) => Value::String(s),
                Err(e) => Value::Bytes(e.into_bytes()),
            }
        }
        "concat" => {
            let a = evs(0)?;
            let b = evs(1)?;
            let sep = if args.len() > 2 { evs(2)? } else { String::new() };
            Value::String(format!("{}{}{}", a, sep, b))
        }
        "isEmpty" => {
            let v = exp_or_body(0)?;
            Value::Bool(match &v {
                Value::Null => true,
                Value::String(s) => s.trim().is_empty(),
                other => other.is_empty(),
            })
        }
        "isAlpha" => Value::Bool({
            let s = exp_or_body(0)?.to_string();
            !s.is_empty() && s.chars().all(|c| c.is_alphabetic())
        }),
        "isNumeric" => Value::Bool({
            let s = exp_or_body(0)?.to_string();
            !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
        }),
        "isAlphaNumeric" => Value::Bool({
            let s = exp_or_body(0)?.to_string();
            !s.is_empty() && s.chars().all(|c| c.is_alphanumeric())
        }),
        "pretty" => {
            let v = exp_or_body(0)?;
            match &v {
                Value::Object(_) | Value::Array(_) => Value::String(v.to_json_pretty()),
                Value::String(_) | Value::Bytes(_) => match crate::json::parse(v.as_str().unwrap_or("")) {
                    Ok(j @ (Value::Object(_) | Value::Array(_))) => Value::String(j.to_json_pretty()),
                    _ => v,
                },
                _ => v,
            }
        }
        "empty" => match evs(0)?.to_ascii_lowercase().as_str() {
            "string" => Value::String(String::new()),
            "list" | "set" | "array" => Value::Array(vec![]),
            "map" | "object" => Value::Object(BTreeMap::new()),
            other => return Err(CamelError::illegal_argument(format!("Function empty(type) supports string, list, map but got: {}", other))),
        },
        "list" => Value::Array(args.iter().map(|a| a.eval(ex)).collect::<Result<Vec<_>>>()?),
        "map" => {
            if args.len() % 2 != 0 {
                return Err(CamelError::illegal_argument("Function map(k1,v1,k2,v2...) requires an even number of arguments"));
            }
            let mut m = BTreeMap::new();
            for k in (0..args.len()).step_by(2) {
                m.insert(evs(k)?, ev(k + 1)?);
            }
            Value::Object(m)
        }
        "jsonpath" => {
            let (input, path) = if args.len() == 2 { (ev(0)?, evs(1)?) } else { (body(), evs(0)?) };
            super::jsonpath::query(&input, &path)?
        }
        "jq" => {
            let (input, q) = if args.len() == 2 { (ev(0)?, evs(1)?) } else { (body(), evs(0)?) };
            super::jq::query(&input, &q)?
        }
        "xpath" => {
            let (input, q) = if args.len() == 2 { (ev(0)?, evs(1)?) } else { (body(), evs(0)?) };
            super::xpath::query(&input.to_string(), &q, None)?
        }
        "messageHistory" => Value::Null,
        "increment" | "decrement" => {
            let v = exp_or_body(0)?;
            let d = if name == "increment" { 1 } else { -1 };
            match v {
                Value::Int(i) => Value::Int(i + d),
                Value::Float(f) => Value::Float(f + d as f64),
                other => match other.as_i64() {
                    Some(i) => Value::Int(i + d),
                    None => return Err(CamelError::new(kinds::NUMBER_FORMAT, format!("Not a number: {}", other))),
                },
            }
        }
        other => return Err(CamelError::expression(format!("Unknown function: {}", other))),
    })
}

fn decode_entities(s: &str) -> String {
    s.replace("&comma;", ",").replace("&empty;", "").replace("&quot;", "\"").replace("&apos;", "'").replace("&space;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex() -> Exchange {
        let mut e = Exchange::with_body(None, "Hello World");
        e.set_header("foo", "abc");
        e.set_header("num", 10);
        e
    }

    #[test]
    fn templating_basics() {
        let e = ex();
        assert_eq!(evaluate("${body}", &e).unwrap(), Value::from("Hello World"));
        assert_eq!(evaluate("Hi ${header.foo}!", &e).unwrap(), Value::from("Hi abc!"));
        assert_eq!(evaluate("${header.num}", &e).unwrap(), Value::Int(10));
        assert_eq!(evaluate("a\\nb", &e).unwrap(), Value::from("a\nb"));
        assert_eq!(evaluate("$${body}", &e).unwrap(), Value::from("${body}"));
    }

    #[test]
    fn predicates_basic() {
        let e = ex();
        assert!(matches("${header.foo} == 'abc'", &e).unwrap());
        assert!(matches("${header.num} > 5 && ${header.num} < 20", &e).unwrap());
        assert!(!matches("${header.num} > 5 && ${header.num} > 20", &e).unwrap());
        assert!(matches("${body} contains 'World'", &e).unwrap());
    }

    #[test]
    fn syntax_errors() {
        let e = SimpleExpression::parse("${body").err().unwrap();
        assert_eq!(e.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX);
        assert!(SimpleExpression::parse("${foo}").is_err());
        assert!(SimplePredicate::parse("${body} ==").is_err());
    }
}
