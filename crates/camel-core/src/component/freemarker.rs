//! A FreeMarker Template Language (FTL 2.3) engine for the `freemarker` component.
//!
//! Interpolations `${expr}` (and legacy `#{expr}`), comments `<#-- -->`, and the directives
//! `if/elseif/else`, `list` (`as x`, `as k, v`, `<#items>`, `<#sep>`, `<#else>`, `<#break>`,
//! `<#continue>`), `assign/global/local` (also `+= -= *= /= %= ++ --` and the capture form),
//! `macro` (named/positional/default parameters, `<#nested>` with loop variables, `<#return>`),
//! user-defined directive calls `<@name a=1/>` / `<@name>body</@name>`, `function` +
//! `<#return v>`, `include`, `import ... as ns` (`<@ns.m/>`), `switch/case/default`,
//! `attempt/recover`, `compress`, `noparse`, `escape/noescape`, `outputformat`/`autoesc`
//! (HTML/XML escaping), `setting` (`number_format`, `boolean_format`), `t/lt/rt/nt`, `ftl`,
//! `flush`, `stop`; white-space stripping of lines that only hold FTL tags; the square
//! bracket syntax when the template starts with `[#ftl`.
//!
//! Expressions: string literals (with `${}` interpolation, escapes, raw `r"..."`), numbers,
//! booleans, sequences `[..]`, hashes `{..}`, ranges (`a..b`, `a..<b`, `a..!b`, `a..`,
//! `a..*n`), `a.b`, `a["b"]`, `seq[i]`, `seq[1..2]` / `str[0..<3]` slices, function calls,
//! lambdas `x -> ...`, built-ins `x?name(args)`, default `x!d` / `x!`, missing test `x??`,
//! `+ - * / %`, `== != = < <= > >= lt lte gt gte`, `&& || !`, special variables `.now`,
//! `.vars`, `.data_model`, `.version`, `.lang`, `.locale`. Missing values raise
//! `InvalidReferenceException` ("The following has evaluated to null or missing"), conditions
//! must be booleans, and booleans are not converted to strings implicitly (as in FreeMarker).
//!
//! Built-ins: strings (`upper_case lower_case cap_first uncap_first capitalize trim length
//! contains starts_with ends_with index_of last_index_of replace split html xhtml xml url
//! url_path js_string json_string j_string number boolean c left_pad right_pad
//! chop_linebreak word_list matches keep_before keep_after keep_before_last keep_after_last
//! remove_beginning remove_ending ensure_starts_with ensure_ends_with truncate substring
//! eval no_esc esc`), numbers (`c string abs round floor ceiling int is_nan is_infinite
//! lower_abc upper_abc`, `?string("0.00")`, `?string.computer|number|percent`), booleans
//! (`c string then`), sequences (`size first last join reverse sort sort_by seq_contains
//! seq_index_of seq_last_index_of chunk min max filter map take_while drop_while`), hashes
//! (`keys values`), generic (`has_content exists if_exists default is_string is_number
//! is_boolean is_sequence is_hash is_hash_ex is_enumerable is_macro is_method switch`) and
//! loop variables (`index counter has_next is_first is_last item_parity item_parity_cap
//! item_cycle is_odd_item is_even_item`). Dates/times are not supported.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::value::{ObjectMap, Value};

use super::template_support::{gobble_lines, lookup_key, GobbleToken, PartialLoader, TemplateEngine};

