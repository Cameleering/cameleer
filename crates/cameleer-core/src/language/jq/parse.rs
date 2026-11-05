//! jq lexer, AST and parser (jq 1.7 grammar) plus the compile-time name resolver.

use crate::error::{CamelError, Result};
use crate::value::Value;

/// A jq syntax error.
pub(crate) fn syntax(q: &str, line: usize, m: &str) -> CamelError {
    CamelError::new(crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid jq expression '{}': {} (line {})", q, m, line))
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StrTok {
    Lit(String),
    Interp(Vec<Tok>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokKind {
    Dot,
    DotDot,
    Field(String),
    Ident(String),
    Var(String),
    Format(String),
    Num(Value),
    Str(Vec<StrTok>),
    P(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tok {
    pub k: TokKind,
    pub line: usize,
}

const PUNCT: &[&str] = &[
    "?//=", "?//", "//=", "|=", "+=", "-=", "*=", "/=", "%=", "==", "!=", "<=", ">=", "//", "|", ",", "+", "-", "*", "/", "%", "=", "<", ">", "?", "(", ")", "[", "]", "{", "}", ":", ";",
];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Lexer<'q> {
    q: &'q str,
    c: Vec<char>,
    i: usize,
    line: usize,
}

impl<'q> Lexer<'q> {
    fn err(&self, m: &str) -> CamelError {
        syntax(self.q, self.line, m)
    }

    fn ident(&mut self) -> String {
        let st = self.i;
        while self.i < self.c.len() && (is_ident_char(self.c[self.i]) || (self.c[self.i] == ':' && self.c.get(self.i + 1) == Some(&':'))) {
            if self.c[self.i] == ':' {
                self.i += 1;
            }
            self.i += 1;
        }
        self.c[st..self.i].iter().collect()
    }

    /// Lex tokens until end of input (`nested == false`) or until the `)` closing a string
    /// interpolation (`nested == true`).
    fn lex(&mut self, nested: bool) -> Result<Vec<Tok>> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        loop {
            if self.i >= self.c.len() {
                if nested {
                    return Err(self.err("unterminated string interpolation"));
                }
                return Ok(out);
            }
            let ch = self.c[self.i];
            if ch == '\n' {
                self.line += 1;
                self.i += 1;
                continue;
            }
            if ch.is_whitespace() {
                self.i += 1;
                continue;
            }
            if ch == '#' {
                while self.i < self.c.len() && self.c[self.i] != '\n' {
                    self.i += 1;
                }
                continue;
            }
            let line = self.line;
            let tok = |k| Tok { k, line };
            match ch {
                '.' => {
                    let n = self.c.get(self.i + 1).copied();
                    if n == Some('.') {
                        self.i += 2;
                        out.push(tok(TokKind::DotDot));
                    } else if n.map(is_ident_start).unwrap_or(false) {
                        self.i += 1;
                        let st = self.i;
                        while self.i < self.c.len() && is_ident_char(self.c[self.i]) {
                            self.i += 1;
                        }
                        out.push(tok(TokKind::Field(self.c[st..self.i].iter().collect())));
                    } else if n.map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        out.push(tok(self.number()?));
                    } else {
                        self.i += 1;
                        out.push(tok(TokKind::Dot));
                    }
                }
                '$' => {
                    self.i += 1;
                    if self.i < self.c.len() && is_ident_start(self.c[self.i]) {
                        let name = self.ident();
                        out.push(tok(TokKind::Var(name)));
                    } else {
                        return Err(self.err("expected a variable name after '$'"));
                    }
                }
                '@' => {
                    self.i += 1;
                    let name = self.ident();
                    if name.is_empty() {
                        return Err(self.err("expected a format name after '@'"));
                    }
                    out.push(tok(TokKind::Format(name)));
                }
                '"' => {
                    self.i += 1;
                    let parts = self.string()?;
                    out.push(tok(TokKind::Str(parts)));
                }
                c if c.is_ascii_digit() => out.push(tok(self.number()?)),
                c if is_ident_start(c) => {
                    let name = self.ident();
                    out.push(tok(TokKind::Ident(name)));
                }
                _ => {
                    let rest: String = self.c[self.i..(self.i + 4).min(self.c.len())].iter().collect();
                    let p = PUNCT.iter().find(|p| rest.starts_with(**p)).ok_or_else(|| self.err(&format!("unexpected character '{}'", ch)))?;
                    if nested {
                        if *p == "(" {
                            depth += 1;
                        } else if *p == ")" {
                            if depth == 0 {
                                self.i += 1;
                                return Ok(out);
                            }
                            depth -= 1;
                        }
                    }
                    self.i += p.chars().count();
                    out.push(tok(TokKind::P(p)));
                }
            }
        }
    }

    fn number(&mut self) -> Result<TokKind> {
        let st = self.i;
        let mut is_float = false;
        while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.i < self.c.len() && self.c[self.i] == '.' && self.c.get(self.i + 1).map(|c| c.is_ascii_digit()).unwrap_or(true) {
            if self.c.get(self.i + 1) != Some(&'.') {
                is_float = true;
                self.i += 1;
                while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            }
        }
        if self.i < self.c.len() && (self.c[self.i] == 'e' || self.c[self.i] == 'E') {
            let save = self.i;
            self.i += 1;
            if self.i < self.c.len() && (self.c[self.i] == '+' || self.c[self.i] == '-') {
                self.i += 1;
            }
            if self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                is_float = true;
                while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        let t: String = self.c[st..self.i].iter().collect();
        if !is_float {
            if let Ok(n) = t.parse::<i64>() {
                return Ok(TokKind::Num(Value::Int(n)));
            }
        }
        let f: f64 = t.parse().map_err(|_| self.err("invalid number literal"))?;
        Ok(TokKind::Num(super::eval::num(f)))
    }

    fn string(&mut self) -> Result<Vec<StrTok>> {
        let mut parts = Vec::new();
        let mut s = String::new();
        loop {
            if self.i >= self.c.len() {
                return Err(self.err("unterminated string"));
            }
            let ch = self.c[self.i];
            self.i += 1;
            match ch {
                '"' => break,
                '\n' => {
                    self.line += 1;
                    s.push('\n');
                }
                '\\' => {
                    let e = *self.c.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 1;
                    match e {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'b' => s.push('\u{8}'),
                        'f' => s.push('\u{c}'),
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.c.get(self.i) == Some(&'\\') && self.c.get(self.i + 1) == Some(&'u') {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        '(' => {
                            if !s.is_empty() {
                                parts.push(StrTok::Lit(std::mem::take(&mut s)));
                            }
                            let toks = self.lex(true)?;
                            parts.push(StrTok::Interp(toks));
                        }
                        other => return Err(self.err(&format!("invalid escape '\\{}'", other))),
                    }
                }
                c => s.push(c),
            }
        }
        if !s.is_empty() || parts.is_empty() {
            parts.push(StrTok::Lit(s));
        }
        Ok(parts)
    }

    fn hex4(&mut self) -> Result<u32> {
        let h: String = self.c.get(self.i..self.i + 4).ok_or_else(|| self.err("invalid \\u escape"))?.iter().collect();
        self.i += 4;
        u32::from_str_radix(&h, 16).map_err(|_| self.err("invalid \\u escape"))
    }
}

/// Tokenize a jq program.
pub(crate) fn lex(q: &str) -> Result<Vec<Tok>> {
    Lexer { q, c: q.chars().collect(), i: 0, line: 1 }.lex(false)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AssignOp {
    Set,
    Update,
    Arith(BinOp),
    Alt,
}

#[derive(Debug, Clone)]
pub(crate) enum StrPart {
    Lit(String),
    Interp(Ast),
}

#[derive(Debug, Clone)]
pub(crate) enum ObjKey {
    /// A literal key (identifier, keyword or plain string).
    Lit(String),
    /// `$name` (key `name`, value `$name`).
    Var(String),
    /// `$__loc__`.
    Loc(usize),
    /// An interpolated string key.
    Str(Option<String>, Vec<StrPart>),
    /// `(expr)`.
    Expr(Ast),
}

#[derive(Debug, Clone)]
pub(crate) enum Pattern {
    Var(String),
    Array(Vec<Pattern>),
    Object(Vec<(ObjKey, Option<Pattern>)>),
}

impl Pattern {
    pub(crate) fn vars(&self, out: &mut Vec<String>) {
        match self {
            Pattern::Var(v) => {
                if !out.contains(v) {
                    out.push(v.clone())
                }
            }
            Pattern::Array(items) => items.iter().for_each(|p| p.vars(out)),
            Pattern::Object(entries) => {
                for (k, p) in entries {
                    if let ObjKey::Var(v) = k {
                        if !out.contains(v) {
                            out.push(v.clone())
                        }
                    }
                    if let Some(p) = p {
                        p.vars(out)
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FuncDef {
    pub name: String,
    /// Parameter names; `$x` parameters keep the `$` prefix.
    pub params: Vec<String>,
    pub body: Ast,
}

#[derive(Debug, Clone)]
pub(crate) enum Ast {
    Identity,
    RecurseAll,
    Lit(Value),
    Str(Option<String>, Vec<StrPart>),
    Format(String),
    Index(Box<Ast>, Box<Ast>),
    Slice(Box<Ast>, Option<Box<Ast>>, Option<Box<Ast>>),
    Iterate(Box<Ast>),
    Try(Box<Ast>, Option<Box<Ast>>),
    Array(Option<Box<Ast>>),
    Object(Vec<(ObjKey, Option<Ast>)>),
    Neg(Box<Ast>),
    Pipe(Box<Ast>, Box<Ast>),
    Comma(Box<Ast>, Box<Ast>),
    Bin(BinOp, Box<Ast>, Box<Ast>),
    And(Box<Ast>, Box<Ast>),
    Or(Box<Ast>, Box<Ast>),
    Alt(Box<Ast>, Box<Ast>),
    Assign(AssignOp, Box<Ast>, Box<Ast>),
    If(Vec<(Ast, Ast)>, Option<Box<Ast>>),
    Reduce(Box<Ast>, Pattern, Box<Ast>, Box<Ast>),
    Foreach(Box<Ast>, Pattern, Box<Ast>, Box<Ast>, Option<Box<Ast>>),
    Bind(Box<Ast>, Vec<Pattern>, Box<Ast>),
    Label(String, Box<Ast>),
    Break(String),
    Var(String),
    Loc(usize),
    Call(String, Vec<Ast>),
    Defs(Vec<FuncDef>, Box<Ast>),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub(crate) struct Parser<'q> {
    pub q: &'q str,
    pub t: Vec<Tok>,
    pub i: usize,
}

const KEYWORDS: &[&str] = &["def", "if", "then", "elif", "else", "end", "as", "reduce", "foreach", "try", "catch", "label", "import", "include", "and", "or", "__loc__"];

impl<'q> Parser<'q> {
    pub(crate) fn new(q: &'q str, t: Vec<Tok>) -> Self {
        Parser { q, t, i: 0 }
    }

    fn line(&self) -> usize {
        self.t.get(self.i).or_else(|| self.t.last()).map(|t| t.line).unwrap_or(1)
    }

    fn err(&self, m: &str) -> CamelError {
        syntax(self.q, self.line(), m)
    }

    fn peek(&self) -> Option<&TokKind> {
        self.t.get(self.i).map(|t| &t.k)
    }

    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), Some(TokKind::P(x)) if *x == p)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Some(TokKind::Ident(x)) if x == k)
    }

    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_p(&mut self, p: &str) -> Result<()> {
        if self.eat_p(p) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}' but found {}", p, self.describe())))
        }
    }

    fn expect_kw(&mut self, k: &str) -> Result<()> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}' but found {}", k, self.describe())))
        }
    }

    fn describe(&self) -> String {
        match self.peek() {
            None => "end of input".into(),
            Some(TokKind::P(p)) => format!("'{}'", p),
            Some(TokKind::Ident(s)) => format!("'{}'", s),
            Some(TokKind::Field(s)) => format!("'.{}'", s),
            Some(TokKind::Var(s)) => format!("'${}'", s),
            Some(other) => format!("{:?}", other),
        }
    }

    /// Parse the whole program.
    pub(crate) fn program(&mut self) -> Result<Ast> {
        if self.t.is_empty() {
            return Ok(Ast::Identity);
        }
        // A program may consist only of definitions.
        let ast = self.pipe()?;
        if self.i < self.t.len() {
            return Err(self.err(&format!("unexpected {}", self.describe())));
        }
        Ok(ast)
    }

    pub(crate) fn pipe(&mut self) -> Result<Ast> {
        if self.is_kw("def") {
            let mut defs = Vec::new();
            while self.is_kw("def") {
                defs.push(self.funcdef()?);
            }
            let body = if self.i >= self.t.len() || self.is_p(")") || self.is_p("]") || self.is_p("}") || self.is_p(";") {
                Ast::Identity
            } else {
                self.pipe()?
            };
            return Ok(Ast::Defs(defs, Box::new(body)));
        }
        let lhs = self.binary(2)?;
        if self.eat_p("|") {
            let rhs = self.pipe()?;
            return Ok(Ast::Pipe(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn funcdef(&mut self) -> Result<FuncDef> {
        self.expect_kw("def")?;
        let name = match self.peek() {
            Some(TokKind::Ident(n)) if !KEYWORDS.contains(&n.as_str()) || n == "not" => n.clone(),
            _ => return Err(self.err("expected a function name after 'def'")),
        };
        self.i += 1;
        let mut params = Vec::new();
        if self.eat_p("(") {
            loop {
                match self.peek().cloned() {
                    Some(TokKind::Ident(n)) => params.push(n),
                    Some(TokKind::Var(n)) => params.push(format!("${}", n)),
                    _ => return Err(self.err("expected a parameter name")),
                }
                self.i += 1;
                if self.eat_p(";") {
                    continue;
                }
                self.expect_p(")")?;
                break;
            }
        }
        self.expect_p(":")?;
        let body = self.pipe()?;
        self.expect_p(";")?;
        Ok(FuncDef { name, params, body })
    }

    /// Precedence levels: 2 `,` · 3 `//` (right) · 4 assignments (nonassoc) · 5 `or` ·
    /// 6 `and` · 7 comparisons (nonassoc) · 8 `+ -` · 9 `* / %`.
    fn binary(&mut self, level: u8) -> Result<Ast> {
        if level > 9 {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        loop {
            let op = match self.peek() {
                Some(TokKind::P(p)) => *p,
                Some(TokKind::Ident(k)) if k == "and" || k == "or" => {
                    if k == "and" {
                        "and"
                    } else {
                        "or"
                    }
                }
                _ => return Ok(lhs),
            };
            let op_level = match op {
                "," => 2,
                "//" => 3,
                "=" | "|=" | "+=" | "-=" | "*=" | "/=" | "%=" | "//=" => 4,
                "or" => 5,
                "and" => 6,
                "==" | "!=" | "<" | "<=" | ">" | ">=" => 7,
                "+" | "-" => 8,
                "*" | "/" | "%" => 9,
                _ => return Ok(lhs),
            };
            if op_level != level {
                return Ok(lhs);
            }
            self.i += 1;
            let rhs = match level {
                3 => self.binary(3)?, // right associative
                4 | 7 => self.binary(level + 1)?,
                _ => self.binary(level + 1)?,
            };
            lhs = match op {
                "," => Ast::Comma(Box::new(lhs), Box::new(rhs)),
                "//" => Ast::Alt(Box::new(lhs), Box::new(rhs)),
                "or" => Ast::Or(Box::new(lhs), Box::new(rhs)),
                "and" => Ast::And(Box::new(lhs), Box::new(rhs)),
                "=" => Ast::Assign(AssignOp::Set, Box::new(lhs), Box::new(rhs)),
                "|=" => Ast::Assign(AssignOp::Update, Box::new(lhs), Box::new(rhs)),
                "//=" => Ast::Assign(AssignOp::Alt, Box::new(lhs), Box::new(rhs)),
                "+=" => Ast::Assign(AssignOp::Arith(BinOp::Add), Box::new(lhs), Box::new(rhs)),
                "-=" => Ast::Assign(AssignOp::Arith(BinOp::Sub), Box::new(lhs), Box::new(rhs)),
                "*=" => Ast::Assign(AssignOp::Arith(BinOp::Mul), Box::new(lhs), Box::new(rhs)),
                "/=" => Ast::Assign(AssignOp::Arith(BinOp::Div), Box::new(lhs), Box::new(rhs)),
                "%=" => Ast::Assign(AssignOp::Arith(BinOp::Mod), Box::new(lhs), Box::new(rhs)),
                o => {
                    let b = match o {
                        "+" => BinOp::Add,
                        "-" => BinOp::Sub,
                        "*" => BinOp::Mul,
                        "/" => BinOp::Div,
                        "%" => BinOp::Mod,
                        "==" => BinOp::Eq,
                        "!=" => BinOp::Ne,
                        "<" => BinOp::Lt,
                        "<=" => BinOp::Le,
                        ">" => BinOp::Gt,
                        _ => BinOp::Ge,
                    };
                    Ast::Bin(b, Box::new(lhs), Box::new(rhs))
                }
            };
            if level == 4 || level == 7 {
                // non-associative
                if let Some(TokKind::P(p)) = self.peek() {
                    let same = match level {
                        4 => matches!(*p, "=" | "|=" | "+=" | "-=" | "*=" | "/=" | "%=" | "//="),
                        _ => matches!(*p, "==" | "!=" | "<" | "<=" | ">" | ">="),
                    };
                    if same {
                        return Err(self.err(&format!("operator '{}' is not associative", p)));
                    }
                }
            }
        }
    }

    fn unary(&mut self) -> Result<Ast> {
        if self.eat_p("-") {
            let e = self.unary()?;
            return Ok(match e {
                Ast::Lit(Value::Int(n)) => Ast::Lit(Value::Int(-n)),
                Ast::Lit(Value::Float(f)) => Ast::Lit(Value::Float(-f)),
                other => Ast::Neg(Box::new(other)),
            });
        }
        self.postfix(true)
    }

    /// A term with suffixes (`.a`, `[..]`, `?`); with `allow_as`, `term as $x | body`.
    fn postfix(&mut self, allow_as: bool) -> Result<Ast> {
        let mut t = self.primary()?;
        loop {
            match self.peek().cloned() {
                Some(TokKind::Field(name)) => {
                    self.i += 1;
                    t = Ast::Index(Box::new(t), Box::new(Ast::Lit(Value::String(name))));
                }
                Some(TokKind::Dot) if matches!(self.t.get(self.i + 1).map(|t| &t.k), Some(TokKind::Str(_)) | Some(TokKind::P("["))) => {
                    self.i += 1;
                    if let Some(TokKind::Str(parts)) = self.peek().cloned() {
                        self.i += 1;
                        let key = self.str_ast(None, parts)?;
                        t = Ast::Index(Box::new(t), Box::new(key));
                    }
                }
                Some(TokKind::P("[")) => {
                    self.i += 1;
                    t = self.bracket_suffix(t)?;
                }
                Some(TokKind::P("?")) => {
                    self.i += 1;
                    t = Ast::Try(Box::new(t), None);
                }
                _ => break,
            }
        }
        if allow_as && self.is_kw("as") {
            self.i += 1;
            let mut pats = vec![self.pattern()?];
            while self.eat_p("?//") {
                pats.push(self.pattern()?);
            }
            self.expect_p("|")?;
            let body = self.pipe()?;
            return Ok(Ast::Bind(Box::new(t), pats, Box::new(body)));
        }
        Ok(t)
    }

    fn bracket_suffix(&mut self, t: Ast) -> Result<Ast> {
        if self.eat_p("]") {
            return Ok(Ast::Iterate(Box::new(t)));
        }
        if self.eat_p(":") {
            let hi = self.pipe()?;
            self.expect_p("]")?;
            return Ok(Ast::Slice(Box::new(t), None, Some(Box::new(hi))));
        }
        let idx = self.pipe()?;
        if self.eat_p(":") {
            let hi = if self.is_p("]") { None } else { Some(Box::new(self.pipe()?)) };
            self.expect_p("]")?;
            return Ok(Ast::Slice(Box::new(t), Some(Box::new(idx)), hi));
        }
        self.expect_p("]")?;
        Ok(Ast::Index(Box::new(t), Box::new(idx)))
    }

    fn str_ast(&mut self, fmt: Option<String>, parts: Vec<StrTok>) -> Result<Ast> {
        let parts = self.str_parts(parts)?;
        if fmt.is_none() && parts.len() == 1 {
            if let StrPart::Lit(s) = &parts[0] {
                return Ok(Ast::Lit(Value::String(s.clone())));
            }
        }
        Ok(Ast::Str(fmt, parts))
    }

    fn str_parts(&mut self, parts: Vec<StrTok>) -> Result<Vec<StrPart>> {
        let mut out = Vec::new();
        for p in parts {
            match p {
                StrTok::Lit(s) => out.push(StrPart::Lit(s)),
                StrTok::Interp(toks) => {
                    if toks.is_empty() {
                        return Err(self.err("empty string interpolation"));
                    }
                    let mut sub = Parser { q: self.q, t: toks, i: 0 };
                    let e = sub.pipe()?;
                    if sub.i < sub.t.len() {
                        return Err(sub.err(&format!("unexpected {}", sub.describe())));
                    }
                    out.push(StrPart::Interp(e));
                }
            }
        }
        Ok(out)
    }

    fn primary(&mut self) -> Result<Ast> {
        let line = self.line();
        let tok = self.t.get(self.i).cloned().ok_or_else(|| self.err("unexpected end of input"))?;
        self.i += 1;
        Ok(match tok.k {
            TokKind::Dot => {
                if let Some(TokKind::Str(parts)) = self.peek().cloned() {
                    self.i += 1;
                    let key = self.str_ast(None, parts)?;
                    Ast::Index(Box::new(Ast::Identity), Box::new(key))
                } else {
                    Ast::Identity
                }
            }
            TokKind::DotDot => Ast::RecurseAll,
            TokKind::Field(name) => Ast::Index(Box::new(Ast::Identity), Box::new(Ast::Lit(Value::String(name)))),
            TokKind::Num(v) => Ast::Lit(v),
            TokKind::Str(parts) => self.str_ast(None, parts)?,
            TokKind::Format(f) => {
                if let Some(TokKind::Str(parts)) = self.peek().cloned() {
                    self.i += 1;
                    self.str_ast(Some(f), parts)?
                } else {
                    Ast::Format(f)
                }
            }
            TokKind::Var(v) => {
                if v == "__loc__" {
                    Ast::Loc(line)
                } else {
                    Ast::Var(v)
                }
            }
            TokKind::P("(") => {
                let e = self.pipe()?;
                self.expect_p(")")?;
                e
            }
            TokKind::P("[") => {
                if self.eat_p("]") {
                    Ast::Array(None)
                } else {
                    let e = self.pipe()?;
                    self.expect_p("]")?;
                    Ast::Array(Some(Box::new(e)))
                }
            }
            TokKind::P("{") => self.object()?,
            TokKind::Ident(name) => match name.as_str() {
                "true" => Ast::Lit(Value::Bool(true)),
                "false" => Ast::Lit(Value::Bool(false)),
                "null" => Ast::Lit(Value::Null),
                "if" => self.if_rest()?,
                "try" => {
                    let body = self.postfix(false)?;
                    let catch = if self.eat_kw("catch") { Some(Box::new(self.postfix(false)?)) } else { None };
                    Ast::Try(Box::new(body), catch)
                }
                "reduce" => {
                    let src = self.postfix(false)?;
                    self.expect_kw("as")?;
                    let pat = self.pattern()?;
                    self.expect_p("(")?;
                    let init = self.pipe()?;
                    self.expect_p(";")?;
                    let upd = self.pipe()?;
                    self.expect_p(")")?;
                    Ast::Reduce(Box::new(src), pat, Box::new(init), Box::new(upd))
                }
                "foreach" => {
                    let src = self.postfix(false)?;
                    self.expect_kw("as")?;
                    let pat = self.pattern()?;
                    self.expect_p("(")?;
                    let init = self.pipe()?;
                    self.expect_p(";")?;
                    let upd = self.pipe()?;
                    let ext = if self.eat_p(";") { Some(Box::new(self.pipe()?)) } else { None };
                    self.expect_p(")")?;
                    Ast::Foreach(Box::new(src), pat, Box::new(init), Box::new(upd), ext)
                }
                "label" => {
                    let name = match self.peek().cloned() {
                        Some(TokKind::Var(v)) => v,
                        _ => return Err(self.err("expected '$name' after 'label'")),
                    };
                    self.i += 1;
                    self.expect_p("|")?;
                    let body = self.pipe()?;
                    Ast::Label(name, Box::new(body))
                }
                "break" => match self.peek().cloned() {
                    Some(TokKind::Var(v)) => {
                        self.i += 1;
                        Ast::Break(v)
                    }
                    _ => return Err(self.err("expected '$name' after 'break'")),
                },
                "def" => {
                    self.i -= 1;
                    let mut defs = Vec::new();
                    while self.is_kw("def") {
                        defs.push(self.funcdef()?);
                    }
                    let body = self.pipe()?;
                    Ast::Defs(defs, Box::new(body))
                }
                "import" | "include" => return Err(self.err("modules (import/include) are not supported")),
                kw if KEYWORDS.contains(&kw) => return Err(syntax(self.q, line, &format!("unexpected keyword '{}'", kw))),
                _ => {
                    let mut args = Vec::new();
                    if self.eat_p("(") {
                        loop {
                            args.push(self.pipe()?);
                            if self.eat_p(";") {
                                continue;
                            }
                            self.expect_p(")")?;
                            break;
                        }
                    }
                    Ast::Call(name, args)
                }
            },
            other => {
                self.i -= 1;
                return Err(self.err(&format!("unexpected {}", match other {
                    TokKind::P(p) => format!("'{}'", p),
                    o => format!("{:?}", o),
                })));
            }
        })
    }

    fn if_rest(&mut self) -> Result<Ast> {
        let mut branches = Vec::new();
        let c = self.pipe()?;
        self.expect_kw("then")?;
        let b = self.pipe()?;
        branches.push((c, b));
        let mut otherwise = None;
        loop {
            if self.eat_kw("elif") {
                let c = self.pipe()?;
                self.expect_kw("then")?;
                let b = self.pipe()?;
                branches.push((c, b));
            } else if self.eat_kw("else") {
                otherwise = Some(Box::new(self.pipe()?));
                self.expect_kw("end")?;
                break;
            } else {
                self.expect_kw("end")?;
                break;
            }
        }
        Ok(Ast::If(branches, otherwise))
    }

    /// Object value: `ExpD` in jq's grammar (terms joined by `|`), parsed leniently
    /// as alternative-level expressions.
    fn objval(&mut self) -> Result<Ast> {
        let lhs = self.binary(3)?;
        if self.eat_p("|") {
            let rhs = self.objval()?;
            return Ok(Ast::Pipe(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn object(&mut self) -> Result<Ast> {
        let mut entries = Vec::new();
        if self.eat_p("}") {
            return Ok(Ast::Object(entries));
        }
        loop {
            let line = self.line();
            let tok = self.t.get(self.i).cloned().ok_or_else(|| self.err("unterminated object"))?;
            self.i += 1;
            let key = match tok.k {
                TokKind::Ident(n) => ObjKey::Lit(n),
                TokKind::Var(v) if v == "__loc__" => ObjKey::Loc(line),
                TokKind::Var(v) => ObjKey::Var(v),
                TokKind::Num(n) => return Err(self.err(&format!("object keys must be strings (found {})", n.to_json()))),
                TokKind::Str(parts) => {
                    let p = self.str_parts(parts)?;
                    match p.as_slice() {
                        [StrPart::Lit(s)] => ObjKey::Lit(s.clone()),
                        _ => ObjKey::Str(None, p),
                    }
                }
                TokKind::Format(f) => match self.peek().cloned() {
                    Some(TokKind::Str(parts)) => {
                        self.i += 1;
                        let p = self.str_parts(parts)?;
                        ObjKey::Str(Some(f), p)
                    }
                    _ => return Err(self.err("expected a string after the format in an object key")),
                },
                TokKind::P("(") => {
                    let e = self.pipe()?;
                    self.expect_p(")")?;
                    ObjKey::Expr(e)
                }
                _ => {
                    self.i -= 1;
                    return Err(self.err(&format!("unexpected {} in object construction", self.describe())));
                }
            };
            let value = if self.eat_p(":") {
                Some(self.objval()?)
            } else {
                if let ObjKey::Expr(_) = key {
                    return Err(self.err("expected ':' after a computed object key"));
                }
                None
            };
            entries.push((key, value));
            if self.eat_p(",") {
                if self.eat_p("}") {
                    break;
                }
                continue;
            }
            self.expect_p("}")?;
            break;
        }
        Ok(Ast::Object(entries))
    }

    fn pattern(&mut self) -> Result<Pattern> {
        match self.peek().cloned() {
            Some(TokKind::Var(v)) => {
                self.i += 1;
                Ok(Pattern::Var(v))
            }
            Some(TokKind::P("[")) => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat_p("]") {
                    return Err(self.err("empty array pattern"));
                }
                loop {
                    items.push(self.pattern()?);
                    if self.eat_p(",") {
                        continue;
                    }
                    self.expect_p("]")?;
                    break;
                }
                Ok(Pattern::Array(items))
            }
            Some(TokKind::P("{")) => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    let tok = self.t.get(self.i).cloned().ok_or_else(|| self.err("unterminated object pattern"))?;
                    self.i += 1;
                    let key = match tok.k {
                        TokKind::Var(v) => ObjKey::Var(v),
                        TokKind::Ident(n) => ObjKey::Lit(n),
                        TokKind::Str(parts) => {
                            let p = self.str_parts(parts)?;
                            match p.as_slice() {
                                [StrPart::Lit(s)] => ObjKey::Lit(s.clone()),
                                _ => ObjKey::Str(None, p),
                            }
                        }
                        TokKind::P("(") => {
                            let e = self.pipe()?;
                            self.expect_p(")")?;
                            ObjKey::Expr(e)
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.err("invalid object pattern"));
                        }
                    };
                    let pat = if self.eat_p(":") {
                        Some(self.pattern()?)
                    } else {
                        if !matches!(key, ObjKey::Var(_)) {
                            return Err(self.err("object pattern entries need a ':' pattern"));
                        }
                        None
                    };
                    entries.push((key, pat));
                    if self.eat_p(",") {
                        continue;
                    }
                    self.expect_p("}")?;
                    break;
                }
                Ok(Pattern::Object(entries))
            }
            _ => Err(self.err("expected a destructuring pattern ($name, [...] or {...})")),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolver: undefined functions, variables and labels are compile errors.
// ---------------------------------------------------------------------------

pub(crate) struct Scope<'s> {
    pub funcs: Vec<(String, usize)>,
    pub vars: Vec<String>,
    pub labels: Vec<String>,
    pub native: &'s dyn Fn(&str, usize) -> bool,
}

impl<'s> Scope<'s> {
    fn has_func(&self, name: &str, arity: usize) -> bool {
        self.funcs.iter().rev().any(|(n, a)| n == name && *a == arity) || (self.native)(name, arity)
    }
}

pub(crate) fn resolve(q: &str, ast: &Ast, sc: &mut Scope<'_>) -> Result<()> {
    let e = |m: String| syntax(q, 1, &m);
    match ast {
        Ast::Identity | Ast::RecurseAll | Ast::Lit(_) | Ast::Format(_) | Ast::Loc(_) => Ok(()),
        Ast::Str(_, parts) => {
            for p in parts {
                if let StrPart::Interp(a) = p {
                    resolve(q, a, sc)?;
                }
            }
            Ok(())
        }
        Ast::Index(a, b) | Ast::Pipe(a, b) | Ast::Comma(a, b) | Ast::Bin(_, a, b) | Ast::And(a, b) | Ast::Or(a, b) | Ast::Alt(a, b) | Ast::Assign(_, a, b) => {
            resolve(q, a, sc)?;
            resolve(q, b, sc)
        }
        Ast::Slice(a, b, c) => {
            resolve(q, a, sc)?;
            if let Some(b) = b {
                resolve(q, b, sc)?;
            }
            if let Some(c) = c {
                resolve(q, c, sc)?;
            }
            Ok(())
        }
        Ast::Iterate(a) | Ast::Neg(a) => resolve(q, a, sc),
        Ast::Try(a, c) => {
            resolve(q, a, sc)?;
            if let Some(c) = c {
                resolve(q, c, sc)?;
            }
            Ok(())
        }
        Ast::Array(a) => a.as_ref().map(|a| resolve(q, a, sc)).unwrap_or(Ok(())),
        Ast::Object(entries) => {
            for (k, v) in entries {
                match k {
                    ObjKey::Var(name) => {
                        if !sc.vars.contains(name) {
                            return Err(e(format!("${} is not defined", name)));
                        }
                    }
                    ObjKey::Str(_, parts) => {
                        for p in parts {
                            if let StrPart::Interp(a) = p {
                                resolve(q, a, sc)?;
                            }
                        }
                    }
                    ObjKey::Expr(a) => resolve(q, a, sc)?,
                    _ => {}
                }
                if let Some(v) = v {
                    resolve(q, v, sc)?;
                }
            }
            Ok(())
        }
        Ast::If(branches, otherwise) => {
            for (c, b) in branches {
                resolve(q, c, sc)?;
                resolve(q, b, sc)?;
            }
            otherwise.as_ref().map(|o| resolve(q, o, sc)).unwrap_or(Ok(()))
        }
        Ast::Reduce(src, pat, init, upd) => {
            resolve(q, src, sc)?;
            resolve(q, init, sc)?;
            resolve_pattern(q, pat, sc)?;
            let n = sc.vars.len();
            let mut vs = Vec::new();
            pat.vars(&mut vs);
            sc.vars.extend(vs);
            let r = resolve(q, upd, sc);
            sc.vars.truncate(n);
            r
        }
        Ast::Foreach(src, pat, init, upd, ext) => {
            resolve(q, src, sc)?;
            resolve(q, init, sc)?;
            resolve_pattern(q, pat, sc)?;
            let n = sc.vars.len();
            let mut vs = Vec::new();
            pat.vars(&mut vs);
            sc.vars.extend(vs);
            let mut r = resolve(q, upd, sc);
            if r.is_ok() {
                if let Some(x) = ext {
                    r = resolve(q, x, sc);
                }
            }
            sc.vars.truncate(n);
            r
        }
        Ast::Bind(src, pats, body) => {
            resolve(q, src, sc)?;
            let mut vs = Vec::new();
            for p in pats {
                resolve_pattern(q, p, sc)?;
                p.vars(&mut vs);
            }
            let n = sc.vars.len();
            sc.vars.extend(vs);
            let r = resolve(q, body, sc);
            sc.vars.truncate(n);
            r
        }
        Ast::Label(name, body) => {
            sc.labels.push(name.clone());
            let r = resolve(q, body, sc);
            sc.labels.pop();
            r
        }
        Ast::Break(name) => {
            if sc.labels.contains(name) {
                Ok(())
            } else {
                Err(e(format!("$*label-{} is not defined", name)))
            }
        }
        Ast::Var(name) => {
            if sc.vars.contains(name) || name == "ENV" || name == "__prog_args" {
                Ok(())
            } else {
                Err(e(format!("${} is not defined", name)))
            }
        }
        Ast::Call(name, args) => {
            if !sc.has_func(name, args.len()) {
                return Err(e(format!("{}/{} is not defined", name, args.len())));
            }
            for a in args {
                resolve(q, a, sc)?;
            }
            Ok(())
        }
        Ast::Defs(defs, body) => {
            let nf = sc.funcs.len();
            for d in defs {
                sc.funcs.push((d.name.clone(), d.params.len()));
                let nf2 = sc.funcs.len();
                let nv = sc.vars.len();
                for p in &d.params {
                    match p.strip_prefix('$') {
                        Some(v) => {
                            sc.vars.push(v.to_string());
                            sc.funcs.push((v.to_string(), 0));
                        }
                        None => sc.funcs.push((p.clone(), 0)),
                    }
                }
                let labels = std::mem::take(&mut sc.labels);
                let r = resolve(q, &d.body, sc);
                sc.labels = labels;
                sc.funcs.truncate(nf2);
                sc.vars.truncate(nv);
                r?;
            }
            let r = resolve(q, body, sc);
            sc.funcs.truncate(nf);
            r
        }
    }
}

fn resolve_pattern(q: &str, p: &Pattern, sc: &mut Scope<'_>) -> Result<()> {
    match p {
        Pattern::Var(_) => Ok(()),
        Pattern::Array(items) => {
            for i in items {
                resolve_pattern(q, i, sc)?;
            }
            Ok(())
        }
        Pattern::Object(entries) => {
            for (k, v) in entries {
                match k {
                    ObjKey::Expr(a) => resolve(q, a, sc)?,
                    ObjKey::Str(_, parts) => {
                        for part in parts {
                            if let StrPart::Interp(a) = part {
                                resolve(q, a, sc)?;
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(v) = v {
                    resolve_pattern(q, v, sc)?;
                }
            }
            Ok(())
        }
    }
}
