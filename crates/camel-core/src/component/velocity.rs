//! A Velocity Template Language (VTL 2.x) engine for the `velocity` component.
//!
//! References: `$var`, `${var}`, quiet `$!var` / `$!{var}`, properties `$a.b` (map keys,
//! JavaBean getters such as `$list.empty`), method calls `$s.toUpperCase()`,
//! `$headers.get("x")`, `$exchange.getIn().getHeader("x")`, index access `$list[0]`,
//! `$map["k"]`. Unresolved (null) references render literally unless quiet.
//!
//! Directives: `#set($a = expr)` (also `$map.key`/`$list[0]` targets), `#if/#elseif/#else/#end`,
//! `#foreach($x in expr)` with `$foreach.count/index/hasNext/first/last/parent` (and the 1.x
//! `$velocityCount`/`$velocityHasNext`), `#break`, `#stop`, `#macro(name $p ...)` and calls
//! `#name(args)` / block calls `#@name(args)...#end` with `$bodyContent`, `#include`,
//! `#parse`, `#evaluate`, `#define`, `#{directive}` braces, `##` and `#* *#` comments,
//! `#[[unparsed]]#`, `\$` / `\#` escapes, and the default `lines` space gobbling.
//!
//! Expressions: literals (`"interpolated $x"`, `'literal'`, integers, decimals, `true`,
//! `false`, `null`), lists `[a, b]`, ranges `[1..n]`, maps `{"k": v}`, `+ - * / %`
//! (integer arithmetic on integers, `+` concatenates strings), comparisons `== != < > <= >=`
//! (and `eq ne lt gt le ge`), `&& || !` (and `and or not`), parentheses. Conditions use
//! Velocity 2's empty check: null, `false`, empty strings/collections/maps and zero are false.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::value::{ObjectMap, Value};

use super::template_support::{gobble_lines, java_method, java_property, java_string, GobbleToken, PartialLoader, TemplateEngine};

