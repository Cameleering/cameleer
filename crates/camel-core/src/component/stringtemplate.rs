//! A StringTemplate 4 engine for the `string-template` component (single templates, as
//! created by Camel with `new ST(template, delimiterStart, delimiterStop)`).
//!
//! Supported: attribute references `<attr>`, properties `<a.b>` / `<a.(expr)>`, string and
//! list literals (`"x"`, `[a, b]`), multi-valued attributes (concatenated; `; separator=", "`,
//! `; null="n"`), anonymous sub-templates applied to each value `<names:{n | <n>}>` (also with
//! the implicit `it`, the iteration numbers `<i>`/`<i0>`, chained `<a:{..}:{..}>` and zipped
//! `<a, b:{x, y | ..}>` application; maps iterate their keys), `<if(c)>`, `<elseif(c)>`,
//! `<else>`, `<endif>` with `!`, `&&`, `||` and parentheses (an attribute is true when it is
//! present, not `false` and not an empty collection), the functions `first last rest trunc
//! strip length reverse trim strlen`, the special characters `<\n>`, `<\t>`, `<\ >`,
//! `<\uXXXX>`, `<! comments !>`, `\<` escapes, and the removal of lines holding only an
//! `if`/`elseif`/`else`/`endif` tag. Named template references (`<x:t()>`) need template
//! groups and fail with "no such template".

use crate::error::{CamelError, Result};
use crate::value::Value;

use super::template_support::{gobble_lines, java_property, GobbleToken, PartialLoader, TemplateEngine};

fn serr(m: impl Into<String>) -> CamelError {
    CamelError::new("STException", format!("StringTemplate: {}", m.into()))
}

#[derive(Debug, Clone)]
enum Expr {
    Attr(String),
    Str(String),
    Bool(bool),
    List(Vec<Expr>),
    Prop(Box<Expr>, String),
    DynProp(Box<Expr>, Box<Expr>),
    Func(String, Box<Expr>),
    Map(Vec<Expr>, Vec<Template>),
    Indirect(Box<Expr>),
}

#[derive(Debug, Clone)]
struct Template {
    params: Vec<String>,
    body: Vec<Node>,
}

#[derive(Debug, Clone)]
enum Cond {
    Expr(Expr),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Expr(Expr, Option<String>, Option<String>),
    If(Vec<(Cond, Vec<Node>)>, Option<Vec<Node>>),
}

#[derive(Debug)]
enum Tok {
    Text(String),
    Expr(Expr, Option<String>, Option<String>),
    If(Cond),
    ElseIf(Cond),
    Else,
    EndIf,
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
        matches!(self, Tok::If(_) | Tok::ElseIf(_) | Tok::Else | Tok::EndIf | Tok::Comment)
    }
}