fn perr(m: impl Into<String>) -> CamelError {
    CamelError::new("ParseException", format!("FreeMarker template error: {}", m.into()))
}
fn terr(m: impl Into<String>) -> CamelError {
    CamelError::new("TemplateException", format!("FreeMarker template error: {}", m.into()))
}
fn missing(expr: &str) -> CamelError {
    CamelError::new(
        "InvalidReferenceException",
        format!("FreeMarker template error: The following has evaluated to null or missing: ==> {}  [Tip: If the failing expression is known to legally refer to something that's sometimes null or missing, either specify a default value like myOptionalVar!myDefault, or use <#if myOptionalVar??>when-present<#else>when-missing</#if>.]", expr),
    )
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum StrPart {
    Lit(String),
    Interp(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RangeKind {
    Incl,
    Excl,
    Unbounded,
    Len,
}

#[derive(Debug, Clone)]
enum Expr {
    Lit(Value),
    Str(Vec<StrPart>),
    Var(String),
    Special(String),
    Seq(Vec<Expr>),
    Hash(Vec<(Expr, Expr)>),
    Range(Box<Expr>, Option<Box<Expr>>, RangeKind),
    Dot(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Builtin(Box<Expr>, String, Option<Vec<Expr>>),
    Default(Box<Expr>, Option<Box<Expr>>),
    Exists(Box<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Lambda(String, Box<Expr>),
}

impl Expr {
    fn src(&self) -> String {
        match self {
            Expr::Var(n) => n.clone(),
            Expr::Special(n) => format!(".{}", n),
            Expr::Dot(t, n) => format!("{}.{}", t.src(), n),
            Expr::Index(t, i) => format!("{}[{}]", t.src(), i.src()),
            Expr::Builtin(t, n, _) => format!("{}?{}", t.src(), n),
            Expr::Call(t, _) => format!("{}(...)", t.src()),
            Expr::Lit(v) => v.to_json(),
            _ => "expression".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scope {
    Assign,
    Global,
    Local,
}

#[derive(Debug, Clone)]
struct MacroDef {
    params: Vec<(String, Option<Expr>)>,
    catch_all: Option<String>,
    body: Arc<Vec<Node>>,
    is_function: bool,
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Interp(Expr),
    If(Vec<(Expr, Vec<Node>)>, Option<Vec<Node>>),
    List { seq: Expr, vars: Vec<String>, body: Vec<Node>, otherwise: Option<Vec<Node>> },
    Items { vars: Vec<String>, body: Vec<Node> },
    Sep(Vec<Node>),
    Break,
    Continue,
    Return(Option<Expr>),
    Stop(Option<Expr>),
    Assign(Scope, Vec<(String, String, Option<Expr>)>),
    Capture(Scope, String, Vec<Node>),
    Call { name: String, named: Vec<(String, Expr)>, positional: Vec<Expr>, body: Option<Arc<Vec<Node>>>, loop_vars: Vec<String> },
    Nested(Vec<Expr>),
    Include(Expr),
    Import(Expr, String),
    Switch(Expr, Vec<(Option<Expr>, Vec<Node>)>),
    Attempt(Vec<Node>, Vec<Node>),
    Compress(Vec<Node>),
    Escape(String, Expr, Vec<Node>),
    NoEscape(Vec<Node>),
    OutputFormat(Option<String>, Vec<Node>),
    Setting(String, Expr),
}

// ---------------------------------------------------------------------------
// Expression parser
// ---------------------------------------------------------------------------

struct EP<'a> {
    c: &'a [char],
    i: usize,
}

fn id_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$' || c == '@'
}
fn id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '@' || c == '-' || c == ':'
}

impl<'a> EP<'a> {
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
    fn eat(&mut self, s: &str) -> bool {
        self.ws();
        if self.starts(s) {
            self.i += s.chars().count();
            true
        } else {
            false
        }
    }
    fn kw(&mut self, w: &str) -> bool {
        self.ws();
        if self.starts(w) && !self.at(w.chars().count()).map(id_char).unwrap_or(false) {
            self.i += w.chars().count();
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> String {
        let st = self.i;
        while self.peek().map(|c| c.is_alphanumeric() || c == '_' || c == '$').unwrap_or(false) {
            self.i += 1;
        }
        self.c[st..self.i].iter().collect()
    }
    fn done(&mut self) -> bool {
        self.ws();
        self.i >= self.c.len()
    }

    fn expr(&mut self) -> Result<Expr> {
        self.ws();
        // lambda: `x -> expr`
        let save = self.i;
        if self.peek().map(id_start).unwrap_or(false) {
            let name = self.ident();
            if self.eat("->") {
                let body = self.expr()?;
                return Ok(Expr::Lambda(name, Box::new(body)));
            }
        }
        self.i = save;
        self.or()
    }
    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.eat("||") {
            let r = self.and()?;
            l = Expr::Bin("||", Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.eq()?;
        loop {
            self.ws();
            if self.starts("&&") {
                self.i += 2;
            } else if self.starts("&amp;&amp;") {
                self.i += 10;
            } else {
                return Ok(l);
            }
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
            } else if self.starts("=") && !self.starts("=>") {
                self.i += 1;
                "=="
            } else {
                return Ok(l);
            };
            let r = self.rel()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn rel(&mut self) -> Result<Expr> {
        let l = self.range()?;
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
        } else if self.kw("lte") {
            "<="
        } else if self.kw("gte") {
            ">="
        } else if self.kw("lt") {
            "<"
        } else if self.kw("gt") {
            ">"
        } else {
            return Ok(l);
        };
        let r = self.range()?;
        Ok(Expr::Bin(op, Box::new(l), Box::new(r)))
    }
    fn range(&mut self) -> Result<Expr> {
        let l = self.add()?;
        self.ws();
        if self.starts("..") {
            self.i += 2;
            let kind = if self.peek() == Some('<') || self.peek() == Some('!') {
                self.i += 1;
                RangeKind::Excl
            } else if self.peek() == Some('*') {
                self.i += 1;
                RangeKind::Len
            } else {
                RangeKind::Incl
            };
            self.ws();
            let unbounded = kind == RangeKind::Incl && matches!(self.peek(), None | Some(']') | Some(')') | Some(',') | Some('>') | Some('}'));
            if unbounded {
                return Ok(Expr::Range(Box::new(l), None, RangeKind::Unbounded));
            }
            let r = self.add()?;
            return Ok(Expr::Range(Box::new(l), Some(Box::new(r)), kind));
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<Expr> {
        let mut l = self.mul()?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some('+') => "+",
                Some('-') if self.at(1) != Some('>') => "-",
                _ => return Ok(l),
            };
            self.i += 1;
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
                Some('/') if self.at(1) != Some('>') => "/",
                Some('%') => "%",
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn unary(&mut self) -> Result<Expr> {
        self.ws();
        match self.peek() {
            Some('!') => {
                self.i += 1;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Some('-') => {
                self.i += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some('+') => {
                self.i += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }
    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some('.') if self.at(1) != Some('.') => {
                    self.i += 1;
                    if self.peek() == Some('*') {
                        self.i += 1;
                        e = Expr::Dot(Box::new(e), "*".into());
                        continue;
                    }
                    let n = self.ident();
                    if n.is_empty() {
                        return Err(perr("expected a name after '.'"));
                    }
                    e = Expr::Dot(Box::new(e), n);
                }
                Some('[') => {
                    self.i += 1;
                    let idx = self.expr()?;
                    if !self.eat("]") {
                        return Err(perr("expected ']'"));
                    }
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                Some('(') => {
                    self.i += 1;
                    let args = self.list(')')?;
                    e = Expr::Call(Box::new(e), args);
                }
                Some('?') if self.at(1) == Some('?') => {
                    self.i += 2;
                    e = Expr::Exists(Box::new(e));
                }
                Some('?') => {
                    self.i += 1;
                    let mut n = self.ident();
                    // `?string.computer` style
                    while self.peek() == Some('.') && self.at(1).map(id_start).unwrap_or(false) && n == "string" {
                        self.i += 1;
                        n = format!("{}.{}", n, self.ident());
                    }
                    if n.is_empty() {
                        return Err(perr("expected a built-in name after '?'"));
                    }
                    let args = if self.peek() == Some('(') {
                        self.i += 1;
                        Some(self.list(')')?)
                    } else {
                        None
                    };
                    e = Expr::Builtin(Box::new(e), n, args);
                }
                Some('!') if self.at(1) != Some('=') => {
                    self.i += 1;
                    // default value operand (optional)
                    let save = self.i;
                    self.ws();
                    let starts_operand = match self.peek() {
                        None => false,
                        Some(c) => c == '"' || c == '\'' || c == '(' || c == '[' || c == '{' || c.is_ascii_digit() || id_start(c) && !self.starts("as ") || c == '-',
                    };
                    let stop_words = ["as", "gt", "lt", "gte", "lte", "using"];
                    let next_word: String = self.c[self.i..].iter().take_while(|c| c.is_alphanumeric()).collect();
                    if starts_operand && !stop_words.contains(&next_word.as_str()) {
                        let d = self.postfix_noop()?;
                        e = Expr::Default(Box::new(e), Some(Box::new(d)));
                    } else {
                        self.i = save;
                        e = Expr::Default(Box::new(e), None);
                    }
                }
                _ => return Ok(e),
            }
        }
    }
    /// Operand of `!` (a primary with suffixes, but no further `!` default).
    fn postfix_noop(&mut self) -> Result<Expr> {
        self.ws();
        if self.peek() == Some('-') {
            self.i += 1;
            return Ok(Expr::Neg(Box::new(self.postfix_noop()?)));
        }
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some('.') if self.at(1) != Some('.') => {
                    self.i += 1;
                    let n = self.ident();
                    e = Expr::Dot(Box::new(e), n);
                }
                Some('[') => {
                    self.i += 1;
                    let idx = self.expr()?;
                    if !self.eat("]") {
                        return Err(perr("expected ']'"));
                    }
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                Some('?') if self.at(1) != Some('?') => {
                    self.i += 1;
                    let n = self.ident();
                    let args = if self.peek() == Some('(') {
                        self.i += 1;
                        Some(self.list(')')?)
                    } else {
                        None
                    };
                    e = Expr::Builtin(Box::new(e), n, args);
                }
                _ => return Ok(e),
            }
        }
    }
    fn list(&mut self, close: char) -> Result<Vec<Expr>> {
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(close) {
            self.i += 1;
            return Ok(out);
        }
        loop {
            out.push(self.expr()?);
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(c) if c == close => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return Err(perr(format!("expected ',' or '{}'", close))),
            }
        }
    }
    fn string(&mut self) -> Result<Expr> {
        let raw = self.peek() == Some('r');
        if raw {
            self.i += 1;
        }
        let q = self.peek().unwrap();
        self.i += 1;
        let mut parts = Vec::new();
        let mut s = String::new();
        loop {
            let c = self.peek().ok_or_else(|| perr("unterminated string literal"))?;
            self.i += 1;
            if c == q {
                break;
            }
            if raw {
                s.push(c);
                continue;
            }
            if c == '\\' {
                let e = self.peek().ok_or_else(|| perr("unterminated string literal"))?;
                self.i += 1;
                s.push(match e {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'f' => '\u{c}',
                    'b' => '\u{8}',
                    'l' => '<',
                    'g' => '>',
                    'a' => '&',
                    'x' => {
                        let st = self.i;
                        while self.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) && self.i - st < 4 {
                            self.i += 1;
                        }
                        let h: String = self.c[st..self.i].iter().collect();
                        char::from_u32(u32::from_str_radix(&h, 16).map_err(|_| perr("bad \\x escape"))?).unwrap_or('\u{FFFD}')
                    }
                    o => o,
                });
                continue;
            }
            if (c == '$' || c == '#') && self.peek() == Some('{') {
                self.i += 1;
                let st = self.i;
                let end = find_close(self.c, st, '}').ok_or_else(|| perr("unclosed ${ in string literal"))?;
                let inner = &self.c[st..end];
                self.i = end + 1;
                if !s.is_empty() {
                    parts.push(StrPart::Lit(std::mem::take(&mut s)));
                }
                parts.push(StrPart::Interp(parse_expr(inner)?));
                continue;
            }
            s.push(c);
        }
        if parts.is_empty() {
            return Ok(Expr::Lit(Value::String(s)));
        }
        if !s.is_empty() {
            parts.push(StrPart::Lit(s));
        }
        Ok(Expr::Str(parts))
    }
    fn primary(&mut self) -> Result<Expr> {
        self.ws();
        let c = self.peek().ok_or_else(|| perr("unexpected end of expression"))?;
        match c {
            '"' | '\'' => self.string(),
            'r' if matches!(self.at(1), Some('"') | Some('\'')) => self.string(),
            '(' => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(")") {
                    return Err(perr("expected ')'"));
                }
                Ok(e)
            }
            '[' => {
                self.i += 1;
                Ok(Expr::Seq(self.list(']')?))
            }
            '{' => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    self.ws();
                    if self.peek() == Some('}') {
                        self.i += 1;
                        return Ok(Expr::Hash(entries));
                    }
                    let k = self.expr()?;
                    if !self.eat(":") {
                        return Err(perr("expected ':' in hash literal"));
                    }
                    let v = self.expr()?;
                    entries.push((k, v));
                    self.ws();
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            '.' => {
                self.i += 1;
                let n = self.ident();
                Ok(Expr::Special(n))
            }
            c if c.is_ascii_digit() => {
                let st = self.i;
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
                Ok(Expr::Lit(if float { Value::Float(t.parse().unwrap()) } else { t.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::Float(t.parse().unwrap())) }))
            }
            c if id_start(c) => {
                let n = self.ident();
                Ok(match n.as_str() {
                    "true" => Expr::Lit(Value::Bool(true)),
                    "false" => Expr::Lit(Value::Bool(false)),
                    _ => Expr::Var(n),
                })
            }
            other => Err(perr(format!("unexpected '{}' in expression", other))),
        }
    }
}

/// Index of the `close` char matching an opening at `start - 1`, skipping strings and
/// nested brackets.
fn find_close(c: &[char], start: usize, close: char) -> Option<usize> {
    let open = match close {
        '}' => '{',
        ')' => '(',
        ']' => '[',
        _ => '\0',
    };
    let mut depth = 0;
    let mut i = start;
    while i < c.len() {
        let ch = c[i];
        if ch == '"' || ch == '\'' {
            let q = ch;
            i += 1;
            while i < c.len() && c[i] != q {
                if c[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn parse_expr(c: &[char]) -> Result<Expr> {
    let mut p = EP { c, i: 0 };
    let e = p.expr()?;
    if !p.done() {
        return Err(perr(format!("unexpected '{}' in expression '{}'", p.c[p.i..].iter().collect::<String>(), c.iter().collect::<String>())));
    }
    Ok(e)
}

// ---------------------------------------------------------------------------
// Template lexer
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Tok {
    Text(String),
    Interp(Expr),
    Open(String, String, bool),
    Close(String),
    Call(String, String, bool),
    CallEnd,
    Comment,
    Trim(char),
}

impl GobbleToken for Tok {
    fn text_mut(&mut self) -> Option<&mut String> {
        match self {
            Tok::Text(t) => Some(t),
            _ => None,
        }
    }
    fn gobbles(&self) -> bool {
        matches!(self, Tok::Open(..) | Tok::Close(_) | Tok::Call(..) | Tok::CallEnd | Tok::Comment | Tok::Trim(_))
    }
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let c: Vec<char> = src.chars().collect();
    let square = src.trim_start().starts_with("[#ftl");
    let (lt, gt) = if square { ('[', ']') } else { ('<', '>') };
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    let starts = |i: usize, s: &str| s.chars().enumerate().all(|(k, ch)| c.get(i + k) == Some(&ch));
    while i < c.len() {
        let ch = c[i];
        if (ch == '$' || ch == '#') && c.get(i + 1) == Some(&'{') {
            let end = find_close(&c, i + 2, '}').ok_or_else(|| perr(format!("unclosed {}{{ at offset {}", ch, i)))?;
            let e = parse_expr(&c[i + 2..end])?;
            if !text.is_empty() {
                out.push(Tok::Text(std::mem::take(&mut text)));
            }
            out.push(Tok::Interp(e));
            i = end + 1;
            continue;
        }
        if ch == lt {
            let comment = format!("{}#--", lt);
            if starts(i, &comment) {
                let close = format!("--{}", gt);
                let mut j = i + 4;
                while j < c.len() && !starts(j, &close) {
                    j += 1;
                }
                if j >= c.len() {
                    return Err(perr("unclosed comment"));
                }
                if !text.is_empty() {
                    out.push(Tok::Text(std::mem::take(&mut text)));
                }
                out.push(Tok::Comment);
                i = j + 3;
                continue;
            }
            let (closing, kind) = match (c.get(i + 1), c.get(i + 2)) {
                (Some('#'), _) => (false, '#'),
                (Some('@'), _) => (false, '@'),
                (Some('/'), Some('#')) => (true, '#'),
                (Some('/'), Some('@')) => (true, '@'),
                _ => {
                    text.push(ch);
                    i += 1;
                    continue;
                }
            };
            let name_start = i + if closing { 3 } else { 2 };
            if kind == '#' && !c.get(name_start).map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                text.push(ch);
                i += 1;
                continue;
            }
            let mut j = name_start;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '.' || c[j] == ':' || c[j] == '-') {
                j += 1;
            }
            let name: String = c[name_start..j].iter().collect();
            // find the closing `>` outside parentheses and strings
            let mut k = j;
            let mut depth = 0i32;
            while k < c.len() {
                let x = c[k];
                if x == '"' || x == '\'' {
                    let q = x;
                    k += 1;
                    while k < c.len() && c[k] != q {
                        if c[k] == '\\' {
                            k += 1;
                        }
                        k += 1;
                    }
                } else if x == '(' || x == '[' || x == '{' {
                    depth += 1;
                } else if x == ')' || (x == ']' && !(square && depth == 0)) || x == '}' {
                    depth -= 1;
                } else if x == gt && depth <= 0 {
                    break;
                }
                k += 1;
            }
            if k >= c.len() {
                return Err(perr(format!("unclosed tag {}{}{} at offset {}", lt, if closing { "/" } else { "" }, name, i)));
            }
            let mut args: String = c[j..k].iter().collect();
            let self_close = args.trim_end().ends_with('/');
            if self_close {
                let t = args.trim_end();
                args = t[..t.len() - 1].to_string();
            }
            if !text.is_empty() {
                out.push(Tok::Text(std::mem::take(&mut text)));
            }
            i = k + 1;
            match (kind, closing) {
                ('#', false) if name == "noparse" => {
                    let close = format!("{}/#noparse{}", lt, gt);
                    let mut m = i;
                    while m < c.len() && !starts(m, &close) {
                        m += 1;
                    }
                    let raw: String = c[i..m.min(c.len())].iter().collect();
                    out.push(Tok::Comment);
                    out.push(Tok::Text(raw));
                    out.push(Tok::Comment);
                    i = (m + close.chars().count()).min(c.len());
                }
                ('#', false) if matches!(name.as_str(), "t" | "lt" | "rt" | "nt") => {
                    out.push(Tok::Trim(match name.as_str() {
                        "t" => 't',
                        "lt" => 'l',
                        "rt" => 'r',
                        _ => 'n',
                    }));
                }
                ('#', false) => out.push(Tok::Open(name, args, self_close)),
                ('#', true) => out.push(Tok::Close(name)),
                ('@', false) => out.push(Tok::Call(name, args, self_close)),
                _ => out.push(Tok::CallEnd),
            }
            continue;
        }
        text.push(ch);
        i += 1;
    }
    if !text.is_empty() {
        out.push(Tok::Text(text));
    }
    Ok(out)
}

/// Apply `<#t>`, `<#lt>`, `<#rt>` to the neighbouring text on the same line.
fn apply_trims(toks: &mut [Tok]) {
    for i in 0..toks.len() {
        let mode = match toks[i] {
            Tok::Trim(m) => m,
            _ => continue,
        };
        if mode == 'n' {
            continue;
        }
        if mode == 't' || mode == 'l' {
            // leading white-space of the line
            for j in (0..i).rev() {
                if let Tok::Text(t) = &mut toks[j] {
                    let ls = t.rfind('\n').map(|p| p + 1).unwrap_or(0);
                    let seg = &t[ls..];
                    if seg.chars().all(char::is_whitespace) {
                        t.truncate(ls);
                    }
                    break;
                }
            }
        }
        if mode == 't' || mode == 'r' {
            for tok in toks.iter_mut().skip(i + 1) {
                if let Tok::Text(t) = tok {
                    if let Some(p) = t.find('\n') {
                        if t[..p].chars().all(char::is_whitespace) {
                            *t = t[p + 1..].to_string();
                        }
                    }
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    i: usize,
    macros: Vec<(String, MacroDef)>,
}

enum Stop {
    Eof,
    Close(String),
    Else,
    ElseIf(Expr),
    Sep,
    Items(Vec<String>),
    Recover,
    Case(Option<Expr>),
    CallEnd,
}

fn vars_list(s: &str) -> Vec<String> {
    s.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect()
}

/// Split `a=1 b="x y" c` style directive parameters.
fn named_params(s: &str) -> Result<Vec<(String, String)>> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && (c[i].is_whitespace() || c[i] == ',') {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let st = i;
        while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.') {
            i += 1;
        }
        let name: String = c[st..i].iter().collect();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if name.is_empty() {
            return Err(perr(format!("bad parameter list '{}'", s)));
        }
        let mut op = String::new();
        for o in ["+=", "-=", "*=", "/=", "%=", "++", "--", "="] {
            if c[i..].iter().take(o.len()).collect::<String>() == o {
                op = o.to_string();
                i += o.len();
                break;
            }
        }
        if op.is_empty() || op == "++" || op == "--" {
            out.push((name, op));
            continue;
        }
        // value: up to the next top-level whitespace that is followed by `name=` or the end
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let vst = i;
        let mut depth = 0i32;
        while i < c.len() {
            let x = c[i];
            if x == '"' || x == '\'' {
                let q = x;
                i += 1;
                while i < c.len() && c[i] != q {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            } else if x == '(' || x == '[' || x == '{' {
                depth += 1;
            } else if x == ')' || x == ']' || x == '}' {
                depth -= 1;
            } else if depth == 0 && (x.is_whitespace() || x == ',') {
                // does a new `name=` start after the white-space?
                let mut k = i;
                while k < c.len() && (c[k].is_whitespace() || c[k] == ',') {
                    k += 1;
                }
                let ns = k;
                while k < c.len() && (c[k].is_alphanumeric() || c[k] == '_') {
                    k += 1;
                }
                while k < c.len() && c[k].is_whitespace() {
                    k += 1;
                }
                let next_is_param = k > ns && (k >= c.len() || c[k] == '=' && c.get(k + 1) != Some(&'=') || ["+=", "-=", "*=", "/=", "%=", "++", "--"].iter().any(|o| c[k..].iter().take(2).collect::<String>() == *o));
                if next_is_param || k >= c.len() {
                    break;
                }
            }
            i += 1;
        }
        out.push((name, format!("{}\u{0}{}", op, c[vst..i].iter().collect::<String>())));
    }
    Ok(out)
}

impl Parser {
    fn block(&mut self) -> Result<(Vec<Node>, Stop)> {
        let mut nodes = Vec::new();
        while self.i < self.toks.len() {
            let t = std::mem::replace(&mut self.toks[self.i], Tok::Comment);
            self.i += 1;
            match t {
                Tok::Text(s) => {
                    if !s.is_empty() {
                        nodes.push(Node::Text(s))
                    }
                }
                Tok::Comment | Tok::Trim(_) => {}
                Tok::Interp(e) => nodes.push(Node::Interp(e)),
                Tok::Close(n) => return Ok((nodes, Stop::Close(n))),
                Tok::CallEnd => return Ok((nodes, Stop::CallEnd)),
                Tok::Call(name, args, self_close) => nodes.push(self.call(name, &args, self_close)?),
                Tok::Open(name, args, self_close) => {
                    let a: Vec<char> = args.chars().collect();
                    match name.as_str() {
                        "else" => return Ok((nodes, Stop::Else)),
                        "elseif" | "elif" => return Ok((nodes, Stop::ElseIf(parse_expr(&a)?))),
                        "sep" => {
                            if self_close {
                                return Ok((nodes, Stop::Sep));
                            }
                            let (body, stop) = self.block()?;
                            nodes.push(Node::Sep(body));
                            match stop {
                                Stop::Close(n) if n == "sep" => {}
                                other => return Ok((nodes, other)),
                            }
                        }
                        "items" => {
                            let v = args.trim().strip_prefix("as").ok_or_else(|| perr("#items requires 'as'"))?;
                            return Ok((nodes, Stop::Items(vars_list(v))));
                        }
                        "recover" => return Ok((nodes, Stop::Recover)),
                        "case" => return Ok((nodes, Stop::Case(Some(parse_expr(&a)?)))),
                        "default" => return Ok((nodes, Stop::Case(None))),
                        "if" => {
                            let mut branches = Vec::new();
                            let mut cond = parse_expr(&a)?;
                            let mut otherwise = None;
                            loop {
                                let (body, stop) = self.block()?;
                                match stop {
                                    Stop::Close(n) if n == "if" => {
                                        branches.push((cond, body));
                                        break;
                                    }
                                    Stop::ElseIf(c) => {
                                        branches.push((cond, body));
                                        cond = c;
                                    }
                                    Stop::Else => {
                                        branches.push((cond, body));
                                        let (eb, s2) = self.block()?;
                                        if !matches!(s2, Stop::Close(ref n) if n == "if") {
                                            return Err(perr("<#else> without </#if>"));
                                        }
                                        otherwise = Some(eb);
                                        break;
                                    }
                                    _ => return Err(perr("<#if> without </#if>")),
                                }
                            }
                            nodes.push(Node::If(branches, otherwise));
                        }
                        "list" | "foreach" => {
                            let (seq, vars) = if name == "foreach" {
                                let (v, s) = args.split_once(" in ").ok_or_else(|| perr("#foreach requires 'in'"))?;
                                (parse_expr(&s.chars().collect::<Vec<_>>())?, vec![v.trim().to_string()])
                            } else {
                                match split_as(&args) {
                                    Some((s, v)) => (parse_expr(&s.chars().collect::<Vec<_>>())?, vars_list(&v)),
                                    None => (parse_expr(&a)?, Vec::new()),
                                }
                            };
                            let (mut body, mut stop) = self.block()?;
                            if let Stop::Items(iv) = stop {
                                let (ib, s2) = self.block()?;
                                if !matches!(s2, Stop::Close(ref n) if n == "items") {
                                    return Err(perr("<#items> without </#items>"));
                                }
                                body.push(Node::Items { vars: iv, body: ib });
                                let (rest, s3) = self.block()?;
                                body.extend(rest);
                                stop = s3;
                            }
                            if let Stop::Sep = stop {
                                let (sb, s2) = self.block()?;
                                body.push(Node::Sep(sb));
                                stop = s2;
                            }
                            let otherwise = match stop {
                                Stop::Close(n) if n == "list" || n == "foreach" => None,
                                Stop::Else => {
                                    let (eb, s2) = self.block()?;
                                    if !matches!(s2, Stop::Close(ref n) if n == "list") {
                                        return Err(perr("<#list> without </#list>"));
                                    }
                                    Some(eb)
                                }
                                _ => return Err(perr("<#list> without </#list>")),
                            };
                            nodes.push(Node::List { seq, vars, body, otherwise });
                        }
                        "break" => nodes.push(Node::Break),
                        "continue" => nodes.push(Node::Continue),
                        "return" => nodes.push(Node::Return(if args.trim().is_empty() { None } else { Some(parse_expr(&a)?) })),
                        "stop" => nodes.push(Node::Stop(if args.trim().is_empty() { None } else { Some(parse_expr(&a)?) })),
                        "flush" | "ftl" => {}
                        "assign" | "global" | "local" => {
                            let scope = match name.as_str() {
                                "assign" => Scope::Assign,
                                "global" => Scope::Global,
                                _ => Scope::Local,
                            };
                            let params = named_params(args.trim())?;
                            if params.len() == 1 && params[0].1.is_empty() && !self_close {
                                let (body, stop) = self.block()?;
                                if !matches!(stop, Stop::Close(ref n) if *n == name) {
                                    return Err(perr(format!("<#{}> without </#{}>", name, name)));
                                }
                                nodes.push(Node::Capture(scope, params[0].0.clone(), body));
                            } else {
                                let mut ops = Vec::new();
                                for (n, v) in params {
                                    let (op, val) = v.split_once('\u{0}').map(|(o, x)| (o.to_string(), Some(x.to_string()))).unwrap_or((v.clone(), None));
                                    let e = match val {
                                        Some(x) => Some(parse_expr(&x.chars().collect::<Vec<_>>())?),
                                        None => None,
                                    };
                                    ops.push((n, op, e));
                                }
                                nodes.push(Node::Assign(scope, ops));
                            }
                        }
                        "macro" | "function" => {
                            let mut p = EP { c: &a, i: 0 };
                            p.ws();
                            let mname = if p.peek() == Some('"') {
                                match p.string()? {
                                    Expr::Lit(v) => v.to_string(),
                                    _ => return Err(perr("bad macro name")),
                                }
                            } else {
                                p.ident()
                            };
                            let mut params = Vec::new();
                            let mut catch_all = None;
                            loop {
                                p.ws();
                                if p.done() {
                                    break;
                                }
                                if p.peek() == Some('(') || p.peek() == Some(')') || p.peek() == Some(',') {
                                    p.i += 1;
                                    continue;
                                }
                                let pn = p.ident();
                                if pn.is_empty() {
                                    return Err(perr(format!("bad parameter list in <#{} {}>", name, args)));
                                }
                                if p.starts("...") {
                                    p.i += 3;
                                    catch_all = Some(pn);
                                    continue;
                                }
                                p.ws();
                                if p.peek() == Some('=') {
                                    p.i += 1;
                                    let d = p.postfix()?;
                                    params.push((pn, Some(d)));
                                } else {
                                    params.push((pn, None));
                                }
                            }
                            let (body, stop) = self.block()?;
                            if !matches!(stop, Stop::Close(ref n) if *n == name) {
                                return Err(perr(format!("<#{}> without </#{}>", name, name)));
                            }
                            self.macros.push((mname, MacroDef { params, catch_all, body: Arc::new(body), is_function: name == "function" }));
                        }
                        "nested" => {
                            let args = if args.trim().is_empty() { Vec::new() } else { EP { c: &a, i: 0 }.list_noclose()? };
                            nodes.push(Node::Nested(args));
                        }
                        "include" => {
                            let first = args.split_whitespace().next().unwrap_or("").to_string();
                            nodes.push(Node::Include(parse_expr(&first.chars().collect::<Vec<_>>())?));
                        }
                        "import" => {
                            let (p, ns) = split_as(&args).ok_or_else(|| perr("#import requires 'as'"))?;
                            nodes.push(Node::Import(parse_expr(&p.chars().collect::<Vec<_>>())?, ns.trim().to_string()));
                        }
                        "switch" => {
                            let subject = parse_expr(&a)?;
                            let (_, mut stop) = self.block()?;
                            let mut cases = Vec::new();
                            loop {
                                match stop {
                                    Stop::Case(c) => {
                                        let (body, s2) = self.block()?;
                                        cases.push((c, body));
                                        stop = s2;
                                    }
                                    Stop::Close(n) if n == "switch" => break,
                                    _ => return Err(perr("<#switch> without </#switch>")),
                                }
                            }
                            nodes.push(Node::Switch(subject, cases));
                        }
                        "attempt" => {
                            let (body, stop) = self.block()?;
                            if !matches!(stop, Stop::Recover) {
                                return Err(perr("<#attempt> without <#recover>"));
                            }
                            let (rec, s2) = self.block()?;
                            if !matches!(s2, Stop::Close(ref n) if n == "attempt" || n == "recover") {
                                return Err(perr("<#attempt> without </#attempt>"));
                            }
                            nodes.push(Node::Attempt(body, rec));
                        }
                        "compress" | "escape" | "noescape" | "outputformat" | "autoesc" | "noautoesc" => {
                            let (body, stop) = self.block()?;
                            if !matches!(stop, Stop::Close(ref n) if *n == name) {
                                return Err(perr(format!("<#{}> without </#{}>", name, name)));
                            }
                            nodes.push(match name.as_str() {
                                "compress" => Node::Compress(body),
                                "escape" => {
                                    let (v, ex) = args.split_once(" as ").ok_or_else(|| perr("#escape requires 'as'"))?;
                                    Node::Escape(v.trim().to_string(), parse_expr(&ex.chars().collect::<Vec<_>>())?, body)
                                }
                                "noescape" => Node::NoEscape(body),
                                "outputformat" => {
                                    let f = match parse_expr(&a)? {
                                        Expr::Lit(v) => v.to_string(),
                                        _ => String::new(),
                                    };
                                    Node::OutputFormat(Some(f), body)
                                }
                                "autoesc" => Node::OutputFormat(Some("HTML".into()), body),
                                _ => Node::OutputFormat(None, body),
                            });
                        }
                        "setting" => {
                            let params = named_params(args.trim())?;
                            for (n, v) in params {
                                let val = v.split_once('\u{0}').map(|(_, x)| x.to_string()).unwrap_or_default();
                                nodes.push(Node::Setting(n, parse_expr(&val.chars().collect::<Vec<_>>())?));
                            }
                        }
                        "visit" | "recurse" | "fallback" => return Err(terr(format!("<#{}> (node processing) is not supported", name))),
                        other => return Err(perr(format!("Unknown directive: #{}", other))),
                    }
                }
            }
        }
        Ok((nodes, Stop::Eof))
    }

    fn call(&mut self, name: String, args: &str, self_close: bool) -> Result<Node> {
        let (args, loop_vars) = match args.split_once(';') {
            Some((a, v)) => (a.to_string(), vars_list(v)),
            None => (args.to_string(), Vec::new()),
        };
        let trimmed = args.trim();
        let mut named = Vec::new();
        let mut positional = Vec::new();
        let looks_named = {
            let mut it = trimmed.chars().skip_while(|c| c.is_alphanumeric() || *c == '_');
            let rest: String = it.by_ref().collect();
            !trimmed.is_empty() && rest.trim_start().starts_with('=') && !rest.trim_start().starts_with("==")
        };
        if looks_named {
            for (n, v) in named_params(trimmed)? {
                let val = v.split_once('\u{0}').map(|(_, x)| x.to_string()).ok_or_else(|| perr(format!("<@{}>: parameter {} needs a value", name, n)))?;
                named.push((n, parse_expr(&val.chars().collect::<Vec<_>>())?));
            }
        } else if !trimmed.is_empty() {
            let a: Vec<char> = trimmed.chars().collect();
            positional = EP { c: &a, i: 0 }.list_noclose()?;
        }
        let body = if self_close {
            None
        } else {
            let (b, stop) = self.block()?;
            match stop {
                Stop::CallEnd => {}
                _ => return Err(perr(format!("<@{}> without </@{}>", name, name))),
            }
            Some(Arc::new(b))
        };
        Ok(Node::Call { name, named, positional, body, loop_vars })
    }
}

impl<'a> EP<'a> {
    /// A comma (or space) separated expression list without brackets.
    fn list_noclose(&mut self) -> Result<Vec<Expr>> {
        let mut out = Vec::new();
        loop {
            self.ws();
            if self.done() {
                return Ok(out);
            }
            out.push(self.expr()?);
            self.ws();
            if self.peek() == Some(',') {
                self.i += 1;
            }
        }
    }
}

fn split_as(s: &str) -> Option<(String, String)> {
    // the last top-level " as " outside strings
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut found = None;
    let mut depth = 0;
    while i < c.len() {
        let x = c[i];
        if x == '"' || x == '\'' {
            let q = x;
            i += 1;
            while i < c.len() && c[i] != q {
                i += 1;
            }
        } else if x == '(' || x == '[' || x == '{' {
            depth += 1;
        } else if x == ')' || x == ']' || x == '}' {
            depth -= 1;
        } else if depth == 0 && x.is_whitespace() && c.get(i + 1) == Some(&'a') && c.get(i + 2) == Some(&'s') && c.get(i + 3).map(|c| c.is_whitespace()).unwrap_or(false) {
            found = Some(i);
        }
        i += 1;
    }
    found.map(|p| (c[..p].iter().collect(), c[p + 4..].iter().collect()))
}

fn compile(src: &str) -> Result<(Vec<Node>, Vec<(String, MacroDef)>)> {
    let mut toks = lex(src)?;
    apply_trims(&mut toks);
    gobble_lines(&mut toks);
    let mut p = Parser { toks, i: 0, macros: Vec::new() };
    let (nodes, stop) = p.block()?;
    match stop {
        Stop::Eof => Ok((nodes, p.macros)),
        Stop::Close(n) => Err(perr(format!("Unexpected closing tag </#{}>", n))),
        Stop::CallEnd => Err(perr("Unexpected closing </@...>")),
        _ => Err(perr("Unexpected directive outside of its parent")),
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

enum Flow {
    Normal,
    Break,
    Continue,
    Return(Option<Value>),
}

#[derive(Clone)]
struct LoopInfo {
    var: String,
    index: usize,
    len: Option<usize>,
}

struct Frame {
    locals: ObjectMap,
    nested: Option<(Arc<Vec<Node>>, Vec<String>, usize)>,
}

struct Ctx<'p> {
    model: ObjectMap,
    globals: ObjectMap,
    frames: Vec<Frame>,
    loops: Vec<(LoopInfo, ObjectMap)>,
    macros: HashMap<String, MacroDef>,
    partials: PartialLoader<'p>,
    number_format: String,
    boolean_format: Option<(String, String)>,
    escapes: Vec<Option<(String, Expr)>>,
    output_format: Vec<Option<String>>,
    depth: usize,
    pending_items: Option<Vec<(Option<String>, Value)>>,
}

fn fm_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) | Value::Float(_) => "number",
        Value::String(_) | Value::Bytes(_) => "string",
        Value::Array(_) => "sequence",
        Value::Object(_) => "hash",
    }
}

fn need_bool(v: &Value, src: &str) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(CamelError::new("NonBooleanException", format!("FreeMarker template error: For \"if\" condition: Expected a boolean, but this has evaluated to a {}: ==> {}", fm_type(other), src))),
    }
}

fn need_num(v: &Value, what: &str) -> Result<f64> {
    v.as_f64().filter(|_| matches!(v, Value::Int(_) | Value::Float(_))).ok_or_else(|| CamelError::new("NonNumericalException", format!("FreeMarker template error: {}: Expected a number, but this has evaluated to a {}", what, fm_type(v))))
}

fn num_val(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

/// Format a number with a `java.text.DecimalFormat` pattern subset (`0 # , . %` and literal
/// prefix/suffix).
fn decimal_format(x: f64, pattern: &str) -> String {
    let pattern = pattern.split(';').next().unwrap_or(pattern);
    let is_pat = |c: char| matches!(c, '0' | '#' | ',' | '.');
    let start = pattern.find(is_pat).unwrap_or(pattern.len());
    let end = pattern.rfind(is_pat).map(|e| e + 1).unwrap_or(start);
    let (prefix, core, suffix) = (&pattern[..start], &pattern[start..end], &pattern[end..]);
    let percent = suffix.contains('%') || prefix.contains('%');
    let x = if percent { x * 100.0 } else { x };
    let (ip, fp) = core.split_once('.').unwrap_or((core, ""));
    let min_int = ip.chars().filter(|c| *c == '0').count();
    let grouping = ip.rfind(',').map(|p| ip[p + 1..].chars().count());
    let min_frac = fp.chars().filter(|c| *c == '0').count();
    let max_frac = fp.chars().filter(|c| *c == '0' || *c == '#').count();
    let neg = x < 0.0;
    let s = format!("{:.*}", max_frac, x.abs());
    let (mut int_part, mut frac_part) = match s.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (s.clone(), String::new()),
    };
    while frac_part.len() > min_frac && frac_part.ends_with('0') {
        frac_part.pop();
    }
    if int_part == "0" && min_int == 0 && !frac_part.is_empty() {
        int_part.clear();
    }
    while int_part.len() < min_int {
        int_part.insert(0, '0');
    }
    if let Some(g) = grouping.filter(|g| *g > 0) {
        let digits: Vec<char> = int_part.chars().collect();
        let mut out = String::new();
        for (k, d) in digits.iter().enumerate() {
            if k > 0 && (digits.len() - k) % g == 0 {
                out.push(',');
            }
            out.push(*d);
        }
        int_part = out;
    }
    let mut out = String::new();
    if neg && (int_part.chars().any(|c| c != '0' && c != ',') || frac_part.chars().any(|c| c != '0')) {
        out.push('-');
    }
    out.push_str(&prefix.replace('\'', ""));
    out.push_str(&int_part);
    if !frac_part.is_empty() {
        out.push('.');
        out.push_str(&frac_part);
    }
    out.push_str(&suffix.replace('\'', ""));
    out
}

fn computer(v: &Value) -> String {
    match v {
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e16 => format!("{}", *f as i64),
        Value::Float(f) if f.is_infinite() => if *f > 0.0 { "INF".into() } else { "-INF".into() },
        Value::Float(f) if f.is_nan() => "NaN".into(),
        Value::Float(f) => format!("{}", f),
        other => other.to_string(),
    }
}

fn escape_html(s: &str, xml: bool) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str(if xml { "&apos;" } else { "&#39;" }),
            c => o.push(c),
        }
    }
    o
}

fn url_escape(s: &str, keep_slash: bool) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) || (keep_slash && b == b'/') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{:02X}", b));
        }
    }
    o
}

fn js_escape(s: &str, json: bool) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\'' if !json => o.push_str("\\'"),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '/' if json => o.push('/'),
            '<' if !json => o.push_str("\\x3C"),
            '>' if !json => o.push_str("\\>"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn cmp_values(a: &Value, b: &Value, op: &str) -> Result<bool> {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64().unwrap().partial_cmp(&b.as_f64().unwrap()).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) if op == "==" || op == "!=" => x.cmp(y),
        _ => {
            if op == "==" || op == "!=" {
                return Err(terr(format!("Can't compare values of these types. Allowed comparisons are between two numbers, two strings, two dates, or two booleans. Left hand operand is a {}, right hand operand is a {}", fm_type(a), fm_type(b))));
            }
            return Err(terr(format!("Can't compare values of these types: {} and {}", fm_type(a), fm_type(b))));
        }
    };
    Ok(match op {
        "==" => ord == Ordering::Equal,
        "!=" => ord != Ordering::Equal,
        "<" => ord == Ordering::Less,
        "<=" => ord != Ordering::Greater,
        ">" => ord == Ordering::Greater,
        _ => ord != Ordering::Less,
    })
}

fn compress(s: &str) -> String {
    let mut lines = Vec::new();
    for line in s.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    let mut out = lines.join("\n");
    if s.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

impl<'p> Ctx<'p> {
    fn lookup(&self, name: &str) -> Option<Value> {
        for (_, vars) in self.loops.iter().rev() {
            if let Some(v) = vars.get(name) {
                return Some(v.clone());
            }
        }
        if let Some(f) = self.frames.last() {
            if let Some(v) = f.locals.get(name) {
                return Some(v.clone());
            }
        }
        if let Some(v) = self.globals.get(name) {
            return Some(v.clone());
        }
        self.model.get(name).cloned()
    }

    fn loop_info(&self, name: &str) -> Option<LoopInfo> {
        self.loops.iter().rev().find(|(i, _)| i.var == name).map(|(i, _)| i.clone())
    }

    fn set(&mut self, scope: Scope, name: &str, v: Value) {
        match scope {
            Scope::Local => {
                if let Some(f) = self.frames.last_mut() {
                    f.locals.insert(name.to_string(), v);
                    return;
                }
                self.globals.insert(name.to_string(), v);
            }
            _ => {
                self.globals.insert(name.to_string(), v);
            }
        }
    }

    fn eval_req(&mut self, e: &Expr) -> Result<Value> {
        match self.eval(e)? {
            Some(v) => Ok(v),
            None => Err(missing(&e.src())),
        }
    }

    fn to_text(&self, v: &Value, src: &str) -> Result<String> {
        Ok(match v {
            Value::String(s) => s.clone(),
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            Value::Int(_) | Value::Float(_) => self.format_number(v),
            Value::Bool(b) => match &self.boolean_format {
                Some((t, f)) => if *b { t.clone() } else { f.clone() },
                None => {
                    return Err(CamelError::new(
                        "NonStringException",
                        format!("FreeMarker template error: Can't convert boolean to string automatically, because the \"boolean_format\" setting was \"true,false\", which is the legacy deprecated default, and we treat it as if no format was set. This is the default configuration; you should provide the format explicitly for each place where you print a boolean, like: something?string('yes', 'no'), something?c ==> {}", src),
                    ))
                }
            },
            Value::Null => return Err(missing(src)),
            other => {
                return Err(CamelError::new(
                    "NonStringException",
                    format!("FreeMarker template error: For \"${{...}}\" content: Expected a string or something automatically convertible to string (number, date or boolean), but this has evaluated to a {}: ==> {}", fm_type(other), src),
                ))
            }
        })
    }

    fn format_number(&self, v: &Value) -> String {
        let f = v.as_f64().unwrap_or(0.0);
        match self.number_format.as_str() {
            "computer" | "c" => computer(v),
            "number" | "" => decimal_format(f, "#,##0.###"),
            "percent" => decimal_format(f, "#,##0%"),
            "currency" => format!("${}", decimal_format(f, "#,##0.00")),
            p => decimal_format(f, p),
        }
    }

    fn eval(&mut self, e: &Expr) -> Result<Option<Value>> {
        Ok(Some(match e {
            Expr::Lit(v) => v.clone(),
            Expr::Str(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        StrPart::Lit(l) => s.push_str(l),
                        StrPart::Interp(x) => {
                            let v = self.eval_req(x)?;
                            s.push_str(&self.to_text(&v, &x.src())?);
                        }
                    }
                }
                Value::String(s)
            }
            Expr::Var(n) => match self.lookup(n) {
                Some(v) if !v.is_null() => v,
                _ => return Ok(None),
            },
            Expr::Special(n) => match n.as_str() {
                "now" => Value::String(crate::util_time::format_millis(crate::util_time::now_millis(), "MMM d, yyyy h:mm:ss a")),
                "vars" | "globals" | "main" | "namespace" => {
                    let mut m = self.model.clone();
                    for (k, v) in &self.globals {
                        m.insert(k.clone(), v.clone());
                    }
                    Value::Object(m)
                }
                "data_model" => Value::Object(self.model.clone()),
                "locals" => Value::Object(self.frames.last().map(|f| f.locals.clone()).unwrap_or_default()),
                "version" => Value::from("2.3.32"),
                "lang" => Value::from("en"),
                "locale" => Value::from("en_US"),
                "output_format" => Value::from(self.output_format.last().cloned().flatten().unwrap_or_else(|| "undefined".into())),
                other => return Err(terr(format!("Unknown special variable: .{}", other))),
            },
            Expr::Seq(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.push(self.eval_req(i)?);
                }
                Value::Array(v)
            }
            Expr::Hash(entries) => {
                let mut m = ObjectMap::new();
                for (k, v) in entries {
                    let k = self.eval_req(k)?.to_string();
                    let v = self.eval_req(v)?;
                    m.insert(k, v);
                }
                Value::Object(m)
            }
            Expr::Range(a, b, kind) => {
                let lo = need_num(&self.eval_req(a)?, "range")? as i64;
                match (b, kind) {
                    (None, _) => Value::Array((lo..lo + 10_000).map(Value::Int).collect()),
                    (Some(b), k) => {
                        let hi = need_num(&self.eval_req(b)?, "range")? as i64;
                        let items: Vec<i64> = match k {
                            RangeKind::Len => (lo..lo + hi.max(0)).collect(),
                            RangeKind::Excl => {
                                if hi >= lo {
                                    (lo..hi).collect()
                                } else {
                                    ((hi + 1)..=lo).rev().collect()
                                }
                            }
                            _ => {
                                if hi >= lo {
                                    (lo..=hi).collect()
                                } else {
                                    (hi..=lo).rev().collect()
                                }
                            }
                        };
                        Value::Array(items.into_iter().map(Value::Int).collect())
                    }
                }
            }
            Expr::Dot(t, n) => {
                let Some(tv) = self.eval(t)? else { return Ok(None) };
                match &tv {
                    Value::Object(_) => match lookup_key(&tv, n) {
                        Some(v) if !v.is_null() => v.clone(),
                        _ => return Ok(None),
                    },
                    _ => match super::template_support::java_property(&tv, n) {
                        Some(v) if !v.is_null() => v,
                        _ => return Ok(None),
                    },
                }
            }
            Expr::Index(t, i) => {
                let Some(tv) = self.eval(t)? else { return Ok(None) };
                if let Expr::Range(a, b, kind) = &**i {
                    let lo = need_num(&self.eval_req(a)?, "slice")? as i64;
                    let len = match &tv {
                        Value::String(s) => s.chars().count() as i64,
                        Value::Array(x) => x.len() as i64,
                        other => return Err(terr(format!("Can't slice a {}", fm_type(other)))),
                    };
                    let hi = match (b, kind) {
                        (None, _) => len,
                        (Some(b), RangeKind::Len) => (lo + need_num(&self.eval_req(b)?, "slice")? as i64).min(len),
                        (Some(b), RangeKind::Excl) => need_num(&self.eval_req(b)?, "slice")? as i64,
                        (Some(b), _) => need_num(&self.eval_req(b)?, "slice")? as i64 + 1,
                    };
                    if lo < 0 || hi > len || lo > hi {
                        return Err(terr(format!("Range [{}..{}) is out of bounds (length {})", lo, hi, len)));
                    }
                    return Ok(Some(match tv {
                        Value::String(s) => Value::String(s.chars().skip(lo as usize).take((hi - lo) as usize).collect()),
                        Value::Array(x) => Value::Array(x[lo as usize..hi as usize].to_vec()),
                        _ => unreachable!(),
                    }));
                }
                let k = self.eval_req(i)?;
                match (&tv, &k) {
                    (Value::Array(a), Value::Int(_) | Value::Float(_)) => match a.get(need_num(&k, "index")? as usize) {
                        Some(v) if !v.is_null() => v.clone(),
                        _ => return Ok(None),
                    },
                    (Value::String(s), Value::Int(n)) => match s.chars().nth(*n as usize) {
                        Some(c) => Value::String(c.to_string()),
                        None => return Err(terr(format!("String index {} out of bounds", n))),
                    },
                    (Value::Object(_), _) => match lookup_key(&tv, &k.to_string()) {
                        Some(v) if !v.is_null() => v.clone(),
                        _ => return Ok(None),
                    },
                    _ => return Err(terr(format!("Can't index a {} with a {}", fm_type(&tv), fm_type(&k)))),
                }
            }
            Expr::Call(t, args) => {
                // user-defined function, or a method on the target
                if let Expr::Var(n) = &**t {
                    if let Some(f) = self.macros.get(n).cloned() {
                        if f.is_function {
                            let mut vals = Vec::new();
                            for a in args {
                                vals.push(self.eval(a)?.unwrap_or(Value::Null));
                            }
                            return self.call_function(n, &f, vals);
                        }
                    }
                }
                if let Expr::Dot(target, method) = &**t {
                    let Some(tv) = self.eval(target)? else { return Ok(None) };
                    let mut vals = Vec::new();
                    for a in args {
                        vals.push(self.eval_req(a)?);
                    }
                    return Ok(super::template_support::java_method(&tv, method, &vals).filter(|v| !v.is_null()));
                }
                return Err(terr(format!("{} is not a function or method", t.src())));
            }
            Expr::Builtin(t, name, args) => return self.builtin(t, name, args.as_deref()),
            Expr::Default(t, d) => match self.eval(t) {
                Ok(Some(v)) => v,
                Ok(None) | Err(_) => match d {
                    Some(d) => return self.eval(d),
                    None => Value::String(String::new()),
                },
            },
            Expr::Exists(t) => Value::Bool(matches!(self.eval(t), Ok(Some(_)))),
            Expr::Not(x) => {
                let v = self.eval_req(x)?;
                Value::Bool(!need_bool(&v, &x.src())?)
            }
            Expr::Neg(x) => {
                let v = self.eval_req(x)?;
                num_val(-need_num(&v, "negation")?)
            }
            Expr::Lambda(..) => return Err(terr("A lambda can only be used as a built-in argument")),
            Expr::Bin(op, a, b) => match *op {
                "&&" => {
                    let av = self.eval_req(a)?;
                    if !need_bool(&av, &a.src())? {
                        Value::Bool(false)
                    } else {
                        let bv = self.eval_req(b)?;
                        Value::Bool(need_bool(&bv, &b.src())?)
                    }
                }
                "||" => {
                    let av = self.eval_req(a)?;
                    if need_bool(&av, &a.src())? {
                        Value::Bool(true)
                    } else {
                        let bv = self.eval_req(b)?;
                        Value::Bool(need_bool(&bv, &b.src())?)
                    }
                }
                "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                    let (av, bv) = (self.eval_req(a)?, self.eval_req(b)?);
                    Value::Bool(cmp_values(&av, &bv, op)?)
                }
                "+" => {
                    let (av, bv) = (self.eval_req(a)?, self.eval_req(b)?);
                    match (&av, &bv) {
                        (Value::Int(x), Value::Int(y)) => x.checked_add(*y).map(Value::Int).unwrap_or(Value::Float(*x as f64 + *y as f64)),
                        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => num_val(av.as_f64().unwrap() + bv.as_f64().unwrap()),
                        (Value::Array(x), Value::Array(y)) => Value::Array(x.iter().chain(y.iter()).cloned().collect()),
                        (Value::Object(x), Value::Object(y)) => {
                            let mut m = x.clone();
                            for (k, v) in y {
                                m.insert(k.clone(), v.clone());
                            }
                            Value::Object(m)
                        }
                        _ => Value::String(format!("{}{}", self.to_text(&av, &a.src())?, self.to_text(&bv, &b.src())?)),
                    }
                }
                o => {
                    let (av, bv) = (self.eval_req(a)?, self.eval_req(b)?);
                    let (x, y) = (need_num(&av, "arithmetic")?, need_num(&bv, "arithmetic")?);
                    match o {
                        "-" => match (&av, &bv) {
                            (Value::Int(p), Value::Int(q)) => Value::Int(p - q),
                            _ => num_val(x - y),
                        },
                        "*" => match (&av, &bv) {
                            (Value::Int(p), Value::Int(q)) => Value::Int(p * q),
                            _ => num_val(x * y),
                        },
                        "/" => {
                            if y == 0.0 {
                                return Err(terr("Arithmetic operation failed: division by zero"));
                            }
                            num_val(x / y)
                        }
                        _ => {
                            if y == 0.0 {
                                return Err(terr("Arithmetic operation failed: division by zero"));
                            }
                            num_val(x % y)
                        }
                    }
                }
            },
        }))
    }

    fn call_lambda(&mut self, l: &Expr, v: Value) -> Result<Value> {
        match l {
            Expr::Lambda(n, body) => {
                let mut vars = ObjectMap::new();
                vars.insert(n.clone(), v);
                self.loops.push((LoopInfo { var: String::new(), index: 0, len: None }, vars));
                let r = self.eval(body);
                self.loops.pop();
                Ok(r?.unwrap_or(Value::Null))
            }
            other => {
                // a function name
                if let Expr::Var(n) = other {
                    if let Some(f) = self.macros.get(n).cloned() {
                        return Ok(self.call_function(n, &f, vec![v])?.unwrap_or(Value::Null));
                    }
                }
                Err(terr("Expected a lambda expression (x -> ...) or a function"))
            }
        }
    }

    fn builtin(&mut self, t: &Expr, name: &str, args: Option<&[Expr]>) -> Result<Option<Value>> {
        // null-tolerant built-ins
        match name {
            "has_content" => {
                return Ok(Some(Value::Bool(match self.eval(t) {
                    Ok(Some(v)) => match &v {
                        Value::String(s) => !s.is_empty(),
                        Value::Array(a) => !a.is_empty(),
                        Value::Object(o) => !o.is_empty(),
                        _ => true,
                    },
                    _ => false,
                })))
            }
            "exists" => return Ok(Some(Value::Bool(matches!(self.eval(t), Ok(Some(_)))))),
            "if_exists" => return Ok(Some(self.eval(t)?.unwrap_or(Value::String(String::new())))),
            "default" => {
                return match self.eval(t)? {
                    Some(v) => Ok(Some(v)),
                    None => match args.and_then(|a| a.first()) {
                        Some(d) => self.eval(d),
                        None => Ok(Some(Value::String(String::new()))),
                    },
                }
            }
            "is_macro" | "is_directive" => return Ok(Some(Value::Bool(matches!(t, Expr::Var(n) if self.macros.get(n).map(|m| !m.is_function).unwrap_or(false))))),
            "is_method" | "is_function" => return Ok(Some(Value::Bool(matches!(t, Expr::Var(n) if self.macros.get(n).map(|m| m.is_function).unwrap_or(false))))),
            _ => {}
        }
        // loop-variable built-ins
        if let Expr::Var(v) = t {
            if let Some(info) = self.loop_info(v) {
                let last = info.len.map(|l| info.index + 1 == l);
                let r = match name {
                    "index" => Some(Value::Int(info.index as i64)),
                    "counter" => Some(Value::Int(info.index as i64 + 1)),
                    "has_next" => Some(Value::Bool(!last.unwrap_or(false))),
                    "is_first" => Some(Value::Bool(info.index == 0)),
                    "is_last" => Some(Value::Bool(last.unwrap_or(false))),
                    "item_parity" => Some(Value::from(if info.index % 2 == 0 { "odd" } else { "even" })),
                    "item_parity_cap" => Some(Value::from(if info.index % 2 == 0 { "Odd" } else { "Even" })),
                    "is_odd_item" => Some(Value::Bool(info.index % 2 == 0)),
                    "is_even_item" => Some(Value::Bool(info.index % 2 == 1)),
                    "item_cycle" => {
                        let a = args.unwrap_or(&[]);
                        if a.is_empty() {
                            return Err(terr("?item_cycle needs at least one argument"));
                        }
                        Some(self.eval_req(&a[info.index % a.len()])?)
                    }
                    _ => None,
                };
                if r.is_some() {
                    return Ok(r);
                }
            }
        }
        let v = self.eval_req(t)?;
        let mut a = Vec::new();
        let lambda_bis = ["filter", "map", "take_while", "drop_while"];
        if !lambda_bis.contains(&name) {
            for x in args.unwrap_or(&[]) {
                a.push(self.eval_req(x)?);
            }
        }
        let s = |i: usize| a.get(i).map(|x| x.to_string()).unwrap_or_default();
        let bad = |what: &str| -> CamelError { terr(format!("?{} can't be applied to a {} ({})", name, fm_type(&v), what)) };
        Ok(Some(match (name, &v) {
            ("string" | "c" | "string.computer" | "string.c", Value::Bool(b)) if a.len() < 2 => Value::String(b.to_string()),
            ("string", Value::Bool(b)) => Value::String(if *b { s(0) } else { s(1) }),
            ("then", Value::Bool(b)) => {
                if a.len() != 2 {
                    return Err(terr("?then needs exactly 2 arguments"));
                }
                if *b {
                    a[0].clone()
                } else {
                    a[1].clone()
                }
            }
            ("c" | "string.computer" | "string.c", Value::Int(_) | Value::Float(_)) => Value::String(computer(&v)),
            ("c", Value::String(_)) => v.clone(),
            ("string", Value::Int(_) | Value::Float(_)) => Value::String(match a.first() {
                Some(p) => decimal_format(v.as_f64().unwrap(), &p.to_string()),
                None => self.format_number(&v),
            }),
            ("string.number", Value::Int(_) | Value::Float(_)) => Value::String(decimal_format(v.as_f64().unwrap(), "#,##0.###")),
            ("string.percent", Value::Int(_) | Value::Float(_)) => Value::String(decimal_format(v.as_f64().unwrap(), "#,##0%")),
            ("string.currency", Value::Int(_) | Value::Float(_)) => Value::String(format!("${}", decimal_format(v.as_f64().unwrap(), "#,##0.00"))),
            ("string", Value::String(_)) => v.clone(),
            ("abs", Value::Int(i)) => Value::Int(i.abs()),
            ("abs", Value::Float(f)) => Value::Float(f.abs()),
            ("round", Value::Int(_) | Value::Float(_)) => Value::Int((v.as_f64().unwrap() + 0.5).floor() as i64),
            ("floor", Value::Int(_) | Value::Float(_)) => Value::Int(v.as_f64().unwrap().floor() as i64),
            ("ceiling", Value::Int(_) | Value::Float(_)) => Value::Int(v.as_f64().unwrap().ceil() as i64),
            ("int" | "long" | "short" | "byte", Value::Int(_) | Value::Float(_)) => Value::Int(v.as_f64().unwrap().trunc() as i64),
            ("double" | "float", Value::Int(_) | Value::Float(_)) => Value::Float(v.as_f64().unwrap()),
            ("number", Value::Int(_) | Value::Float(_)) => v.clone(),
            ("is_nan", Value::Int(_) | Value::Float(_)) => Value::Bool(v.as_f64().unwrap().is_nan()),
            ("is_infinite", Value::Int(_) | Value::Float(_)) => Value::Bool(v.as_f64().unwrap().is_infinite()),
            ("lower_abc" | "upper_abc", Value::Int(n)) => {
                if *n < 1 {
                    return Err(terr("?lower_abc requires a number >= 1"));
                }
                let mut n = *n;
                let mut out = Vec::new();
                while n > 0 {
                    n -= 1;
                    out.push((b'a' + (n % 26) as u8) as char);
                    n /= 26;
                }
                let s: String = out.into_iter().rev().collect();
                Value::String(if name == "upper_abc" { s.to_uppercase() } else { s })
            }
            ("is_string", _) => Value::Bool(matches!(v, Value::String(_))),
            ("is_number", _) => Value::Bool(matches!(v, Value::Int(_) | Value::Float(_))),
            ("is_boolean", _) => Value::Bool(matches!(v, Value::Bool(_))),
            ("is_sequence" | "is_collection" | "is_indexable", _) => Value::Bool(matches!(v, Value::Array(_))),
            ("is_enumerable", _) => Value::Bool(matches!(v, Value::Array(_) | Value::Object(_))),
            ("is_hash" | "is_hash_ex", _) => Value::Bool(matches!(v, Value::Object(_))),
            ("is_date" | "is_date_like", _) => Value::Bool(false),
            ("switch", _) => {
                let mut k = 0;
                while k + 1 < a.len() {
                    if cmp_values(&v, &a[k], "==").unwrap_or(false) {
                        return Ok(Some(a[k + 1].clone()));
                    }
                    k += 2;
                }
                if a.len() % 2 == 1 {
                    a[a.len() - 1].clone()
                } else {
                    return Err(terr("?switch: no case matched and no default was given"));
                }
            }
            (_, Value::String(st)) => {
                let st = st.clone();
                match name {
                    "upper_case" => Value::String(st.to_uppercase()),
                    "lower_case" => Value::String(st.to_lowercase()),
                    "cap_first" => {
                        let lead = st.len() - st.trim_start().len();
                        let mut cs = st[lead..].chars();
                        Value::String(match cs.next() {
                            Some(c) => format!("{}{}{}", &st[..lead], c.to_uppercase(), cs.as_str()),
                            None => st.clone(),
                        })
                    }
                    "uncap_first" => {
                        let lead = st.len() - st.trim_start().len();
                        let mut cs = st[lead..].chars();
                        Value::String(match cs.next() {
                            Some(c) => format!("{}{}{}", &st[..lead], c.to_lowercase(), cs.as_str()),
                            None => st.clone(),
                        })
                    }
                    "capitalize" => {
                        let mut out = String::new();
                        let mut start = true;
                        for c in st.chars() {
                            if c.is_whitespace() {
                                start = true;
                                out.push(c);
                            } else if start {
                                out.extend(c.to_uppercase());
                                start = false;
                            } else {
                                out.extend(c.to_lowercase());
                            }
                        }
                        Value::String(out)
                    }
                    "trim" => Value::String(st.trim().to_string()),
                    "length" => Value::Int(st.chars().count() as i64),
                    "size" => return Err(bad("use ?length for strings")),
                    "contains" => Value::Bool(st.contains(&s(0))),
                    "starts_with" => Value::Bool(st.starts_with(&s(0))),
                    "ends_with" => Value::Bool(st.ends_with(&s(0))),
                    "index_of" => {
                        let from = a.get(1).and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
                        let byte_from = st.char_indices().nth(from).map(|(b, _)| b).unwrap_or(st.len());
                        Value::Int(st[byte_from..].find(&s(0)).map(|b| st[..byte_from + b].chars().count() as i64).unwrap_or(-1))
                    }
                    "last_index_of" => Value::Int(st.rfind(&s(0)).map(|b| st[..b].chars().count() as i64).unwrap_or(-1)),
                    "replace" => {
                        let flags = s(2);
                        if flags.contains('r') {
                            let re = crate::regex::Regex::with_flags(&s(0), flags.contains('i')).map_err(|e| terr(e.message))?;
                            Value::String(if flags.contains('f') { re.replace_first(&st, &s(1)) } else { re.replace_all(&st, &s(1)) })
                        } else if flags.contains('i') {
                            let re = crate::regex::Regex::with_flags(&crate::regex::Regex::quote(&s(0)), true).map_err(|e| terr(e.message))?;
                            Value::String(re.replace_all(&st, &s(1).replace('$', "\\$")))
                        } else if flags.contains('f') {
                            Value::String(st.replacen(&s(0), &s(1), 1))
                        } else {
                            Value::String(st.replace(&s(0), &s(1)))
                        }
                    }
                    "split" => {
                        let flags = s(1);
                        let parts: Vec<String> = if flags.contains('r') {
                            crate::regex::Regex::with_flags(&s(0), flags.contains('i')).map_err(|e| terr(e.message))?.split(&st)
                        } else {
                            st.split(&s(0)).map(String::from).collect()
                        };
                        Value::from(parts)
                    }
                    "html" => Value::String(escape_html(&st, false)),
                    "xhtml" | "xml" => Value::String(escape_html(&st, true)),
                    "rtf" => Value::String(st.replace('\\', "\\\\").replace('{', "\\{").replace('}', "\\}")),
                    "url" => Value::String(url_escape(&st, false)),
                    "url_path" => Value::String(url_escape(&st, true)),
                    "js_string" => Value::String(js_escape(&st, false)),
                    "json_string" => Value::String(js_escape(&st, true)),
                    "j_string" => Value::String(js_escape(&st, true).replace("\\/", "/")),
                    "number" => {
                        let t = st.trim();
                        match t.parse::<i64>() {
                            Ok(i) => Value::Int(i),
                            Err(_) => Value::Float(t.parse::<f64>().map_err(|_| terr(format!("Can't convert this string to number: \"{}\"", st)))?),
                        }
                    }
                    "boolean" => match st.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => return Err(terr(format!("Can't convert this string to boolean: \"{}\"", st))),
                    },
                    "left_pad" | "right_pad" => {
                        let n = a.first().and_then(|x| x.as_i64()).unwrap_or(0) as usize;
                        let pad = a.get(1).map(|x| x.to_string()).unwrap_or_else(|| " ".into());
                        let len = st.chars().count();
                        if len >= n || pad.is_empty() {
                            Value::String(st)
                        } else {
                            let fill: String = pad.chars().cycle().take(n - len).collect();
                            Value::String(if name == "left_pad" { format!("{}{}", fill, st) } else { format!("{}{}", st, fill) })
                        }
                    }
                    "chop_linebreak" => Value::String(st.strip_suffix("\r\n").or_else(|| st.strip_suffix('\n')).or_else(|| st.strip_suffix('\r')).unwrap_or(&st).to_string()),
                    "word_list" => Value::from(st.split_whitespace().map(String::from).collect::<Vec<_>>()),
                    "matches" => {
                        let re = crate::regex::Regex::with_flags(&s(0), s(1).contains('i')).map_err(|e| terr(e.message))?;
                        Value::Bool(re.is_full_match(&st))
                    }
                    "keep_before" => Value::String(st.find(&s(0)).map(|p| st[..p].to_string()).unwrap_or(st.clone())),
                    "keep_after" => Value::String(st.find(&s(0)).map(|p| st[p + s(0).len()..].to_string()).unwrap_or_default()),
                    "keep_before_last" => Value::String(st.rfind(&s(0)).map(|p| st[..p].to_string()).unwrap_or(st.clone())),
                    "keep_after_last" => Value::String(st.rfind(&s(0)).map(|p| st[p + s(0).len()..].to_string()).unwrap_or_default()),
                    "remove_beginning" => Value::String(st.strip_prefix(&s(0)).unwrap_or(&st).to_string()),
                    "remove_ending" => Value::String(st.strip_suffix(&s(0)).unwrap_or(&st).to_string()),
                    "ensure_starts_with" => Value::String(if st.starts_with(&s(0)) { st } else { format!("{}{}", s(0), st) }),
                    "ensure_ends_with" => Value::String(if st.ends_with(&s(0)) { st } else { format!("{}{}", st, s(0)) }),
                    "truncate" | "truncate_w" | "truncate_c" => {
                        let n = a.first().and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
                        let term = a.get(1).map(|x| x.to_string()).unwrap_or_else(|| "[...]".into());
                        if st.chars().count() <= n {
                            Value::String(st)
                        } else {
                            let keep = n.saturating_sub(term.chars().count() + 1);
                            let base: String = st.chars().take(keep).collect();
                            Value::String(format!("{} {}", base.trim_end(), term))
                        }
                    }
                    "substring" => {
                        let chars: Vec<char> = st.chars().collect();
                        let from = a.first().and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
                        let to = a.get(1).and_then(|x| x.as_i64()).map(|x| x as usize).unwrap_or(chars.len()).min(chars.len());
                        Value::String(chars.get(from..to).map(|c| c.iter().collect()).unwrap_or_default())
                    }
                    "eval" | "eval_json" => {
                        let chars: Vec<char> = st.chars().collect();
                        if name == "eval_json" {
                            crate::json::parse(&st).map_err(|e| terr(e.message))?
                        } else {
                            let e = parse_expr(&chars)?;
                            self.eval_req(&e)?
                        }
                    }
                    "no_esc" | "esc" => Value::String(st),
                    _ => return Err(bad("unknown or unsupported built-in for strings")),
                }
            }
            (_, Value::Array(items)) => {
                let items = items.clone();
                match name {
                    "size" => Value::Int(items.len() as i64),
                    "first" => return Ok(items.first().cloned().filter(|x| !x.is_null())),
                    "last" => return Ok(items.last().cloned().filter(|x| !x.is_null())),
                    "reverse" => Value::Array(items.into_iter().rev().collect()),
                    "join" => {
                        if items.is_empty() && a.len() > 1 {
                            return Ok(Some(a[1].clone()));
                        }
                        let mut parts = Vec::new();
                        for x in &items {
                            if x.is_null() {
                                continue;
                            }
                            parts.push(self.to_text(x, "sequence item")?);
                        }
                        let mut j = parts.join(&s(0));
                        if a.len() > 2 && !items.is_empty() {
                            j.push_str(&s(2));
                        }
                        Value::String(j)
                    }
                    "seq_contains" => Value::Bool(items.iter().any(|x| cmp_values(x, &a[0], "==").unwrap_or(false))),
                    "seq_index_of" => Value::Int(items.iter().position(|x| cmp_values(x, &a[0], "==").unwrap_or(false)).map(|p| p as i64).unwrap_or(-1)),
                    "seq_last_index_of" => Value::Int(items.iter().rposition(|x| cmp_values(x, &a[0], "==").unwrap_or(false)).map(|p| p as i64).unwrap_or(-1)),
                    "sort" | "sort_by" => {
                        let keys: Vec<String> = match (name, a.first()) {
                            ("sort_by", Some(Value::Array(ks))) => ks.iter().map(|k| k.to_string()).collect(),
                            ("sort_by", Some(k)) => vec![k.to_string()],
                            ("sort_by", None) => return Err(terr("?sort_by needs an argument")),
                            _ => vec![],
                        };
                        let key_of = |x: &Value| -> Value {
                            let mut cur = x.clone();
                            for k in &keys {
                                cur = lookup_key(&cur, k).cloned().unwrap_or(Value::Null);
                            }
                            cur
                        };
                        let mut sorted = items.clone();
                        let mut err = None;
                        sorted.sort_by(|x, y| {
                            let (kx, ky) = (key_of(x), key_of(y));
                            match (&kx, &ky) {
                                (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => kx.as_f64().unwrap().partial_cmp(&ky.as_f64().unwrap()).unwrap_or(std::cmp::Ordering::Equal),
                                (Value::String(p), Value::String(q)) => p.to_lowercase().cmp(&q.to_lowercase()).then(p.cmp(q)),
                                (Value::Bool(p), Value::Bool(q)) => p.cmp(q),
                                _ => {
                                    err = Some(terr("?sort: values must be all numbers, all strings or all booleans"));
                                    std::cmp::Ordering::Equal
                                }
                            }
                        });
                        if let Some(e) = err {
                            return Err(e);
                        }
                        Value::Array(sorted)
                    }
                    "chunk" => {
                        let n = a.first().and_then(|x| x.as_i64()).unwrap_or(1).max(1) as usize;
                        let mut out: Vec<Value> = items.chunks(n).map(|c| Value::Array(c.to_vec())).collect();
                        if let (Some(fill), Some(Value::Array(last))) = (a.get(1), out.last_mut()) {
                            while last.len() < n {
                                last.push(fill.clone());
                            }
                        }
                        Value::Array(out)
                    }
                    "min" | "max" => {
                        let mut best: Option<Value> = None;
                        for x in items.iter().filter(|x| !x.is_null()) {
                            best = Some(match best {
                                None => x.clone(),
                                Some(b) => {
                                    let less = cmp_values(x, &b, "<")?;
                                    if (name == "min") == less {
                                        x.clone()
                                    } else {
                                        b
                                    }
                                }
                            });
                        }
                        return Ok(best);
                    }
                    "filter" | "map" | "take_while" | "drop_while" => {
                        let l = args.and_then(|a| a.first()).ok_or_else(|| terr(format!("?{} needs a lambda argument", name)))?.clone();
                        let mut out = Vec::new();
                        let mut dropping = true;
                        for x in items {
                            let r = self.call_lambda(&l, x.clone())?;
                            match name {
                                "map" => out.push(r),
                                "filter" => {
                                    if need_bool(&r, "lambda")? {
                                        out.push(x)
                                    }
                                }
                                "take_while" => {
                                    if !need_bool(&r, "lambda")? {
                                        break;
                                    }
                                    out.push(x);
                                }
                                _ => {
                                    if dropping && need_bool(&r, "lambda")? {
                                        continue;
                                    }
                                    dropping = false;
                                    out.push(x);
                                }
                            }
                        }
                        Value::Array(out)
                    }
                    _ => return Err(bad("unknown or unsupported built-in for sequences")),
                }
            }
            (_, Value::Object(o)) => match name {
                "keys" => Value::Array(o.keys().map(|k| Value::String(k.clone())).collect()),
                "values" => Value::Array(o.values().cloned().collect()),
                "size" => Value::Int(o.len() as i64),
                _ => return Err(bad("unknown or unsupported built-in for hashes")),
            },
            ("date" | "time" | "datetime" | "date_if_unknown" | "iso" | "iso_utc", _) => return Err(terr(format!("?{}: dates are not supported by the rust-camel FreeMarker engine", name))),
            _ => return Err(bad("unknown or unsupported built-in")),
        }))
    }

    fn call_function(&mut self, name: &str, f: &MacroDef, vals: Vec<Value>) -> Result<Option<Value>> {
        let mut locals = ObjectMap::new();
        for (k, (p, d)) in f.params.iter().enumerate() {
            let v = match vals.get(k).cloned().filter(|v| !v.is_null()) {
                Some(v) => v,
                None => match d {
                    Some(d) => self.eval_req(d)?,
                    None => return Err(terr(format!("Function \"{}\" needs parameter \"{}\"", name, p))),
                },
            };
            locals.insert(p.clone(), v);
        }
        if let Some(rest) = &f.catch_all {
            locals.insert(rest.clone(), Value::Array(vals.into_iter().skip(f.params.len()).collect()));
        }
        self.depth += 1;
        if self.depth > 200 {
            return Err(terr("function recursion too deep"));
        }
        self.frames.push(Frame { locals, nested: None });
        let mut sink = String::new();
        let r = self.render(&f.body.clone(), &mut sink);
        self.frames.pop();
        self.depth -= 1;
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(None),
        }
    }

    fn render(&mut self, nodes: &[Node], out: &mut String) -> Result<Flow> {
        for n in nodes {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Interp(e) => {
                    let v = match self.escapes.last().cloned().flatten() {
                        Some((var, ex)) => {
                            let inner = self.eval_req(e)?;
                            let mut vars = ObjectMap::new();
                            vars.insert(var, inner);
                            self.loops.push((LoopInfo { var: String::new(), index: 0, len: None }, vars));
                            let r = self.eval_req(&ex);
                            self.loops.pop();
                            r?
                        }
                        None => self.eval_req(e)?,
                    };
                    let text = self.to_text(&v, &e.src())?;
                    let text = match self.output_format.last().cloned().flatten().as_deref() {
                        Some("HTML") => escape_html(&text, false),
                        Some("XHTML") | Some("XML") => escape_html(&text, true),
                        _ => text,
                    };
                    out.push_str(&text);
                }
                Node::If(branches, otherwise) => {
                    let mut done = false;
                    for (c, body) in branches {
                        let v = self.eval_req(c)?;
                        if need_bool(&v, &c.src())? {
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
                Node::List { seq, vars, body, otherwise } => {
                    let v = self.eval_req(seq)?;
                    let items: Vec<(Option<String>, Value)> = match v {
                        Value::Array(a) => a.into_iter().map(|x| (None, x)).collect(),
                        Value::Object(o) => o.into_iter().map(|(k, x)| (Some(k), x)).collect(),
                        other => return Err(terr(format!("Expected a sequence or hash for #list, but this has evaluated to a {}: ==> {}", fm_type(&other), seq.src()))),
                    };
                    if items.is_empty() {
                        if let Some(o) = otherwise {
                            match self.render(o, out)? {
                                Flow::Normal => {}
                                f => return Ok(f),
                            }
                        }
                        continue;
                    }
                    if vars.is_empty() {
                        // <#list seq> ... <#items as x> ... </#items> ... </#list>
                        self.pending_items = Some(items);
                        let f = self.render(body, out);
                        self.pending_items = None;
                        match f? {
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            _ => continue,
                        }
                    }
                    match self.iterate(items, vars, body, out)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Node::Items { vars, body } => {
                    let items = self.pending_items.take().ok_or_else(|| terr("<#items> must be inside <#list seq>"))?;
                    match self.iterate(items, vars, body, out)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Break => return Ok(Flow::Break),
                        _ => {}
                    }
                }
                Node::Sep(body) => {
                    let has_next = self.loops.last().map(|(i, _)| i.len.map(|l| i.index + 1 < l).unwrap_or(false)).unwrap_or(false);
                    if has_next {
                        match self.render(body, out)? {
                            Flow::Normal => {}
                            f => return Ok(f),
                        }
                    }
                }
                Node::Break => return Ok(Flow::Break),
                Node::Continue => return Ok(Flow::Continue),
                Node::Return(e) => {
                    let v = match e {
                        Some(e) => self.eval(e)?,
                        None => None,
                    };
                    return Ok(Flow::Return(v));
                }
                Node::Stop(e) => {
                    let msg = match e {
                        Some(e) => self.eval_req(e)?.to_string(),
                        None => String::new(),
                    };
                    return Err(CamelError::new("StopException", format!("FreeMarker: stop {}", msg)));
                }
                Node::Assign(scope, ops) => {
                    for (name, op, e) in ops {
                        let cur = self.lookup(name);
                        let v = match (op.as_str(), e) {
                            ("=", Some(e)) => self.eval_req(e)?,
                            ("++" | "--", _) => {
                                let c = cur.ok_or_else(|| missing(name))?;
                                num_val(need_num(&c, name)? + if op == "++" { 1.0 } else { -1.0 })
                            }
                            (o, Some(e)) => {
                                let c = cur.ok_or_else(|| missing(name))?;
                                let bin = match o {
                                    "+=" => "+",
                                    "-=" => "-",
                                    "*=" => "*",
                                    "/=" => "/",
                                    _ => "%",
                                };
                                let expr = Expr::Bin(bin, Box::new(Expr::Lit(c)), Box::new(e.clone()));
                                self.eval_req(&expr)?
                            }
                            _ => return Err(perr(format!("bad assignment to {}", name))),
                        };
                        self.set(*scope, name, v);
                    }
                }
                Node::Capture(scope, name, body) => {
                    let mut buf = String::new();
                    self.render(body, &mut buf)?;
                    self.set(*scope, name, Value::String(buf));
                }
                Node::Call { name, named, positional, body, loop_vars } => {
                    let m = self.macros.get(name).cloned().ok_or_else(|| {
                        CamelError::new("InvalidReferenceException", format!("FreeMarker template error: The following has evaluated to null or missing: ==> {} (macro or directive)", name))
                    })?;
                    if m.is_function {
                        return Err(terr(format!("{} is a function, not a macro; call it as {}(...)", name, name)));
                    }
                    let mut locals = ObjectMap::new();
                    let mut extra = ObjectMap::new();
                    for (k, e) in positional.iter().enumerate() {
                        let v = self.eval_req(e)?;
                        match m.params.get(k) {
                            Some((p, _)) => {
                                locals.insert(p.clone(), v);
                            }
                            None if m.catch_all.is_some() => {
                                extra.insert(k.to_string(), v);
                            }
                            None => return Err(terr(format!("Macro \"{}\" only accepts {} parameters", name, m.params.len()))),
                        }
                    }
                    for (p, e) in named {
                        let v = self.eval_req(e)?;
                        if m.params.iter().any(|(q, _)| q == p) {
                            locals.insert(p.clone(), v);
                        } else if m.catch_all.is_some() {
                            extra.insert(p.clone(), v);
                        } else {
                            return Err(terr(format!("Macro \"{}\" has no parameter with name \"{}\"", name, p)));
                        }
                    }
                    for (p, d) in &m.params {
                        if !locals.contains_key(p) {
                            match d {
                                Some(d) => {
                                    let v = self.eval_req(d)?;
                                    locals.insert(p.clone(), v);
                                }
                                None => return Err(terr(format!("When calling macro \"{}\", required parameter \"{}\" was not specified", name, p))),
                            }
                        }
                    }
                    if let Some(c) = &m.catch_all {
                        locals.insert(c.clone(), Value::Object(extra));
                    }
                    self.depth += 1;
                    if self.depth > 200 {
                        return Err(terr("macro recursion too deep"));
                    }
                    let caller_depth = self.frames.len();
                    self.frames.push(Frame { locals, nested: body.clone().map(|b| (b, loop_vars.clone(), caller_depth)) });
                    let r = self.render(&m.body.clone(), out);
                    self.frames.pop();
                    self.depth -= 1;
                    r?;
                }
                Node::Nested(args) => {
                    let Some(frame) = self.frames.last() else { continue };
                    let Some((body, loop_vars, caller_depth)) = frame.nested.clone() else { continue };
                    let mut vars = ObjectMap::new();
                    for (k, a) in args.iter().enumerate() {
                        let v = self.eval_req(a)?;
                        if let Some(lv) = loop_vars.get(k) {
                            vars.insert(lv.clone(), v);
                        }
                    }
                    // the nested content sees the caller's locals
                    let hidden: Vec<Frame> = self.frames.drain(caller_depth..).collect();
                    self.loops.push((LoopInfo { var: String::new(), index: 0, len: None }, vars));
                    let r = self.render(&body, out);
                    self.loops.pop();
                    self.frames.extend(hidden);
                    r?;
                }
                Node::Include(e) => {
                    let name = self.eval_req(e)?.to_string();
                    let text = (self.partials)(&name).ok_or_else(|| CamelError::new("TemplateNotFoundException", format!("FreeMarker: Template not found for name \"{}\"", name)))?;
                    self.depth += 1;
                    if self.depth > 50 {
                        return Err(terr("#include nested too deeply"));
                    }
                    let (nodes, macros) = compile(&text)?;
                    for (k, m) in macros {
                        self.macros.insert(k, m);
                    }
                    let r = self.render(&nodes, out);
                    self.depth -= 1;
                    r?;
                }
                Node::Import(e, ns) => {
                    let name = self.eval_req(e)?.to_string();
                    let text = (self.partials)(&name).ok_or_else(|| CamelError::new("TemplateNotFoundException", format!("FreeMarker: Template not found for name \"{}\"", name)))?;
                    let (nodes, macros) = compile(&text)?;
                    let saved = std::mem::take(&mut self.globals);
                    let mut sink = String::new();
                    for (k, m) in &macros {
                        self.macros.insert(k.clone(), m.clone());
                    }
                    self.render(&nodes, &mut sink)?;
                    let ns_vars = std::mem::replace(&mut self.globals, saved);
                    for (k, m) in macros {
                        self.macros.insert(format!("{}.{}", ns, k), m);
                    }
                    self.globals.insert(ns.clone(), Value::Object(ns_vars));
                }
                Node::Switch(subject, cases) => {
                    let v = self.eval_req(subject)?;
                    let mut matched = false;
                    'cases: for pass in 0..2 {
                        for (c, body) in cases {
                            if !matched {
                                let hit = match c {
                                    Some(c) if pass == 0 => {
                                        let cv = self.eval_req(c)?;
                                        cmp_values(&v, &cv, "==").unwrap_or(false)
                                    }
                                    None if pass == 1 => true,
                                    _ => false,
                                };
                                if !hit {
                                    continue;
                                }
                                matched = true;
                            }
                            match self.render(body, out)? {
                                Flow::Normal => {}
                                Flow::Break => break 'cases,
                                f => return Ok(f),
                            }
                        }
                        if matched {
                            break;
                        }
                    }
                }
                Node::Attempt(body, rec) => {
                    let mut buf = String::new();
                    match self.render(body, &mut buf) {
                        Ok(Flow::Normal) => out.push_str(&buf),
                        Ok(f) => {
                            out.push_str(&buf);
                            return Ok(f);
                        }
                        Err(_) => match self.render(rec, out)? {
                            Flow::Normal => {}
                            f => return Ok(f),
                        },
                    }
                }
                Node::Compress(body) => {
                    let mut buf = String::new();
                    let f = self.render(body, &mut buf)?;
                    out.push_str(&compress(&buf));
                    if !matches!(f, Flow::Normal) {
                        return Ok(f);
                    }
                }
                Node::Escape(var, e, body) => {
                    self.escapes.push(Some((var.clone(), e.clone())));
                    let r = self.render(body, out);
                    self.escapes.pop();
                    match r? {
                        Flow::Normal => {}
                        f => return Ok(f),
                    }
                }
                Node::NoEscape(body) => {
                    self.escapes.push(None);
                    let r = self.render(body, out);
                    self.escapes.pop();
                    match r? {
                        Flow::Normal => {}
                        f => return Ok(f),
                    }
                }
                Node::OutputFormat(f, body) => {
                    self.output_format.push(f.clone().filter(|f| f != "plainText" && f != "RTF"));
                    let r = self.render(body, out);
                    self.output_format.pop();
                    match r? {
                        Flow::Normal => {}
                        fl => return Ok(fl),
                    }
                }
                Node::Setting(name, e) => {
                    let v = self.eval_req(e)?.to_string();
                    match name.as_str() {
                        "number_format" | "numberFormat" => self.number_format = v,
                        "boolean_format" | "booleanFormat" => {
                            self.boolean_format = if v == "c" {
                                Some(("true".into(), "false".into()))
                            } else {
                                v.split_once(',').map(|(t, f)| (t.to_string(), f.to_string()))
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(Flow::Normal)
    }

    fn iterate(&mut self, items: Vec<(Option<String>, Value)>, vars: &[String], body: &[Node], out: &mut String) -> Result<Flow> {
        let n = items.len();
        for (i, (key, item)) in items.into_iter().enumerate() {
            let mut scope = ObjectMap::new();
            let main_var = vars.first().cloned().unwrap_or_default();
            match (key, vars.len()) {
                (Some(k), 2) => {
                    scope.insert(vars[0].clone(), Value::String(k));
                    scope.insert(vars[1].clone(), item);
                }
                (Some(k), _) => {
                    scope.insert(main_var.clone(), Value::String(k));
                }
                (None, _) => {
                    scope.insert(main_var.clone(), item);
                }
            }
            self.loops.push((LoopInfo { var: main_var, index: i, len: Some(n) }, scope));
            let f = self.render(body, out);
            self.loops.pop();
            match f? {
                Flow::Normal | Flow::Continue => {}
                Flow::Break => break,
                Flow::Return(v) => return Ok(Flow::Return(v)),
            }
        }
        Ok(Flow::Normal)
    }
}

/// The FreeMarker engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct FreemarkerEngine;

impl TemplateEngine for FreemarkerEngine {
    fn render(&self, template: &str, context: &Value, partials: PartialLoader<'_>) -> Result<String> {
        let (nodes, macros) = compile(template)?;
        let model = match context {
            Value::Object(o) => o.clone(),
            _ => ObjectMap::new(),
        };
        let mut ctx = Ctx {
            model,
            globals: ObjectMap::new(),
            frames: Vec::new(),
            loops: Vec::new(),
            macros: macros.into_iter().collect(),
            partials,
            number_format: "number".into(),
            boolean_format: None,
            escapes: Vec::new(),
            output_format: Vec::new(),
            depth: 0,
            pending_items: None,
        };
        let mut out = String::new();
        ctx.render(&nodes, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Value {
        crate::json::parse(r#"{"body":"B","headers":{"name":"N","list":["a","b","c"],"n":1234.5678,"i":3,"flag":true,"m":{"k":"v","z":"w"},"people":[{"name":"Zed","age":30},{"name":"amy","age":25}]},"exchange":{"properties":{"p":1}}}"#).unwrap()
    }

    fn r(t: &str) -> String {
        FreemarkerEngine.render(t, &data(), &|n| if n == "inc.ftl" { Some("[${body}]<#macro hi>hi!</#macro>".into()) } else { None }).unwrap_or_else(|e| panic!("{}: {}", t, e))
    }

    fn e(t: &str) -> String {
        FreemarkerEngine.render(t, &data(), &|_| None).unwrap_err().message
    }

    #[test]
    fn interpolation_and_builtins() {
        assert_eq!(r("Hi ${headers.name} ${body} ${missing!\"dflt\"}${missing!} <#-- c -->."), "Hi N B dflt .");
        assert_eq!(r("${headers.n} ${headers.n?c} ${headers.i} ${headers.n?string(\"0.00\")} ${1234567}"), "1,234.568 1234.5678 3 1234.57 1,234,567");
        assert_eq!(r("${headers.name?lower_case}${body?length} ${\"hello world\"?cap_first} ${\"hello world\"?capitalize} ${headers.flag?c} ${headers.flag?string(\"y\",\"n\")} ${headers.flag?then(1,2)}"), "n1 Hello world Hello World true y 1");
        assert_eq!(r("${headers.list?size} ${headers.list?join(\", \")} ${headers.list?first}${headers.list?last} ${headers.list?reverse?join(\"\")} ${headers.list[1]}"), "3 a, b, c ac cba b");
        assert_eq!(r("${headers.m?keys?join(\",\")} ${headers.m.k} ${headers.m[\"z\"]} ${(headers.missing.deep)!\"x\"} ${headers.name???c} ${nope???c}"), "k,z v w x true false");
        assert_eq!(r("${headers.people?sort_by(\"name\")?map(p -> p.name)?join(\",\")} ${headers.people?filter(p -> p.age > 26)?size}"), "amy,Zed 1");
        assert_eq!(r("${\"a,b\"?split(\",\")?size} ${\"<x>\"?html} ${\"a b\"?url} ${\"abc\"[1..]} ${\"abc\"[0..<2]} ${\"x\"?left_pad(3, \".\")}"), "2 &lt;x&gt; a%20b bc ab ..x");
        assert_eq!(r("${1 + 2 * 3} ${10 / 4} ${7 % 3} ${\"a\" + 1} ${([1,2] + [3])?size}"), "7 2.5 1 a1 3");
        assert_eq!(r("${\"Hi ${headers.name}!\"} ${r\"${raw}\"} ${headers.i?string.computer} ${3?lower_abc}${28?upper_abc}"), "Hi N! ${raw} 3 cAB");
        assert_eq!(r("${headers.i?switch(1, \"one\", 3, \"three\", \"other\")} ${(headers.i > 2)?c} ${exchange.properties.p}"), "three true 1");
    }

    #[test]
    fn directives() {
        assert_eq!(r("<#if headers.i gt 2>big<#elseif headers.i == 2>two<#else>small</#if>"), "big");
        assert_eq!(r("<#list headers.list as x>${x?counter}:${x}<#sep>, </#sep></#list>"), "1:a, 2:b, 3:c");
        assert_eq!(r("<#list headers.list as x>${x}<#if x?is_last>.</#if><#if x == \"b\"><#break></#if></#list>"), "ab");
        assert_eq!(r("<#list [] as x>${x}<#else>empty</#list>"), "empty");
        assert_eq!(r("<#list headers.list><ul><#items as x><li>${x}</li></#items></ul></#list>"), "<ul><li>a</li><li>b</li><li>c</li></ul>");
        assert_eq!(r("<#list headers.m as k, v>${k}=${v};</#list><#list 1..3 as i>${i}</#list><#list 3..1 as i>${i}</#list>"), "k=v;z=w;123321");
        assert_eq!(r("<#assign x = 1 y = \"s\"><#assign x += 2>${x}${y}<#assign cap>c${x}</#assign>${cap}<#assign x++>${x}"), "3sc34");
        assert_eq!(r("<#macro greet who p=\"!\">Hi ${who}${p}<#nested/></#macro><@greet who=\"A\"/> <@greet \"B\" \".\">[n]</@greet>"), "Hi A! Hi B.[n]");
        assert_eq!(r("<#macro rep n><#list 1..n as i><#nested i/></#list></#macro><@rep n=3; c>${c}</@rep>"), "123");
        assert_eq!(r("<#function twice x><#return x * 2></#function>${twice(21)}"), "42");
        assert_eq!(r("<#switch headers.i><#case 1>one<#break><#case 3>three<#case 4>four<#break><#default>d</#switch>"), "threefour");
        assert_eq!(r("<#attempt>${nope}<#recover>recovered</#attempt>"), "recovered");
        assert_eq!(r("<#compress>  a   b\n\n   c  </#compress>"), "a b\nc");
        assert_eq!(r("<#noparse>${x}<#if></#noparse>"), "${x}<#if>");
        assert_eq!(r("<#include \"inc.ftl\"><@hi/>"), "[B]hi!");
        assert_eq!(r("<#import \"inc.ftl\" as lib><@lib.hi/>"), "hi!");
        assert_eq!(r("<#escape x as x?html>${\"<b>\"}<#noescape>${\"<i>\"}</#noescape></#escape>"), "&lt;b&gt;<i>");
        assert_eq!(r("<#outputformat \"HTML\">${\"&\"}</#outputformat><#setting number_format=\"0.0\">${2} <#setting boolean_format=\"yes,no\">${headers.flag}"), "&amp;2.0 yes");
        assert_eq!(r("[#ftl][#if true]sq[/#if] ${body}"), "sq B");
    }

    #[test]
    fn whitespace_stripping() {
        assert_eq!(r("<ul>\n  <#list headers.list as x>\n  <li>${x}</li>\n  </#list>\n</ul>\n"), "<ul>\n  <li>a</li>\n  <li>b</li>\n  <li>c</li>\n</ul>\n");
        assert_eq!(r("a\n  <#assign z = 1>\nb ${z}  <#t>\nc"), "a\nb 1c");
    }

    #[test]
    fn errors() {
        assert!(e("${missing}").contains("evaluated to null or missing"));
        assert!(e("<#if x>y</#if>").contains("evaluated to null or missing"));
        assert!(e("<#if body>y</#if>").contains("Expected a boolean"));
        assert!(e("${headers.flag}").contains("Can't convert boolean to string"));
        assert!(e("${headers.list}").contains("Expected a string"));
        assert!(e("<#if true>").contains("without"));
        assert!(e("<#bogus>").contains("Unknown directive"));
        assert!(e("<@nomacro/>").contains("missing"));
        assert!(e("${body?nosuch}").contains("?nosuch"));
    }
}