fn perr(m: impl Into<String>) -> CamelError {
    CamelError::new("ParseErrorException", format!("Velocity: {}", m.into()))
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Part {
    Prop(String),
    Method(String, Vec<Expr>),
    Index(Expr),
}

#[derive(Debug, Clone)]
struct VRef {
    quiet: bool,
    root: String,
    parts: Vec<Part>,
    raw: String,
}

#[derive(Debug, Clone)]
enum Expr {
    Lit(Value),
    Interp(Arc<Vec<Node>>),
    Ref(Box<VRef>),
    List(Vec<Expr>),
    Range(Box<Expr>, Box<Expr>),
    Map(Vec<(Expr, Expr)>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Word(String),
}

#[derive(Debug, Clone)]
struct Macro {
    params: Vec<String>,
    body: Arc<Vec<Node>>,
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Ref(VRef),
    Set(VRef, Expr),
    If(Vec<(Expr, Vec<Node>)>, Option<Vec<Node>>),
    Foreach(String, Expr, Vec<Node>),
    Break,
    Stop,
    Call { name: String, args: Vec<Expr>, body: Option<Arc<Vec<Node>>>, raw: String },
    Include(Vec<Expr>),
    Parse(Expr),
    Evaluate(Expr),
    Define(String, Arc<Vec<Node>>),
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Tok {
    Text(String),
    Ref(VRef),
    Set(VRef, Expr),
    If(Expr),
    ElseIf(Expr),
    Else,
    End,
    Foreach(String, Expr),
    Break,
    Stop,
    Macro(String, Vec<String>),
    Call(String, Vec<Expr>, bool, String),
    Include(Vec<Expr>),
    Parse(Expr),
    Evaluate(Expr),
    Define(String),
    Comment,
}

impl GobbleToken for Tok {
    fn text_mut(&mut self) -> Option<&mut String> {
        match self {
            Tok::Text(t) => Some(t),
            _ => None,
        }
    }
    fn gobbles(&self) -> bool {
        matches!(self, Tok::Set(..) | Tok::If(_) | Tok::ElseIf(_) | Tok::Else | Tok::End | Tok::Foreach(..) | Tok::Break | Tok::Stop | Tok::Macro(..) | Tok::Define(_) | Tok::Comment)
    }
}


struct Lexer<'s> {
    c: &'s [char],
    i: usize,
}

fn is_id_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_id(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

impl<'s> Lexer<'s> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }
    fn ws(&mut self) {
        while self.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
            self.i += 1;
        }
    }
    fn starts(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.at(k) == Some(ch))
    }
    fn ident(&mut self) -> String {
        let st = self.i;
        while self.peek().map(is_id).unwrap_or(false) {
            self.i += 1;
        }
        // an identifier never ends with '-'
        while self.i > st && self.c[self.i - 1] == '-' {
            self.i -= 1;
        }
        self.c[st..self.i].iter().collect()
    }

    fn tokens(&mut self) -> Result<Vec<Tok>> {
        let mut out: Vec<Tok> = Vec::new();
        let mut text = String::new();
        macro_rules! flush {
            () => {
                if !text.is_empty() {
                    out.push(Tok::Text(std::mem::take(&mut text)));
                }
            };
        }
        while let Some(c) = self.peek() {
            if c == '\\' && matches!(self.at(1), Some('$') | Some('#')) {
                text.push(self.at(1).unwrap());
                self.i += 2;
                // copy the escaped reference / directive name literally
                while let Some(n) = self.peek() {
                    if is_id(n) || n == '!' || n == '{' || n == '}' {
                        text.push(n);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }
            if c == '$' {
                let save = self.i;
                match self.reference()? {
                    Some(r) => {
                        flush!();
                        out.push(Tok::Ref(r));
                    }
                    None => {
                        self.i = save + 1;
                        text.push('$');
                    }
                }
                continue;
            }
            if c == '#' {
                if self.at(1) == Some('#') {
                    // line comment, including the newline; drop indentation when alone
                    let line_start = text.rfind('\n').map(|p| p + 1).unwrap_or(0);
                    let alone = text[line_start..].chars().all(|c| c == ' ' || c == '\t') && (line_start > 0 || out.is_empty());
                    if alone {
                        text.truncate(line_start);
                    }
                    while let Some(n) = self.peek() {
                        self.i += 1;
                        if n == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                if self.at(1) == Some('*') {
                    let start = self.i;
                    self.i += 2;
                    while self.i < self.c.len() && !self.starts("*#") {
                        self.i += 1;
                    }
                    if self.i >= self.c.len() {
                        return Err(perr(format!("unclosed block comment at offset {}", start)));
                    }
                    self.i += 2;
                    flush!();
                    out.push(Tok::Comment);
                    continue;
                }
                if self.starts("#[[") {
                    self.i += 3;
                    while self.i < self.c.len() && !self.starts("]]#") {
                        text.push(self.c[self.i]);
                        self.i += 1;
                    }
                    self.i += 3;
                    continue;
                }
                let save = self.i;
                match self.directive()? {
                    Some(t) => {
                        flush!();
                        out.push(t);
                    }
                    None => {
                        self.i = save + 1;
                        text.push('#');
                    }
                }
                continue;
            }
            text.push(c);
            self.i += 1;
        }
        flush!();
        Ok(out)
    }

    /// A reference at `$`; None if this `$` is literal text.
    fn reference(&mut self) -> Result<Option<VRef>> {
        let start = self.i;
        self.i += 1;
        let quiet = self.peek() == Some('!');
        if quiet {
            self.i += 1;
        }
        let braced = self.peek() == Some('{');
        if braced {
            self.i += 1;
        }
        if !self.peek().map(is_id_start).unwrap_or(false) {
            return Ok(None);
        }
        let root = self.ident();
        let mut parts = Vec::new();
        loop {
            if self.peek() == Some('.') && self.at(1).map(is_id_start).unwrap_or(false) {
                self.i += 1;
                let name = self.ident();
                if self.peek() == Some('(') {
                    self.i += 1;
                    let args = self.args(')')?;
                    parts.push(Part::Method(name, args));
                } else {
                    parts.push(Part::Prop(name));
                }
                continue;
            }
            if self.peek() == Some('[') {
                let save = self.i;
                self.i += 1;
                self.ws();
                match self.expr() {
                    Ok(e) => {
                        self.ws();
                        if self.peek() == Some(']') {
                            self.i += 1;
                            parts.push(Part::Index(e));
                            continue;
                        }
                        self.i = save;
                    }
                    Err(_) => self.i = save,
                }
            }
            break;
        }
        if braced {
            if self.peek() != Some('}') {
                return Err(perr(format!("unclosed reference ${{{} at offset {}", root, start)));
            }
            self.i += 1;
        }
        let raw: String = self.c[start..self.i].iter().collect();
        Ok(Some(VRef { quiet, root, parts, raw }))
    }

    fn args(&mut self, close: char) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(close) {
                self.i += 1;
                return Ok(args);
            }
            if self.peek().is_none() {
                return Err(perr(format!("expected '{}'", close)));
            }
            args.push(self.expr()?);
            self.ws();
            if self.peek() == Some(',') {
                self.i += 1;
            }
        }
    }

    fn directive(&mut self) -> Result<Option<Tok>> {
        let start = self.i;
        self.i += 1;
        let braced = self.peek() == Some('{');
        if braced {
            self.i += 1;
        }
        let block_call = !braced && self.peek() == Some('@');
        if block_call {
            self.i += 1;
        }
        if !self.peek().map(is_id_start).unwrap_or(false) {
            return Ok(None);
        }
        let st = self.i;
        while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
            self.i += 1;
        }
        let name: String = self.c[st..self.i].iter().collect();
        if braced {
            if self.peek() != Some('}') {
                return Ok(None);
            }
            self.i += 1;
        }
        let paren = |me: &mut Lexer| -> bool {
            let save = me.i;
            while me.peek() == Some(' ') || me.peek() == Some('\t') {
                me.i += 1;
            }
            if me.peek() == Some('(') {
                me.i += 1;
                true
            } else {
                me.i = save;
                false
            }
        };
        if block_call {
            let args = if paren(self) { self.args(')')? } else { Vec::new() };
            let raw: String = self.c[start..self.i].iter().collect();
            return Ok(Some(Tok::Call(name, args, true, raw)));
        }
        Ok(Some(match name.as_str() {
            "else" => Tok::Else,
            "end" => Tok::End,
            "break" => {
                if paren(self) {
                    self.args(')')?;
                }
                Tok::Break
            }
            "stop" => Tok::Stop,
            "set" => {
                if !paren(self) {
                    return Err(perr("#set requires (...)"));
                }
                self.ws();
                if self.peek() != Some('$') {
                    return Err(perr("#set: expected a reference"));
                }
                let r = self.reference()?.ok_or_else(|| perr("#set: expected a reference"))?;
                self.ws();
                if self.peek() != Some('=') {
                    return Err(perr("#set: expected '='"));
                }
                self.i += 1;
                self.ws();
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(perr("#set: expected ')'"));
                }
                self.i += 1;
                Tok::Set(r, e)
            }
            "if" | "elseif" => {
                if !paren(self) {
                    return Err(perr(format!("#{} requires (...)", name)));
                }
                self.ws();
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(perr(format!("#{}: expected ')'", name)));
                }
                self.i += 1;
                if name == "if" {
                    Tok::If(e)
                } else {
                    Tok::ElseIf(e)
                }
            }
            "foreach" => {
                if !paren(self) {
                    return Err(perr("#foreach requires (...)"));
                }
                self.ws();
                let r = self.reference()?.ok_or_else(|| perr("#foreach: expected a reference"))?;
                self.ws();
                if !self.starts("in") {
                    return Err(perr("#foreach: expected 'in'"));
                }
                self.i += 2;
                self.ws();
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(perr("#foreach: expected ')'"));
                }
                self.i += 1;
                Tok::Foreach(r.root, e)
            }
            "macro" => {
                if !paren(self) {
                    return Err(perr("#macro requires (...)"));
                }
                self.ws();
                let mname = self.ident();
                if mname.is_empty() {
                    return Err(perr("#macro: expected a name"));
                }
                let mut params = Vec::new();
                loop {
                    self.ws();
                    match self.peek() {
                        Some(')') => {
                            self.i += 1;
                            break;
                        }
                        Some(',') => self.i += 1,
                        Some('$') => {
                            let r = self.reference()?.ok_or_else(|| perr("#macro: bad parameter"))?;
                            params.push(r.root);
                            self.ws();
                            if self.peek() == Some('=') {
                                // default values (Velocity 2): ignored, parameter stays optional
                                self.i += 1;
                                self.ws();
                                self.expr()?;
                            }
                        }
                        _ => return Err(perr("#macro: bad parameter list")),
                    }
                }
                Tok::Macro(mname, params)
            }
            "include" => {
                if !paren(self) {
                    return Err(perr("#include requires (...)"));
                }
                Tok::Include(self.args(')')?)
            }
            "parse" | "evaluate" => {
                if !paren(self) {
                    return Err(perr(format!("#{} requires (...)", name)));
                }
                let mut a = self.args(')')?;
                if a.len() != 1 {
                    return Err(perr(format!("#{} takes one argument", name)));
                }
                if name == "parse" {
                    Tok::Parse(a.remove(0))
                } else {
                    Tok::Evaluate(a.remove(0))
                }
            }
            "define" => {
                if !paren(self) {
                    return Err(perr("#define requires (...)"));
                }
                self.ws();
                let r = self.reference()?.ok_or_else(|| perr("#define: expected a reference"))?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(perr("#define: expected ')'"));
                }
                self.i += 1;
                Tok::Define(r.root)
            }
            _ => {
                // a macro call (resolved at render time)
                if !paren(self) {
                    return Ok(None);
                }
                let args = self.args(')')?;
                let raw: String = self.c[start..self.i].iter().collect();
                Tok::Call(name, args, false, raw)
            }
        }))
    }

    // --- expressions ---

    fn kw(&mut self, w: &str) -> bool {
        if self.starts(w) && !self.at(w.chars().count()).map(is_id).unwrap_or(false) {
            self.i += w.chars().count();
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<Expr> {
        self.or()
    }
    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        loop {
            self.ws();
            if self.starts("||") {
                self.i += 2;
            } else if !self.kw("or") {
                return Ok(l);
            }
            self.ws();
            let r = self.and()?;
            l = Expr::Bin("||", Box::new(l), Box::new(r));
        }
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.eq()?;
        loop {
            self.ws();
            if self.starts("&&") {
                self.i += 2;
            } else if !self.kw("and") {
                return Ok(l);
            }
            self.ws();
            let r = self.eq()?;
            l = Expr::Bin("&&", Box::new(l), Box::new(r));
        }
    }
    fn eq(&mut self) -> Result<Expr> {
        let mut l = self.rel()?;
        loop {
            self.ws();
            let op = if self.starts("==") {
                self.i += 2;
                "=="
            } else if self.starts("!=") {
                self.i += 2;
                "!="
            } else if self.kw("eq") {
                "=="
            } else if self.kw("ne") {
                "!="
            } else {
                return Ok(l);
            };
            self.ws();
            let r = self.rel()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn rel(&mut self) -> Result<Expr> {
        let mut l = self.add()?;
        loop {
            self.ws();
            let op = if self.starts("<=") {
                self.i += 2;
                "<="
            } else if self.starts(">=") {
                self.i += 2;
                ">="
            } else if self.starts("<") {
                self.i += 1;
                "<"
            } else if self.starts(">") {
                self.i += 1;
                ">"
            } else if self.kw("lt") {
                "<"
            } else if self.kw("gt") {
                ">"
            } else if self.kw("le") {
                "<="
            } else if self.kw("ge") {
                ">="
            } else {
                return Ok(l);
            };
            self.ws();
            let r = self.add()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn add(&mut self) -> Result<Expr> {
        let mut l = self.mul()?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some('+') => "+",
                Some('-') if !self.starts("--") => "-",
                _ => return Ok(l),
            };
            self.i += 1;
            self.ws();
            let r = self.mul()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn mul(&mut self) -> Result<Expr> {
        let mut l = self.unary()?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some('*') => "*",
                Some('/') => "/",
                Some('%') => "%",
                _ => return Ok(l),
            };
            self.i += 1;
            self.ws();
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn unary(&mut self) -> Result<Expr> {
        self.ws();
        if self.peek() == Some('!') {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.kw("not") {
            self.ws();
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.peek() == Some('-') && !self.at(1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.i += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<Expr> {
        self.ws();
        let c = self.peek().ok_or_else(|| perr("unexpected end of template in expression"))?;
        match c {
            '$' => {
                let r = self.reference()?.ok_or_else(|| perr("bad reference in expression"))?;
                Ok(Expr::Ref(Box::new(r)))
            }
            '"' | '\'' => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    let ch = self.peek().ok_or_else(|| perr("unterminated string literal"))?;
                    self.i += 1;
                    if ch == c {
                        if self.peek() == Some(c) {
                            s.push(c);
                            self.i += 1;
                            continue;
                        }
                        break;
                    }
                    if ch == '\\' && self.peek() == Some(c) {
                        s.push(c);
                        self.i += 1;
                        continue;
                    }
                    s.push(ch);
                }
                if c == '"' && (s.contains('$') || s.contains('#')) {
                    let chars: Vec<char> = s.chars().collect();
                    let nodes = parse(&chars)?;
                    Ok(Expr::Interp(Arc::new(nodes)))
                } else {
                    Ok(Expr::Lit(Value::String(s)))
                }
            }
            '[' => {
                self.i += 1;
                self.ws();
                if self.peek() == Some(']') {
                    self.i += 1;
                    return Ok(Expr::List(vec![]));
                }
                let first = self.expr()?;
                self.ws();
                if self.starts("..") {
                    self.i += 2;
                    let hi = self.expr()?;
                    self.ws();
                    if self.peek() != Some(']') {
                        return Err(perr("expected ']' after range"));
                    }
                    self.i += 1;
                    return Ok(Expr::Range(Box::new(first), Box::new(hi)));
                }
                let mut items = vec![first];
                loop {
                    self.ws();
                    match self.peek() {
                        Some(',') => {
                            self.i += 1;
                            items.push(self.expr()?);
                        }
                        Some(']') => {
                            self.i += 1;
                            return Ok(Expr::List(items));
                        }
                        _ => return Err(perr("expected ',' or ']' in list")),
                    }
                }
            }
            '{' => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    self.ws();
                    if self.peek() == Some('}') {
                        self.i += 1;
                        return Ok(Expr::Map(entries));
                    }
                    let k = self.expr()?;
                    self.ws();
                    if self.peek() != Some(':') {
                        return Err(perr("expected ':' in map literal"));
                    }
                    self.i += 1;
                    let v = self.expr()?;
                    entries.push((k, v));
                    self.ws();
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            '(' => {
                self.i += 1;
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(perr("expected ')'"));
                }
                self.i += 1;
                Ok(e)
            }
            c if c.is_ascii_digit() || c == '-' => {
                let st = self.i;
                self.i += 1;
                while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    self.i += 1;
                }
                let mut float = false;
                if self.peek() == Some('.') && self.at(1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    float = true;
                    self.i += 1;
                    while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        self.i += 1;
                    }
                }
                let t: String = self.c[st..self.i].iter().collect();
                Ok(Expr::Lit(if float { Value::Float(t.parse().map_err(|_| perr("bad number"))?) } else { Value::Int(t.parse().map_err(|_| perr("bad number"))?) }))
            }
            c if is_id_start(c) => {
                let w = self.ident();
                Ok(match w.as_str() {
                    "true" => Expr::Lit(Value::Bool(true)),
                    "false" => Expr::Lit(Value::Bool(false)),
                    "null" => Expr::Lit(Value::Null),
                    _ => Expr::Word(w),
                })
            }
            other => Err(perr(format!("unexpected '{}' in expression", other))),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser (tokens -> tree)
// ---------------------------------------------------------------------------

fn parse(chars: &[char]) -> Result<Vec<Node>> {
    let mut toks = Lexer { c: chars, i: 0 }.tokens()?;
    gobble_lines(&mut toks);
    let mut it = toks.into_iter().peekable();
    let (nodes, end) = parse_block(&mut it)?;
    if let Some(e) = end {
        return Err(perr(format!("unexpected {}", e)));
    }
    Ok(nodes)
}

type Toks = std::iter::Peekable<std::vec::IntoIter<Tok>>;

/// Parse until `#end`/`#else`/`#elseif` (returned) or the end of input.
fn parse_block(it: &mut Toks) -> Result<(Vec<Node>, Option<&'static str>)> {
    let mut nodes = Vec::new();
    while let Some(t) = it.next() {
        match t {
            Tok::Text(s) => {
                if !s.is_empty() {
                    nodes.push(Node::Text(s))
                }
            }
            Tok::Comment => {}
            Tok::Ref(r) => nodes.push(Node::Ref(r)),
            Tok::Set(r, e) => nodes.push(Node::Set(r, e)),
            Tok::Break => nodes.push(Node::Break),
            Tok::Stop => nodes.push(Node::Stop),
            Tok::Include(a) => nodes.push(Node::Include(a)),
            Tok::Parse(e) => nodes.push(Node::Parse(e)),
            Tok::Evaluate(e) => nodes.push(Node::Evaluate(e)),
            Tok::End => return Ok((nodes, Some("#end"))),
            Tok::Else => {
                // stash marker by returning; the caller decides
                return Ok((nodes, Some("#else")));
            }
            Tok::ElseIf(e) => {
                ELSEIF.with(|c| *c.borrow_mut() = Some(e));
                return Ok((nodes, Some("#elseif")));
            }
            Tok::If(cond) => {
                let mut branches = Vec::new();
                let mut cond = cond;
                let mut otherwise = None;
                loop {
                    let (body, end) = parse_block(it)?;
                    match end {
                        Some("#end") => {
                            branches.push((cond, body));
                            break;
                        }
                        Some("#elseif") => {
                            branches.push((cond, body));
                            cond = ELSEIF.with(|c| c.borrow_mut().take()).unwrap();
                        }
                        Some("#else") => {
                            branches.push((cond, body));
                            let (eb, end2) = parse_block(it)?;
                            if end2 != Some("#end") {
                                return Err(perr("#else without matching #end"));
                            }
                            otherwise = Some(eb);
                            break;
                        }
                        _ => return Err(perr("#if without matching #end")),
                    }
                }
                nodes.push(Node::If(branches, otherwise));
            }
            Tok::Foreach(var, e) => {
                let (body, end) = parse_block(it)?;
                if end != Some("#end") {
                    return Err(perr("#foreach without matching #end"));
                }
                nodes.push(Node::Foreach(var, e, body));
            }
            Tok::Macro(name, params) => {
                let (body, end) = parse_block(it)?;
                if end != Some("#end") {
                    return Err(perr("#macro without matching #end"));
                }
                MACROS.with(|m| m.borrow_mut().push((name, Macro { params, body: Arc::new(body) })));
            }
            Tok::Define(name) => {
                let (body, end) = parse_block(it)?;
                if end != Some("#end") {
                    return Err(perr("#define without matching #end"));
                }
                nodes.push(Node::Define(name, Arc::new(body)));
            }
            Tok::Call(name, args, block, raw) => {
                let body = if block {
                    let (b, end) = parse_block(it)?;
                    if end != Some("#end") {
                        return Err(perr(format!("#@{} without matching #end", name)));
                    }
                    Some(Arc::new(b))
                } else {
                    None
                };
                nodes.push(Node::Call { name, args, body, raw });
            }
        }
    }
    Ok((nodes, None))
}

thread_local! {
    static ELSEIF: std::cell::RefCell<Option<Expr>> = const { std::cell::RefCell::new(None) };
    static MACROS: std::cell::RefCell<Vec<(String, Macro)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Parse a template; returns the nodes and the macros it defines.
fn compile(template: &str) -> Result<(Vec<Node>, Vec<(String, Macro)>)> {
    MACROS.with(|m| m.borrow_mut().clear());
    let chars: Vec<char> = template.chars().collect();
    let r = parse(&chars);
    let macros = MACROS.with(|m| std::mem::take(&mut *m.borrow_mut()));
    Ok((r?, macros))
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

enum Flow {
    Normal,
    Break,
    Stop,
}

struct Ctx<'p> {
    scopes: Vec<ObjectMap>,
    macros: HashMap<String, Macro>,
    defines: HashMap<String, Arc<Vec<Node>>>,
    partials: PartialLoader<'p>,
    depth: usize,
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
    }
}

fn num_op(op: &str, a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => match op {
            "+" => x.checked_add(*y).map(Value::Int).unwrap_or(Value::Float(*x as f64 + *y as f64)),
            "-" => x.checked_sub(*y).map(Value::Int).unwrap_or(Value::Float(*x as f64 - *y as f64)),
            "*" => x.checked_mul(*y).map(Value::Int).unwrap_or(Value::Float(*x as f64 * *y as f64)),
            "/" => {
                if *y == 0 {
                    Value::Null
                } else {
                    Value::Int(x / y)
                }
            }
            _ => {
                if *y == 0 {
                    Value::Null
                } else {
                    Value::Int(x % y)
                }
            }
        },
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) if !matches!(a, Value::String(_)) && !matches!(b, Value::String(_)) => match op {
                "+" => Value::Float(x + y),
                "-" => Value::Float(x - y),
                "*" => Value::Float(x * y),
                "/" => {
                    if y == 0.0 {
                        Value::Null
                    } else {
                        Value::Float(x / y)
                    }
                }
                _ => {
                    if y == 0.0 {
                        Value::Null
                    } else {
                        Value::Float(x % y)
                    }
                }
            },
            _ => Value::Null,
        },
    }
}

fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
        (Value::Null, _) | (_, Value::Null) => None,
        _ => Some(java_string(a).cmp(&java_string(b))),
    }
}

fn equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64() == b.as_f64(),
        _ => java_string(a) == java_string(b),
    }
}