struct Lexer<'a> {
    c: &'a [char],
    i: usize,
    start: char,
    stop: char,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn ws(&mut self) {
        while self.peek().map(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r').unwrap_or(false) {
            self.i += 1;
        }
    }
    fn eat(&mut self, s: &str) -> bool {
        self.ws();
        let n = s.chars().count();
        if self.c.get(self.i..self.i + n).map(|w| w.iter().copied().eq(s.chars())).unwrap_or(false) {
            self.i += n;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.peek().map(|c| c.is_alphanumeric() || c == '_' || c == '/').unwrap_or(false) {
            self.i += 1;
        }
        self.c[st..self.i].iter().collect()
    }

    /// Tokens of a template body until end of input, or `}` when `sub` (a sub-template).
    fn tokens(&mut self, sub: bool) -> Result<Vec<Tok>> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut depth = 0;
        while let Some(c) = self.peek() {
            if c == '\\' {
                if let Some(n) = self.c.get(self.i + 1).copied() {
                    if n == self.start || n == '\\' || (sub && (n == '}' || n == '{')) {
                        text.push(n);
                        self.i += 2;
                        continue;
                    }
                }
            }
            if sub && c == '{' {
                depth += 1;
            }
            if sub && c == '}' {
                if depth == 0 {
                    self.i += 1;
                    if !text.is_empty() {
                        out.push(Tok::Text(text));
                    }
                    return Ok(out);
                }
                depth -= 1;
            }
            if c != self.start {
                text.push(c);
                self.i += 1;
                continue;
            }
            if !text.is_empty() {
                out.push(Tok::Text(std::mem::take(&mut text)));
            }
            self.i += 1;
            // comment
            if self.peek() == Some('!') {
                let st = self.i;
                while self.i + 1 < self.c.len() && !(self.c[self.i] == '!' && self.c[self.i + 1] == self.stop) {
                    self.i += 1;
                }
                if self.i + 1 >= self.c.len() {
                    return Err(serr(format!("unterminated comment at offset {}", st)));
                }
                self.i += 2;
                out.push(Tok::Comment);
                continue;
            }
            // special characters
            if self.peek() == Some('\\') {
                let st = self.i;
                while self.i < self.c.len() && self.c[self.i] != self.stop {
                    self.i += 1;
                }
                let spec: String = self.c[st..self.i].iter().collect();
                self.i += 1;
                let mut s = String::new();
                let sc: Vec<char> = spec.chars().collect();
                let mut k = 0;
                while k < sc.len() {
                    if sc[k] == '\\' && k + 1 < sc.len() {
                        match sc[k + 1] {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            ' ' => s.push(' '),
                            '\\' => s.push('\\'),
                            'u' => {
                                let h: String = sc[k + 2..(k + 6).min(sc.len())].iter().collect();
                                s.push(char::from_u32(u32::from_str_radix(&h, 16).map_err(|_| serr("bad \\u escape"))?).unwrap_or('\u{FFFD}'));
                                k += 4;
                            }
                            o => return Err(serr(format!("invalid escape <\\{}>", o))),
                        }
                        k += 2;
                    } else {
                        k += 1;
                    }
                }
                out.push(Tok::Text(s));
                continue;
            }
            self.ws();
            let save = self.i;
            let word = self.ident();
            match word.as_str() {
                "if" | "elseif" if self.peek() == Some('(') => {
                    self.i += 1;
                    let c = self.cond()?;
                    if !self.eat(")") {
                        return Err(serr(format!("{}: expected ')'", word)));
                    }
                    self.close()?;
                    out.push(if word == "if" { Tok::If(c) } else { Tok::ElseIf(c) });
                }
                "else" => {
                    self.close()?;
                    out.push(Tok::Else);
                }
                "endif" => {
                    self.close()?;
                    out.push(Tok::EndIf);
                }
                _ => {
                    self.i = save;
                    let e = self.expr()?;
                    let (mut sep, mut null) = (None, None);
                    if self.eat(";") {
                        loop {
                            let opt = self.ident();
                            if opt.is_empty() {
                                return Err(serr("expected an option name after ';'"));
                            }
                            let val = if self.eat("=") {
                                match self.primary()? {
                                    Expr::Str(s) => Some(s),
                                    _ => None,
                                }
                            } else {
                                None
                            };
                            match opt.as_str() {
                                "separator" => sep = val,
                                "null" => null = val,
                                "format" | "wrap" | "anchor" => {}
                                o => return Err(serr(format!("unknown option '{}'", o))),
                            }
                            if !self.eat(",") {
                                break;
                            }
                        }
                    }
                    self.close()?;
                    out.push(Tok::Expr(e, sep, null));
                }
            }
        }
        if sub {
            return Err(serr("unterminated anonymous template '{'"));
        }
        if !text.is_empty() {
            out.push(Tok::Text(text));
        }
        Ok(out)
    }

    fn close(&mut self) -> Result<()> {
        self.ws();
        if self.peek() == Some(self.stop) {
            self.i += 1;
            Ok(())
        } else {
            Err(serr(format!("expected '{}' at offset {} but found {:?}", self.stop, self.i, self.peek())))
        }
    }

    fn cond(&mut self) -> Result<Cond> {
        let mut l = self.cond_and()?;
        while self.eat("||") {
            let r = self.cond_and()?;
            l = Cond::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn cond_and(&mut self) -> Result<Cond> {
        let mut l = self.cond_not()?;
        while self.eat("&&") {
            let r = self.cond_not()?;
            l = Cond::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn cond_not(&mut self) -> Result<Cond> {
        if self.eat("!") {
            return Ok(Cond::Not(Box::new(self.cond_not()?)));
        }
        self.ws();
        if self.peek() == Some('(') {
            let save = self.i;
            self.i += 1;
            if let Ok(c) = self.cond() {
                if self.eat(")") {
                    return Ok(c);
                }
            }
            self.i = save;
        }
        Ok(Cond::Expr(self.expr()?))
    }

    fn expr(&mut self) -> Result<Expr> {
        let mut items = vec![self.member()?];
        // zipped: a, b : {x, y | ...}
        loop {
            let save = self.i;
            if self.eat(",") {
                if let Ok(e) = self.member() {
                    self.ws();
                    if self.peek() == Some(',') || self.peek() == Some(':') {
                        items.push(e);
                        continue;
                    }
                }
                self.i = save;
            }
            break;
        }
        if items.len() > 1 {
            if !self.eat(":") {
                return Err(serr("expected ':' after zipped attributes"));
            }
            let t = self.template_ref()?;
            return Ok(Expr::Map(items, vec![t]));
        }
        let mut e = items.pop().unwrap();
        while self.eat(":") {
            let mut ts = vec![self.template_ref()?];
            while self.eat(",") {
                ts.push(self.template_ref()?);
            }
            e = Expr::Map(vec![e], ts);
        }
        Ok(e)
    }

    fn template_ref(&mut self) -> Result<Template> {
        self.ws();
        if self.peek() == Some('{') {
            self.i += 1;
            // optional `x, y |`
            let save = self.i;
            let mut params = Vec::new();
            loop {
                let id = self.ident();
                if id.is_empty() {
                    break;
                }
                params.push(id);
                if self.eat(",") {
                    continue;
                }
                break;
            }
            if params.is_empty() || !self.eat("|") {
                params.clear();
                self.i = save;
            } else {
                // whitespace right after `|` is not part of the template
                while self.peek() == Some(' ') {
                    self.i += 1;
                }
                if self.peek() == Some('\n') {
                    self.i += 1;
                }
            }
            let mut toks = self.tokens(true)?;
            gobble_lines(&mut toks);
            let body = build(toks)?;
            return Ok(Template { params, body });
        }
        let name = self.ident();
        Err(serr(format!("no such template: /{}", if name.is_empty() { "?" } else { &name })))
    }

    fn member(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.peek() == Some('.') {
                self.i += 1;
                if self.peek() == Some('(') {
                    self.i += 1;
                    let k = self.expr()?;
                    if !self.eat(")") {
                        return Err(serr("expected ')'"));
                    }
                    e = Expr::DynProp(Box::new(e), Box::new(k));
                } else {
                    let n = self.ident();
                    if n.is_empty() {
                        return Err(serr("expected a property name after '.'"));
                    }
                    e = Expr::Prop(Box::new(e), n);
                }
                continue;
            }
            return Ok(e);
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        self.ws();
        match self.peek() {
            Some('"') => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    let c = self.peek().ok_or_else(|| serr("unterminated string"))?;
                    self.i += 1;
                    match c {
                        '"' => break,
                        '\\' => {
                            let e = self.peek().ok_or_else(|| serr("unterminated string"))?;
                            self.i += 1;
                            s.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                o => o,
                            });
                        }
                        c => s.push(c),
                    }
                }
                Ok(Expr::Str(s))
            }
            Some('[') => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat("]") {
                    return Ok(Expr::List(items));
                }
                loop {
                    items.push(self.expr()?);
                    if self.eat(",") {
                        continue;
                    }
                    if !self.eat("]") {
                        return Err(serr("expected ']'"));
                    }
                    return Ok(Expr::List(items));
                }
            }
            Some('(') => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(")") {
                    return Err(serr("expected ')'"));
                }
                Ok(Expr::Indirect(Box::new(e)))
            }
            Some('{') => {
                let t = self.template_ref()?;
                Ok(Expr::Map(vec![Expr::Bool(true)], vec![t]))
            }
            _ => {
                let n = self.ident();
                if n.is_empty() {
                    return Err(serr(format!("unexpected {:?} in expression", self.peek())));
                }
                if self.peek() == Some('(') {
                    self.i += 1;
                    if ["first", "last", "rest", "trunc", "strip", "length", "reverse", "trim", "strlen"].contains(&n.as_str()) {
                        let a = self.expr()?;
                        if !self.eat(")") {
                            return Err(serr("expected ')'"));
                        }
                        return Ok(Expr::Func(n, Box::new(a)));
                    }
                    return Err(serr(format!("no such template: /{}", n)));
                }
                Ok(match n.as_str() {
                    "true" => Expr::Bool(true),
                    "false" => Expr::Bool(false),
                    _ => Expr::Attr(n),
                })
            }
        }
    }
}