impl<'p> Ctx<'p> {
    fn get(&self, name: &str) -> Option<Value> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    fn set(&mut self, name: &str, v: Value) {
        for s in self.scopes.iter_mut().skip(1).rev() {
            if s.contains_key(name) {
                s.insert(name.to_string(), v);
                return;
            }
        }
        if v.is_null() {
            self.scopes[0].remove(name);
        } else {
            self.scopes[0].insert(name.to_string(), v);
        }
    }

    /// Resolve a reference; None when unresolved (undefined, null, unknown method).
    fn resolve(&mut self, r: &VRef) -> Result<Option<Value>> {
        let mut cur = match self.get(&r.root) {
            Some(v) => v,
            None => match self.defines.get(&r.root).cloned() {
                Some(nodes) => {
                    let mut out = String::new();
                    self.render(&nodes, &mut out)?;
                    Value::String(out)
                }
                None => return Ok(None),
            },
        };
        for p in &r.parts {
            if cur.is_null() {
                return Ok(None);
            }
            let next = match p {
                Part::Prop(name) => java_property(&cur, name),
                Part::Method(name, args) => {
                    let mut vals = Vec::new();
                    for a in args {
                        vals.push(self.eval(a)?);
                    }
                    java_method(&cur, name, &vals)
                }
                Part::Index(e) => {
                    let k = self.eval(e)?;
                    match (&cur, &k) {
                        (Value::Array(a), _) => k.as_i64().and_then(|i| {
                            let i = if i < 0 { a.len() as i64 + i } else { i };
                            a.get(i as usize).cloned()
                        }),
                        (Value::Object(_), _) => super::template_support::lookup_key(&cur, &k.to_string()).cloned(),
                        _ => None,
                    }
                }
            };
            match next {
                Some(v) => cur = v,
                None => return Ok(None),
            }
        }
        Ok(if cur.is_null() { None } else { Some(cur) })
    }

    fn eval(&mut self, e: &Expr) -> Result<Value> {
        Ok(match e {
            Expr::Lit(v) => v.clone(),
            Expr::Word(w) => Value::String(w.clone()),
            Expr::Interp(nodes) => {
                let mut out = String::new();
                self.render(nodes, &mut out)?;
                Value::String(out)
            }
            Expr::Ref(r) => self.resolve(r)?.unwrap_or(Value::Null),
            Expr::List(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.push(self.eval(i)?);
                }
                Value::Array(v)
            }
            Expr::Range(a, b) => {
                let (x, y) = (self.eval(a)?.as_i64(), self.eval(b)?.as_i64());
                match (x, y) {
                    (Some(x), Some(y)) => {
                        if x <= y {
                            Value::Array((x..=y).map(Value::Int).collect())
                        } else {
                            Value::Array((y..=x).rev().map(Value::Int).collect())
                        }
                    }
                    _ => Value::Null,
                }
            }
            Expr::Map(entries) => {
                let mut m = ObjectMap::new();
                for (k, v) in entries {
                    let k = self.eval(k)?.to_string();
                    let v = self.eval(v)?;
                    m.insert(k, v);
                }
                Value::Object(m)
            }
            Expr::Not(x) => Value::Bool(!truthy(&self.eval(x)?)),
            Expr::Neg(x) => num_op("-", &Value::Int(0), &self.eval(x)?),
            Expr::Bin(op, a, b) => match *op {
                "&&" => Value::Bool(truthy(&self.eval(a)?) && truthy(&self.eval(b)?)),
                "||" => Value::Bool(truthy(&self.eval(a)?) || truthy(&self.eval(b)?)),
                "==" => Value::Bool(equals(&self.eval(a)?, &self.eval(b)?)),
                "!=" => Value::Bool(!equals(&self.eval(a)?, &self.eval(b)?)),
                "<" | ">" | "<=" | ">=" => {
                    let o = compare(&self.eval(a)?, &self.eval(b)?);
                    Value::Bool(match (*op, o) {
                        (_, None) => false,
                        ("<", Some(o)) => o.is_lt(),
                        (">", Some(o)) => o.is_gt(),
                        ("<=", Some(o)) => o.is_le(),
                        (_, Some(o)) => o.is_ge(),
                    })
                }
                "+" => {
                    let (x, y) = (self.eval(a)?, self.eval(b)?);
                    if matches!(x, Value::String(_)) || matches!(y, Value::String(_)) {
                        Value::String(format!("{}{}", render_value(&x), render_value(&y)))
                    } else {
                        num_op("+", &x, &y)
                    }
                }
                o => {
                    let (x, y) = (self.eval(a)?, self.eval(b)?);
                    num_op(o, &x, &y)
                }
            },
        })
    }

    fn render(&mut self, nodes: &[Node], out: &mut String) -> Result<Flow> {
        for n in nodes {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Ref(r) => match self.resolve(r)? {
                    Some(v) => out.push_str(&render_value(&v)),
                    None if r.quiet => {}
                    None => out.push_str(&r.raw),
                },
                Node::Set(target, e) => {
                    let v = self.eval(e)?;
                    self.assign(target, v)?;
                }
                Node::If(branches, otherwise) => {
                    let mut done = false;
                    for (c, body) in branches {
                        if truthy(&self.eval(c)?) {
                            done = true;
                            match self.render(body, out)? {
                                Flow::Normal => {}
                                f => return Ok(f),
                            }
                            break;
                        }
                    }
                    if !done {
                        if let Some(o) = otherwise {
                            match self.render(o, out)? {
                                Flow::Normal => {}
                                f => return Ok(f),
                            }
                        }
                    }
                }
                Node::Foreach(var, e, body) => {
                    let items = match self.eval(e)? {
                        Value::Array(a) => a,
                        Value::Object(o) => o.into_values().collect(),
                        Value::Null => Vec::new(),
                        other => vec![other],
                    };
                    let parent = self.get("foreach");
                    let n = items.len();
                    for (i, item) in items.into_iter().enumerate() {
                        let mut scope = ObjectMap::new();
                        let mut fe = ObjectMap::new();
                        fe.insert("index".into(), Value::Int(i as i64));
                        fe.insert("count".into(), Value::Int(i as i64 + 1));
                        fe.insert("hasNext".into(), Value::Bool(i + 1 < n));
                        fe.insert("first".into(), Value::Bool(i == 0));
                        fe.insert("last".into(), Value::Bool(i + 1 == n));
                        if let Some(p) = &parent {
                            fe.insert("parent".into(), p.clone());
                        }
                        scope.insert("foreach".into(), Value::Object(fe));
                        scope.insert("velocityCount".into(), Value::Int(i as i64 + 1));
                        scope.insert("velocityHasNext".into(), Value::Bool(i + 1 < n));
                        scope.insert(var.clone(), item);
                        self.scopes.push(scope);
                        let f = self.render(body, out);
                        self.scopes.pop();
                        match f? {
                            Flow::Normal => {}
                            Flow::Break => break,
                            Flow::Stop => return Ok(Flow::Stop),
                        }
                    }
                }
                Node::Break => return Ok(Flow::Break),
                Node::Stop => return Ok(Flow::Stop),
                Node::Define(name, body) => {
                    self.defines.insert(name.clone(), body.clone());
                }
                Node::Call { name, args, body, raw } => {
                    let Some(m) = self.macros.get(name).cloned() else {
                        out.push_str(raw);
                        if body.is_some() {
                            return Err(perr(format!("unknown block macro #@{}", name)));
                        }
                        continue;
                    };
                    let mut scope = ObjectMap::new();
                    for (k, p) in m.params.iter().enumerate() {
                        let v = match args.get(k) {
                            Some(a) => self.eval(a)?,
                            None => Value::Null,
                        };
                        scope.insert(p.clone(), v);
                    }
                    if let Some(b) = body {
                        let mut content = String::new();
                        self.render(b, &mut content)?;
                        scope.insert("bodyContent".into(), Value::String(content));
                    }
                    self.depth += 1;
                    if self.depth > 100 {
                        return Err(CamelError::new("MacroOverflowException", format!("Velocity: macro #{} recursion too deep", name)));
                    }
                    self.scopes.push(scope);
                    let f = self.render(&m.body, out);
                    self.scopes.pop();
                    self.depth -= 1;
                    if let Flow::Stop = f? {
                        return Ok(Flow::Stop);
                    }
                }
                Node::Include(args) => {
                    for a in args {
                        let name = self.eval(a)?.to_string();
                        let text = (self.partials)(&name).ok_or_else(|| CamelError::new("ResourceNotFoundException", format!("Velocity: unable to find resource '{}'", name)))?;
                        out.push_str(&text);
                    }
                }
                Node::Parse(e) | Node::Evaluate(e) => {
                    let text = match n {
                        Node::Parse(_) => {
                            let name = self.eval(e)?.to_string();
                            (self.partials)(&name).ok_or_else(|| CamelError::new("ResourceNotFoundException", format!("Velocity: unable to find resource '{}'", name)))?
                        }
                        _ => self.eval(e)?.to_string(),
                    };
                    self.depth += 1;
                    if self.depth > 20 {
                        return Err(perr("#parse/#evaluate nested too deeply"));
                    }
                    let (nodes, macros) = compile(&text)?;
                    for (k, m) in macros {
                        self.macros.insert(k, m);
                    }
                    let f = self.render(&nodes, out);
                    self.depth -= 1;
                    if let Flow::Stop = f? {
                        return Ok(Flow::Stop);
                    }
                }
            }
        }
        Ok(Flow::Normal)
    }

    fn assign(&mut self, target: &VRef, v: Value) -> Result<()> {
        if target.parts.is_empty() {
            self.set(&target.root, v);
            return Ok(());
        }
        // Navigate to the container and set the last key/index.
        let mut root = self.get(&target.root).unwrap_or(Value::Null);
        let mut keys = Vec::new();
        for p in &target.parts {
            keys.push(match p {
                Part::Prop(n) => Value::String(n.clone()),
                Part::Index(e) => self.eval(e)?,
                Part::Method(..) => return Err(perr("#set: cannot assign to a method call")),
            });
        }
        fn put(cur: &mut Value, keys: &[Value], v: Value) {
            let (k, rest) = keys.split_first().unwrap();
            match cur {
                Value::Object(o) => {
                    let key = k.to_string();
                    if rest.is_empty() {
                        o.insert(key, v);
                    } else {
                        let child = o.entry(key).or_insert(Value::Object(ObjectMap::new()));
                        put(child, rest, v);
                    }
                }
                Value::Array(a) => {
                    if let Some(i) = k.as_i64().filter(|i| *i >= 0 && (*i as usize) < a.len()) {
                        if rest.is_empty() {
                            a[i as usize] = v;
                        } else {
                            put(&mut a[i as usize], rest, v);
                        }
                    }
                }
                _ => {}
            }
        }
        if root.is_null() {
            return Ok(());
        }
        put(&mut root, &keys, v);
        self.set(&target.root, root);
        Ok(())
    }
}