fn build(toks: Vec<Tok>) -> Result<Vec<Node>> {
    let mut it = toks.into_iter();
    let (nodes, end) = block(&mut it)?;
    match end {
        None => Ok(nodes),
        Some(e) => Err(serr(format!("unexpected <{}>", e))),
    }
}

fn block(it: &mut std::vec::IntoIter<Tok>) -> Result<(Vec<Node>, Option<&'static str>)> {
    let mut nodes = Vec::new();
    while let Some(t) = it.next() {
        match t {
            Tok::Text(s) => nodes.push(Node::Text(s)),
            Tok::Comment => {}
            Tok::Expr(e, s, n) => nodes.push(Node::Expr(e, s, n)),
            Tok::Else => return Ok((nodes, Some("else"))),
            Tok::EndIf => return Ok((nodes, Some("endif"))),
            Tok::ElseIf(c) => {
                PENDING.with(|p| *p.borrow_mut() = Some(c));
                return Ok((nodes, Some("elseif")));
            }
            Tok::If(c) => {
                let mut branches = Vec::new();
                let mut cond = c;
                let mut otherwise = None;
                loop {
                    let (body, end) = block(it)?;
                    branches.push((cond, body));
                    match end {
                        Some("endif") => break,
                        Some("elseif") => cond = PENDING.with(|p| p.borrow_mut().take()).unwrap(),
                        Some("else") => {
                            let (eb, e2) = block(it)?;
                            if e2 != Some("endif") {
                                return Err(serr("missing <endif>"));
                            }
                            otherwise = Some(eb);
                            break;
                        }
                        _ => return Err(serr("missing <endif>")),
                    }
                }
                nodes.push(Node::If(branches, otherwise));
            }
        }
    }
    Ok((nodes, None))
}