fn render_value(v: &Value) -> String {
    java_string(v)
}

/// The Velocity engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct VelocityEngine;

impl TemplateEngine for VelocityEngine {
    fn render(&self, template: &str, context: &Value, partials: PartialLoader<'_>) -> Result<String> {
        let (nodes, macros) = compile(template)?;
        let global = match context {
            Value::Object(o) => o.clone(),
            _ => ObjectMap::new(),
        };
        let mut ctx = Ctx { scopes: vec![global], macros: macros.into_iter().collect(), defines: HashMap::new(), partials, depth: 0 };
        let mut out = String::new();
        ctx.render(&nodes, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: &str) -> String {
        let d = crate::json::parse(r#"{"body":"B","headers":{"name":"N","list":["a","b","c"],"n":3,"m":{"k":"v"},"empty":""},"exchange":{"properties":{"p":1},"in":{"headers":{"x":"X"},"body":"B"}}}"#).unwrap();
        VelocityEngine.render(t, &d, &|n| if n == "inc.vm" { Some("[$body]".into()) } else { None }).unwrap_or_else(|e| panic!("{}: {}", t, e))
    }

    #[test]
    fn references() {
        assert_eq!(r("Dear $headers.name. ${body}! $!missing|$missing|\\$body ## comment\nx"), "Dear N. B! |$missing|$body x");
        assert_eq!(r("cost: $5 #* block *#ok"), "cost: $5 ok");
        assert_eq!(r("$body.toLowerCase() $headers.get(\"name\") $headers.list.size() $headers.list[1] $headers.list.get(0) $headers.m.k $headers.list.empty"), "b N 3 b a v false");
        assert_eq!(r("$exchange.getIn().getHeader(\"x\") ${exchange.properties.p} $exchange.in.body.length()"), "X 1 1");
        assert_eq!(r("$body.nosuch() $!body.nosuch()|"), "$body.nosuch() |");
        assert_eq!(r("$headers.list $headers.m"), "[a, b, c] {k=v}");
        assert_eq!(r("#[[ $body #if ]]# done"), " $body #if  done");
    }

    #[test]
    fn directives() {
        assert_eq!(r("#set($x = 5)#set($y = $x * 2 + 1)$y"), "11");
        assert_eq!(r("#if($headers.n > 2)big#elseif($headers.n == 2)two#{else}small#end"), "big");
        assert_eq!(r("#if($headers.empty)e#else-#end #if(!$missing)m#end #if($headers.n == 3 && $body == \"B\")ok#end"), "- m ok");
        assert_eq!(r("#foreach($i in $headers.list)$foreach.count:$i#if($foreach.hasNext),#end#end"), "1:a,2:b,3:c");
        assert_eq!(r("#foreach($i in [1..5])#if($i == 3)#break#end$i#end"), "12");
        assert_eq!(r("#foreach($v in $headers.m)$v$velocityCount#end"), "v1");
        assert_eq!(r("#macro(greet $who $p)Hi $who$p#end#greet($headers.name \"!\") #greet(\"x\", \".\")"), "Hi N! Hi x.");
        assert_eq!(r("#macro(wrap)<$!bodyContent>#end#@wrap()in $body#end"), "<in B>");
        assert_eq!(r("#set($s = \"v=$body\")$s #set($l = ['a', 2])$l.size() #set($mm = {\"a\": 1})$mm.a"), "v=B 2 1");
        assert_eq!(r("#set($mm = {})#set($mm.k = 'x')$mm.k"), "x");
        assert_eq!(r("a#stop b"), "a");
        assert_eq!(r("#parse(\"inc.vm\")#include(\"inc.vm\")"), "[B][$body]");
        assert_eq!(r("#evaluate('#set($q = 7)$q')"), "7");
        assert_eq!(r("#define($blk)<$body>#end$blk$blk"), "<B><B>");
        assert_eq!(r("$headers.n % 2 #set($d = 7 / 2)$d #set($f = 7.0 / 2)$f"), "3 % 2 3 3.5");
    }

    #[test]
    fn line_gobbling() {
        assert_eq!(r("<ul>\n#foreach($i in $headers.list)\n  <li>$i</li>\n#end\n</ul>\n"), "<ul>\n  <li>a</li>\n  <li>b</li>\n  <li>c</li>\n</ul>\n");
        assert_eq!(r("  #set($a = 1)\nv=$a\n    #if(true)\n  yes\n    #end\n"), "v=1\n  yes\n");
    }

    #[test]
    fn errors() {
        let d = Value::Null;
        assert!(VelocityEngine.render("#if(true)x", &d, &|_| None).is_err());
        assert!(VelocityEngine.render("#foreach($a in [1])", &d, &|_| None).is_err());
        assert!(VelocityEngine.render("#parse('nope.vm')", &d, &|_| None).is_err());
        assert!(VelocityEngine.render("${unclosed", &d, &|_| None).is_err());
        assert_eq!(VelocityEngine.render("#unknown(1) #notadirective", &d, &|_| None).unwrap(), "#unknown(1) #notadirective");
    }
}