thread_local! {
    static PENDING: std::cell::RefCell<Option<Cond>> = const { std::cell::RefCell::new(None) };
}

struct Ctx {
    scopes: Vec<Vec<(String, Value)>>,
    root: Value,
}

fn items_of(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a.clone()),
        Value::Object(o) => Some(o.keys().map(|k| Value::String(k.clone())).collect()),
        _ => None,
    }
}

impl Ctx {
    fn attr(&self, name: &str) -> Value {
        for s in self.scopes.iter().rev() {
            if let Some((_, v)) = s.iter().rev().find(|(k, _)| k == name) {
                return v.clone();
            }
        }
        super::template_support::lookup_key(&self.root, name).cloned().unwrap_or(Value::Null)
    }

    fn eval(&mut self, e: &Expr) -> Result<Value> {
        Ok(match e {
            Expr::Attr(n) => self.attr(n),
            Expr::Str(s) => Value::String(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::List(items) => {
                let mut out = Vec::new();
                for i in items {
                    match self.eval(i)? {
                        Value::Array(a) => out.extend(a),
                        Value::Null => {}
                        v => out.push(v),
                    }
                }
                Value::Array(out)
            }
            Expr::Prop(t, n) => {
                let tv = self.eval(t)?;
                java_property(&tv, n).unwrap_or(Value::Null)
            }
            Expr::DynProp(t, k) => {
                let tv = self.eval(t)?;
                let kv = self.eval(k)?;
                let key = self.render_value(&kv, None, None)?;
                java_property(&tv, &key).unwrap_or(Value::Null)
            }
            Expr::Indirect(x) => {
                let v = self.eval(x)?;
                Value::String(self.render_value(&v, None, None)?)
            }
            Expr::Func(f, a) => {
                let v = self.eval(a)?;
                let list = items_of(&v);
                match (f.as_str(), list) {
                    ("first", Some(l)) => l.first().cloned().unwrap_or(Value::Null),
                    ("last", Some(l)) => l.last().cloned().unwrap_or(Value::Null),
                    ("rest", Some(l)) => Value::Array(l.into_iter().skip(1).collect()),
                    ("trunc", Some(mut l)) => {
                        l.pop();
                        Value::Array(l)
                    }
                    ("strip", Some(l)) => Value::Array(l.into_iter().filter(|x| !x.is_null()).collect()),
                    ("reverse", Some(l)) => Value::Array(l.into_iter().rev().collect()),
                    ("length", Some(l)) => Value::Int(l.len() as i64),
                    ("length", None) => Value::Int(if v.is_null() { 0 } else { 1 }),
                    ("first" | "last", None) => v,
                    ("rest" | "trunc", None) => Value::Null,
                    ("strip" | "reverse", None) => v,
                    ("trim", _) => Value::String(self.render_value(&v, None, None)?.trim().to_string()),
                    ("strlen", _) => Value::Int(self.render_value(&v, None, None)?.chars().count() as i64),
                    _ => Value::Null,
                }
            }
            Expr::Map(sources, templates) => {
                if sources.len() > 1 {
                    // zipped application
                    let t = &templates[0];
                    let lists: Vec<Vec<Value>> = sources.iter().map(|s| self.eval(s).map(|v| items_of(&v).unwrap_or_else(|| if v.is_null() { vec![] } else { vec![v] }))).collect::<Result<_>>()?;
                    let n = lists.iter().map(|l| l.len()).max().unwrap_or(0);
                    let mut out = Vec::new();
                    for i in 0..n {
                        let args: Vec<Value> = lists.iter().map(|l| l.get(i).cloned().unwrap_or(Value::Null)).collect();
                        out.push(Value::String(self.apply(t, &args, i)?));
                    }
                    return Ok(Value::Array(out));
                }
                let mut v = self.eval(&sources[0])?;
                for (ti, t) in templates.iter().enumerate() {
                    let _ = ti;
                    v = match &v {
                        Value::Null => Value::Null,
                        other => match items_of(other) {
                            Some(items) => {
                                let mut out = Vec::new();
                                for (i, item) in items.into_iter().enumerate() {
                                    if item.is_null() {
                                        continue;
                                    }
                                    let t = &templates[i % templates.len()];
                                    out.push(Value::String(self.apply(t, &[item], i)?));
                                }
                                Value::Array(out)
                            }
                            None => Value::String(self.apply(t, std::slice::from_ref(other), 0)?),
                        },
                    };
                    if templates.len() > 1 {
                        // alternating templates are applied in one pass
                        break;
                    }
                }
                v
            }
        })
    }

    fn apply(&mut self, t: &Template, args: &[Value], index: usize) -> Result<String> {
        let mut scope = vec![("i".to_string(), Value::Int(index as i64 + 1)), ("i0".to_string(), Value::Int(index as i64))];
        if t.params.is_empty() {
            scope.push(("it".to_string(), args.first().cloned().unwrap_or(Value::Null)));
        } else {
            if t.params.len() != args.len() && args.len() > 1 {
                return Err(serr(format!("anonymous template has {} parameters but {} values were given", t.params.len(), args.len())));
            }
            for (k, p) in t.params.iter().enumerate() {
                scope.push((p.clone(), args.get(k).cloned().unwrap_or(Value::Null)));
            }
        }
        self.scopes.push(scope);
        let mut out = String::new();
        let r = self.render(&t.body, &mut out);
        self.scopes.pop();
        r?;
        Ok(out)
    }

    fn truth(&mut self, c: &Cond) -> Result<bool> {
        Ok(match c {
            Cond::Not(x) => !self.truth(x)?,
            Cond::And(a, b) => self.truth(a)? && self.truth(b)?,
            Cond::Or(a, b) => self.truth(a)? || self.truth(b)?,
            Cond::Expr(e) => match self.eval(e)? {
                Value::Null => false,
                Value::Bool(b) => b,
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            },
        })
    }

    fn render_value(&self, v: &Value, sep: Option<&str>, null: Option<&str>) -> Result<String> {
        Ok(match v {
            Value::Null => null.unwrap_or("").to_string(),
            Value::Array(_) | Value::Object(_) => {
                let items = items_of(v).unwrap();
                let mut parts = Vec::new();
                for x in &items {
                    if x.is_null() {
                        if let Some(n) = null {
                            parts.push(n.to_string());
                        }
                        continue;
                    }
                    parts.push(self.render_value(x, sep, null)?);
                }
                parts.join(sep.unwrap_or(""))
            }
            other => other.to_string(),
        })
    }

    fn render(&mut self, nodes: &[Node], out: &mut String) -> Result<()> {
        for n in nodes {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Expr(e, sep, null) => {
                    let v = self.eval(e)?;
                    out.push_str(&self.render_value(&v, sep.as_deref(), null.as_deref())?);
                }
                Node::If(branches, otherwise) => {
                    let mut done = false;
                    for (c, body) in branches {
                        if self.truth(c)? {
                            self.render(body, out)?;
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        if let Some(o) = otherwise {
                            self.render(o, out)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// The StringTemplate engine with configurable delimiters.
#[derive(Clone, Debug)]
pub struct StringTemplateEngine {
    pub start: char,
    pub stop: char,
}

impl Default for StringTemplateEngine {
    fn default() -> Self {
        StringTemplateEngine { start: '<', stop: '>' }
    }
}

impl TemplateEngine for StringTemplateEngine {
    fn render(&self, template: &str, context: &Value, _partials: PartialLoader<'_>) -> Result<String> {
        let chars: Vec<char> = template.chars().collect();
        let mut lx = Lexer { c: &chars, i: 0, start: self.start, stop: self.stop };
        let mut toks = lx.tokens(false)?;
        gobble_lines(&mut toks);
        let nodes = build(toks)?;
        let mut ctx = Ctx { scopes: Vec::new(), root: context.clone() };
        let mut out = String::new();
        ctx.render(&nodes, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Value {
        crate::json::parse(r#"{"body":"B","headers":{"name":"N","list":["a","b"],"empty":[],"flag":false,"users":[{"name":"x","age":1},{"name":"y","age":2}],"m":{"k":"v"}},"exchange":{"properties":{"p":1}}}"#).unwrap()
    }

    fn r(t: &str) -> String {
        StringTemplateEngine::default().render(t, &data(), &|_| None).unwrap_or_else(|e| panic!("{}: {}", t, e))
    }

    #[test]
    fn attributes() {
        assert_eq!(r("Hi <headers.name>, body=<body> <! c !>p=<exchange.properties.p> \\<x"), "Hi N, body=B p=1 <x");
        assert_eq!(r("<headers.list>|<missing>|<headers.list; separator=\", \">"), "ab||a, b");
        assert_eq!(r("<headers.list:{x | [<x>]}> <headers.list:{[<it>]}; separator=\",\"> <headers.users:{u | <u.name>=<u.age>}; separator=\" \">"), "[a][b] [a],[b] x=1 y=2");
        assert_eq!(r("<headers.list:{x | <i>.<x>}:{y | (<y>)}>"), "(1.a)(2.b)");
        assert_eq!(r("<headers.list, headers.users:{a, u | <a>-<u.name>}; separator=\";\">"), "a-x;b-y");
        assert_eq!(r("<first(headers.list)><last(headers.list)><length(headers.list)><rest(headers.list)><reverse(headers.list)><strlen(body)>"), "ab2bba1");
        assert_eq!(r("<headers.m.(\"k\")> <headers.m:{k | <k>}> <[\"x\", headers.list]>"), "v k xab");
        assert_eq!(r("a<\\n>b<\\t>c<\\ >d<\\u0041>"), "a\nb\tc dA");
        let e = StringTemplateEngine { start: '$', stop: '$' };
        assert_eq!(e.render("$body$!", &data(), &|_| None).unwrap(), "B!");
    }

    #[test]
    fn conditionals() {
        assert_eq!(r("<if(headers.name)>yes<else>no<endif> <if(headers.flag)>t<elseif(!headers.empty)>e<else>f<endif>"), "yes e");
        assert_eq!(r("<if(headers.name && !missing)>ok<endif><if(missing || headers.list)>ok2<endif>"), "okok2");
        let d = StringTemplateEngine { start: '$', stop: '$' };
        assert_eq!(d.render("<ul>\n$if(headers.name)$\n  <li>$headers.name$</li>\n$endif$\n</ul>", &data(), &|_| None).unwrap(), "<ul>\n  <li>N</li>\n</ul>");
    }

    #[test]
    fn errors() {
        let e = StringTemplateEngine::default();
        assert!(e.render("<if(x)>y", &data(), &|_| None).is_err());
        assert!(e.render("<headers.list:t()>", &data(), &|_| None).unwrap_err().message.contains("no such template"));
        assert!(e.render("<body", &data(), &|_| None).is_err());
    }
}
